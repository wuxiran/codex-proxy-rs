//! 按账号和模型建立、验证并保留 WebSocket 候选。
//! 完整响应、模型声明和启用的答案检查通过后才发布，回池的待验证连接不能被业务领养。
//! 配置专用代理时，失败候选关闭后重新寻找路由，通过后业务复用同一条活连接。
//! 普通账号出口设置不改写，客户端当轮票和既有续接仍遵守原来的路由约束。

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

use futures::StreamExt as _;
use gateway_core::account::ProviderAccount;
use secrecy::ExposeSecret;
use serde_json::{Map, Value, json};
use tokio::{sync::Notify, time::Instant};
use turn_state::WarmPoolSettings;

use crate::credential::{CODEX_AUTHENTICATION_KIND_OAUTH, CodexCredentialRepository};
use crate::transport::profile::CodexWireProfileState;
use crate::transport::protocol::responses::CodexResponsesRequest;
use crate::transport::websocket::WARM_CONVERSATION_PREFIX;
use crate::transport::{
    CodexBackendClient, CodexBackendStreamingResponse, CodexRequestContext, CodexWebSocketPool,
    WarmConnectionApproval,
};

/// 探针/开连接的整次调用硬上限的下限保护。
const MIN_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const MODEL_ACTIVITY_WINDOW: Duration = Duration::from_secs(600);
const MAX_ACTIVE_MODELS: usize = 16;
/// 缺省探针模型（设置没配时）。
const DEFAULT_WARM_MODEL: &str = "gpt-6-astra";
/// 缺省探针题：糖果题，满血答 21。答案只读开头判定，不落明文。
const DEFAULT_PROBE_PROMPT: &str = concat!(
    "在一个黑色的袋子里放有三种口味的糖果，每种糖果有两种不同的形状（圆形和五角星形，",
    "不同的形状靠手感可以分辨）。现已知不同口味的糖和不同形状的数量统计如下表。",
    "参赛者需要在活动前决定摸出的糖果数目，那么，最少取出多少个糖果才能保证手中同时",
    "拥有不同形状的苹果味和桃子味的糖？（同时手中有圆形苹果味匹配五角星桃子味糖果，",
    "或者有圆形桃子味匹配五角星苹果味糖果都满足要求）\n\n",
    "形状 | 苹果味 | 桃子味 | 西瓜味\n圆形 | 7 | 9 | 8\n五角星形 | 7 | 6 | 4\n\n",
    "不许联网，自己计算。只回答最少取出的糖果总数，使用一个整数，不要解释。"
);

/// 一个账号一次保活循环的结果摘要；不含答案明文、不含票/cookie 值。
#[derive(Debug, Clone)]
pub(crate) struct WarmReport {
    pub(crate) at: SystemTime,
    pub(crate) held: usize,
    pub(crate) opened: bool,
    pub(crate) verdict: Option<&'static str>,
    pub(crate) served_model: Option<String>,
    pub(crate) error: Option<String>,
    /// 最终这条连接落的网关（`unified-N`），从 `__oailb` 解出；观测/验证换节点用。
    pub(crate) gateway: Option<String>,
    /// 本轮实际探了几次（降智会换节点重试）。
    pub(crate) attempts: u32,
    /// 重试路上探到的网关序列（含最终那个），看有没有真的换到不同节点。
    pub(crate) tried_gateways: Vec<String>,
    pub(crate) connection_id: Option<uuid::Uuid>,
    pub(crate) model: String,
}

#[derive(Default)]
struct AccountState {
    models: BTreeMap<String, Instant>,
    warm_models: Vec<String>,
    cooldown_until: HashMap<usize, Instant>,
    in_flight: bool,
    last: Option<WarmReport>,
}

#[derive(Default)]
struct State {
    accounts: HashMap<String, AccountState>,
    policy: Option<(WarmPoolSettings, turn_state::CloudMintSettings)>,
}

/// 探针判决。
enum Verdict {
    /// 本次启用的检查通过，是否检查答案由报告单独标记。
    Verified(Option<String>),
    /// 声明或答案未通过检查。
    Degraded(Option<String>),
    /// 没答成：上游报错 / 连接断 / 超时。
    Failed(String),
}

/// 候选的敏感内容只在验收与条件发布期间存在，不进入 Debug 或管理响应。
struct ProbeCapture {
    ticket: Option<String>,
    headers: Vec<String>,
    route: Option<crate::route_pair::RoutePairRef>,
    connection_id: Option<uuid::Uuid>,
    pin_generation: Option<String>,
}

impl ProbeCapture {
    fn previous_approval<'a>(
        &self,
        previous: Option<&'a (uuid::Uuid, WarmConnectionApproval)>,
    ) -> Option<&'a WarmConnectionApproval> {
        // 同一槽位也可能已重连，旧票的证明只能由原连接继承。
        previous
            .filter(|(id, _)| self.connection_id == Some(*id))
            .map(|(_, approval)| approval)
    }
}

/// 取消或提前返回也撤销候选，后台维护随后回收连接，不会遗留可被领养的半成品。
struct PendingApproval(WarmConnectionApproval);

impl Drop for PendingApproval {
    fn drop(&mut self) {
        if !self.0.published() {
            self.0.reject();
        }
    }
}

/// 不实现 Debug：持有仓库句柄与账号上下文。
pub(crate) struct WarmPoolService {
    repository: CodexCredentialRepository,
    pins: crate::turn_state_pin::TurnStatePins,
    mint: Arc<crate::turn_state_mint::CloudMintService>,
    /// 每次探针从活 profile 快照现建客户端（清掉 residency 头）：既能拿到 __oailb 看网关、
    /// 让裸开探到不同节点，又跟随线上 profile 版本、不产生 connection_profile 漂移使领养失配。
    http: reqwest::Client,
    base_url: String,
    profile: CodexWireProfileState,
    pool: Arc<CodexWebSocketPool>,
    wake: Notify,
    /// 全局在途开连接/探针数，配合 `max_total_connections` 限流。
    in_flight_total: AtomicUsize,
    state: Mutex<State>,
}

/// Drop 守卫：warm 任务无论正常结束还是 panic，都减在途计数并清账号 in_flight，
/// 避免任务 panic 后账号卡在 in_flight=true 再也不被补齐、in_flight_total 永久虚高。
struct WarmInFlight {
    service: Arc<WarmPoolService>,
    account_id: String,
}

impl Drop for WarmInFlight {
    fn drop(&mut self) {
        self.service.in_flight_total.fetch_sub(1, Ordering::AcqRel);
        self.service.clear_in_flight(&self.account_id);
    }
}

impl WarmPoolService {
    pub(crate) fn pool_activity(
        &self,
        now_ms: u64,
    ) -> Result<BTreeMap<String, turn_state::pool::PoolActivity>, turn_state::pool::PoolUnavailable>
    {
        use turn_state::pool::{PoolActivity, PoolAttempt, PoolUnavailable};
        let state = self.state.lock().map_err(|_| PoolUnavailable)?;
        Ok(state
            .accounts
            .iter()
            .map(|(id, entry)| {
                let cooldown = entry
                    .cooldown_until
                    .values()
                    .filter_map(|until| {
                        let remaining = until.saturating_duration_since(Instant::now());
                        (!remaining.is_zero()).then(|| {
                            now_ms.saturating_add(
                                u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX),
                            )
                        })
                    })
                    .min();
                (
                    id.clone(),
                    PoolActivity {
                        in_flight: entry.in_flight,
                        cooldown_until_ms: cooldown,
                        last: entry.last.as_ref().map(|last| PoolAttempt {
                            at_ms: last
                                .at
                                .duration_since(SystemTime::UNIX_EPOCH)
                                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
                            kind: "verification".to_owned(),
                            attempts: u64::from(last.attempts),
                            verdict: last.verdict.map(str::to_owned),
                            error: last.error.clone(),
                            gateway: last.gateway.clone(),
                            model: Some(last.model.clone()),
                        }),
                    },
                )
            })
            .collect())
    }
    pub(crate) fn new(
        repository: CodexCredentialRepository,
        pins: crate::turn_state_pin::TurnStatePins,
        mint: Arc<crate::turn_state_mint::CloudMintService>,
        http: reqwest::Client,
        base_url: impl Into<String>,
        profile: CodexWireProfileState,
        pool: Arc<CodexWebSocketPool>,
    ) -> Self {
        Self {
            repository,
            pins,
            mint,
            http,
            base_url: base_url.into(),
            profile,
            pool,
            wake: Notify::new(),
            in_flight_total: AtomicUsize::new(0),
            state: Mutex::new(State::default()),
        }
    }

    /// 现建一个「去 residency」的基础客户端（跟随活 profile 版本）。
    fn residency_free_client(&self) -> CodexBackendClient {
        let mut wire = self.profile.snapshot();
        wire.residency = None;
        CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            CodexWireProfileState::new(wire),
        )
        .with_websocket_pool(Arc::clone(&self.pool))
    }

    fn settings(&self) -> WarmPoolSettings {
        self.pins.service().settings().warm_pool
    }

    pub(crate) fn enabled(&self) -> bool {
        self.settings().enabled && !self.pins.service().settings().dry_run
    }

    /// 记录业务模型，并唤醒自动模型配置的预热任务。
    pub(crate) fn note_request(&self, account_id: &str, model: &str) {
        let added = if let Ok(mut state) = self.state.lock() {
            let entry = state.accounts.entry(account_id.to_owned()).or_default();
            entry
                .models
                .retain(|_, seen| seen.elapsed() < MODEL_ACTIVITY_WINDOW);
            if model.is_empty() {
                false
            } else {
                let added = !entry.models.contains_key(model);
                if added
                    && entry.models.len() >= MAX_ACTIVE_MODELS
                    && let Some(oldest) = entry
                        .models
                        .iter()
                        .min_by_key(|(_, seen)| *seen)
                        .map(|(model, _)| model.clone())
                {
                    entry.models.remove(&oldest);
                }
                entry.models.insert(model.to_owned(), Instant::now());
                added
            }
        } else {
            false
        };
        if added {
            self.wake.notify_one();
        }
    }

    /// 账号导入或变更后唤醒 worker，资格与在途状态在调度时重新核对。
    pub(crate) fn notify_accounts_changed(&self, account_ids: &[String]) {
        if let Ok(mut state) = self.state.lock() {
            for id in account_ids {
                state.accounts.entry(id.clone()).or_default();
            }
        }
        self.wake.notify_one();
    }

    /// 账号被摘除：忘掉它的保活状态（连接由池的 evict 关闭）。
    pub(crate) fn forget(&self, account_id: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.accounts.remove(account_id);
        }
    }

    pub(crate) async fn wait_wake(&self) {
        self.wake.notified().await;
    }

    /// 账号详情观测用：保活连接数 + 上次探针摘要。
    pub(crate) fn account_view(&self, account_id: &str) -> Value {
        let held = self.pool.warm_len_for_account(account_id);
        let last = self
            .state
            .lock()
            .ok()
            .and_then(|state| state.accounts.get(account_id).and_then(|a| a.last.clone()));
        json!({
            "enabled": self.enabled(),
            "held": held,
            "last": last.map(|r| json!({
                "atMs": r.at.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
                "held": r.held,
                "opened": r.opened,
                "verdict": r.verdict,
                "servedModel": r.served_model,
                "error": r.error,
                "gateway": r.gateway,
                "attempts": r.attempts,
                "triedGateways": r.tried_gateways,
                "connectionId": r.connection_id.map(|id| id.to_string()),
                "probeModel": r.model,
            })),
        })
    }

    /// 一轮保活：刷新业务复用开关、补齐/续探每个合格账号的保活连接。
    pub(crate) async fn run_cycle(self: &Arc<Self>) {
        if self.pins.service().settings().dry_run {
            return;
        }
        let settings = self.settings();
        self.pool
            .set_warm_reuse(settings.enabled && settings.business_reuse);
        if !settings.enabled {
            return;
        }
        let changed_accounts = {
            let mut state = Self::lock_state(&self.state);
            let policy = (settings.clone(), self.pins.service().settings().cloud_mint);
            if state.policy.as_ref() == Some(&policy) {
                Vec::new()
            } else {
                state.policy = Some(policy);
                for entry in state.accounts.values_mut() {
                    entry.cooldown_until.clear();
                }
                state.accounts.keys().cloned().collect::<Vec<_>>()
            }
        };
        for account in changed_accounts {
            self.pool.evict_warm_for_account(&account).await;
        }
        self.pool.restrict_warm_lifetime(settings.max_age());
        let accounts = match self.repository.list_for_provider().await {
            Ok(accounts) => accounts,
            Err(_) => return,
        };
        let now = Instant::now();
        for account in accounts {
            if !Self::eligible(&account) {
                continue;
            }
            // 只保活「已开启 state 绑定（固定自身 state）」的账号——即导入时勾选的新号；
            // 存量/未绑定号一律不碰（不 hunt、不烧动态网关、不动在用号）。
            // 用当前 account（活跃号 revision 会被业务改）判断，别用列表里的陈旧副本。
            let bound = match self.repository.store().get_account(account.id()).await {
                Ok(Some(fresh)) => {
                    // fork: account-ticket — 已过期的账号不预热
                    if crate::fork_account_ticket::is_expired(
                        self.repository.store(),
                        &fresh,
                        SystemTime::now(),
                    )
                    .await
                    {
                        continue;
                    }
                    self.repository
                        .load_runtime_credential(&fresh)
                        .await
                        .map(|c| c.turn_state_pin.is_some())
                        .unwrap_or(false)
                }
                _ => false,
            };
            if !bound {
                continue;
            }
            let id = account.id().as_str().to_owned();
            let models = self.probe_models(&id, &settings);
            let models_changed = {
                let mut state = Self::lock_state(&self.state);
                let entry = state.accounts.entry(id.clone()).or_default();
                if entry.in_flight {
                    continue;
                }
                let changed = entry.warm_models != models;
                if changed {
                    entry.warm_models = models.clone();
                    entry.cooldown_until.clear();
                }
                changed
            };
            if models_changed {
                self.pool.evict_warm_for_account(&id).await;
            }
            let want = settings.connections_per_account as usize * models.len();
            // 池是「哪些 slot 有活连接」的唯一真相（可能被业务领养/被 evict 掉）。
            // 先在池锁外取占用序号，再进 warmer 状态锁，避免同时持两把锁。
            let occupied = self.pool.warm_slots_for_account(&id);
            let needs_reprobe = self.pool.warm_slots_due_for_reprobe(&id);
            let can_open = self.in_flight_total.load(Ordering::Acquire)
                + self.pool.warm_len_total()
                < settings.max_total_connections as usize
                && self.pool.has_connect_capacity();

            // 挑一个要开/要复探的 slot（open 用空闲序号、reprobe 用已占用且到期的序号）。
            let (slot, reason) = {
                let mut state = Self::lock_state(&self.state);
                let entry = state.accounts.entry(id.clone()).or_default();
                if entry.in_flight {
                    continue;
                }
                // 复探不依赖补池成功；同时有两类任务时交替，避免较短复探间隔饿死补池。
                let due = settings
                    .probe
                    .then(|| needs_reprobe.iter().next().copied())
                    .flatten();
                let missing = can_open
                    .then(|| {
                        (0..want).find(|slot| {
                            !occupied.contains(slot)
                                && entry
                                    .cooldown_until
                                    .get(slot)
                                    .is_none_or(|until| *until <= now)
                        })
                    })
                    .flatten();
                let pick = match (due, missing) {
                    (Some(slot), None) => Some((slot, "reprobe")),
                    (Some(slot), Some(_))
                        if entry.last.as_ref().is_some_and(|last| last.opened) =>
                    {
                        Some((slot, "reprobe"))
                    }
                    (_, Some(slot)) => Some((slot, "open")),
                    (None, None) => None,
                };
                match pick {
                    Some(pick) => {
                        entry.in_flight = true;
                        pick
                    }
                    None => continue,
                }
            };

            let service = Arc::clone(self);
            let task_settings = settings.clone();
            self.in_flight_total.fetch_add(1, Ordering::AcqRel);
            // Drop 守卫：任务正常结束或 panic 都会减在途计数、清 in_flight，避免卡死账号。
            let guard = WarmInFlight {
                service: Arc::clone(self),
                account_id: id.clone(),
            };
            self.pool.spawn_connect_task(async move {
                let _guard = guard;
                service
                    .warm_one(account, slot, reason, &task_settings)
                    .await;
            });
        }
    }

    fn lock_state(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
        // 锁中毒不该让 daemon crash-loop：其余访问器都用 if let Ok，这里也容忍。
        state.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn clear_in_flight(&self, account_id: &str) {
        if let Ok(mut state) = self.state.lock()
            && let Some(entry) = state.accounts.get_mut(account_id)
        {
            entry.in_flight = false;
        }
    }

    fn eligible(account: &ProviderAccount) -> bool {
        account.provider().as_str() == "openai"
            && account.authentication_kind() == CODEX_AUTHENTICATION_KIND_OAUTH
            && account.enabled()
    }

    /// 为一个模型槽位寻找或复探连接，失败候选关闭后有限重试。
    /// 新拨不保证网关一定变化，仍以完整探针结果决定发布，耗尽后仅冷却该槽位。
    async fn warm_one(
        &self,
        account: ProviderAccount,
        slot: usize,
        reason: &'static str,
        settings: &WarmPoolSettings,
    ) {
        let id = account.id().as_str().to_owned();
        let mut report = WarmReport {
            at: SystemTime::now(),
            held: 0,
            opened: reason == "open",
            verdict: None,
            served_model: None,
            error: None,
            gateway: None,
            attempts: 0,
            tried_gateways: Vec::new(),
            connection_id: None,
            model: String::new(),
        };
        let mut verified = false;
        let mut all_degraded = false;
        let models = self.probe_models(&id, settings);
        let model = models[slot / settings.connections_per_account as usize % models.len()].clone();
        report.model.clone_from(&model);
        let mint_settings = self.pins.service().settings().cloud_mint;
        let proxy_url = if mint_settings.enabled {
            mint_settings.upstream_proxy_url.clone()
        } else {
            String::new()
        };
        let business_client = match self.residency_free_client().for_account(&account) {
            Ok(client) => client,
            Err(_) => {
                self.set_cooldown(&id, slot, settings.cooldown());
                return;
            }
        };
        let deadline =
            tokio::time::Instant::now() + settings.probe_timeout().max(MIN_PROBE_TIMEOUT);
        for attempt in 0..=settings.probe_retries {
            report.attempts = attempt + 1;
            let approval = WarmConnectionApproval::scoped(
                model.clone(),
                settings.max_age(),
                (!proxy_url.is_empty()).then(|| business_client.pool_egress_key().to_owned()),
            );
            let _pending = PendingApproval(approval.clone());
            let previous_approval = self.pool.begin_warm_probe(&id, slot, &approval);
            let publication = if proxy_url.is_empty() || !settings.business_reuse {
                None
            } else {
                match tokio::time::timeout_at(deadline, self.mint.warm_publication(&id)).await {
                    Ok(Ok(snapshot)) => Some(snapshot),
                    Ok(Err(_)) => {
                        report.error = Some("credential_changed".to_owned());
                        break;
                    }
                    Err(_) => {
                        report.error = Some("timeout".to_owned());
                        break;
                    }
                }
            };
            let fresh =
                attempt > 0 || (reason == "open" && self.pool.warm_len_for_account(&id) == 0);
            let result = tokio::time::timeout_at(
                deadline,
                self.warm_probe_model(
                    &account, slot, settings, &model, &approval, &proxy_url, fresh,
                ),
            )
            .await
            .unwrap_or_else(|_| Err("timeout".to_owned()));
            let mut capture = None;
            let result = result.map(|(verdict, evidence)| {
                report.connection_id = evidence.connection_id;
                let gateway = evidence
                    .route
                    .as_ref()
                    .and_then(|route| route.gateway.clone());
                capture = Some(evidence);
                (verdict, gateway)
            });
            let current = self.pins.service().settings();
            if current.dry_run
                || !current.warm_pool.enabled
                || current.warm_pool != *settings
                || current.cloud_mint != mint_settings
            {
                approval.reject();
                self.pool.evict_warm_slot(&id, slot).await;
                return;
            }
            match result {
                Ok((Verdict::Verified(served_model), gateway)) => {
                    if !self.pool.has_warm_candidate(&approval) {
                        approval.reject();
                        report.error = Some("connection_lost".to_owned());
                        self.set_cooldown(&id, slot, settings.cooldown());
                        break;
                    }
                    if let Some(snapshot) = &publication {
                        let Some(evidence) = &capture else {
                            approval.reject();
                            break;
                        };
                        let Some(route) = &evidence.route else {
                            approval.reject();
                            self.pool.evict_warm_slot(&id, slot).await;
                            report.error = Some("no_route_pair".to_owned());
                            continue;
                        };
                        // 复探未新签票时保留原票的到期约束，连接质量由本次完整答题决定。
                        let ticket = evidence
                            .ticket
                            .as_deref()
                            .map(|ticket| (model.as_str(), ticket));
                        // 进入提交后必须完成失败恢复，不能在 CAS 已提交时用探针超时取消。
                        if tokio::time::Instant::now() >= deadline
                            || self
                                .mint
                                .publish_warm(snapshot, &evidence.headers, route, ticket)
                                .await
                                .is_err()
                        {
                            approval.reject();
                            self.pool.evict_warm_slot(&id, slot).await;
                            report.error = Some("candidate_publication_failed".to_owned());
                            break;
                        }
                        self.pool
                            .evict_other_warm_routes(&id, &route.fingerprint)
                            .await;
                    }
                    let latest = self.pins.service().settings();
                    if latest.dry_run
                        || latest.warm_pool != *settings
                        || latest.cloud_mint != mint_settings
                    {
                        approval.reject();
                        self.pool.evict_warm_slot(&id, slot).await;
                        return;
                    }
                    let ticket = capture
                        .as_ref()
                        .and_then(|evidence| evidence.ticket.as_deref());
                    let ticket_ttl = latest.ttl().min(latest.cloud_mint.ticket_ttl());
                    let valid_for = verification_valid_for(
                        settings.reprobe(),
                        ticket_ttl,
                        ticket,
                        report.at,
                        SystemTime::now(),
                    );
                    approval.publish_rechecked(
                        settings.probe,
                        valid_for,
                        ticket,
                        ticket.map(|ticket| {
                            verification_valid_for(
                                ticket_ttl,
                                ticket_ttl,
                                Some(ticket),
                                report.at,
                                SystemTime::now(),
                            )
                        }),
                        Some(WarmConnectionApproval::policy_key(
                            settings,
                            ticket_ttl,
                            capture
                                .as_ref()
                                .and_then(|evidence| evidence.pin_generation.as_deref()),
                        )),
                        capture.as_ref().and_then(|evidence| {
                            evidence.previous_approval(previous_approval.as_ref())
                        }),
                    );
                    if !approval.published() {
                        self.pool.evict_warm_slot(&id, slot).await;
                        report.verdict = Some("failed");
                        report.error = Some("verification_expired".to_owned());
                        continue;
                    }
                    report.verdict = Some(if settings.probe { "verified" } else { "ready" });
                    report.served_model = served_model;
                    report.gateway = gateway.clone();
                    if let Some(gw) = gateway {
                        report.tried_gateways.push(gw);
                    }
                    tracing::info!(
                        target: "ws_warm",
                        account_id = %id,
                        slot,
                        attempt = report.attempts,
                        gateway = report.gateway.as_deref().unwrap_or("?"),
                        checked = settings.probe,
                        connection_id = report.connection_id.map(|id| id.to_string()).as_deref().unwrap_or(""),
                        "[ws-warm] candidate accepted and held"
                    );
                    verified = true;
                    break;
                }
                Ok((Verdict::Degraded(model), gateway)) => {
                    approval.reject();
                    report.verdict = Some("degraded");
                    report.served_model = model;
                    report.gateway = gateway.clone();
                    if let Some(gw) = gateway {
                        report.tried_gateways.push(gw);
                    }
                    all_degraded = true;
                    // 下一次从新连接、新候选路由开始，到重试上限后冷却该槽位。
                    self.pool.evict_warm_slot(&id, slot).await;
                }
                Ok((Verdict::Failed(code), gateway)) => {
                    approval.reject();
                    report.verdict = Some("failed");
                    report.error = Some(code);
                    report.gateway = gateway;
                    self.pool.evict_warm_slot(&id, slot).await;
                    if matches!(
                        report.error.as_deref(),
                        Some("timeout" | "no_answer" | "no_terminal")
                    ) && tokio::time::Instant::now() < deadline
                    {
                        continue;
                    }
                    self.set_cooldown(&id, slot, settings.cooldown().min(Duration::from_secs(120)));
                    break; // auth/超时重试也没用
                }
                Err(error) => {
                    approval.reject();
                    self.pool.evict_warm_slot(&id, slot).await;
                    report.verdict = Some("failed");
                    report.error = Some(error);
                    if matches!(
                        report.error.as_deref(),
                        Some(
                            "upstream_transport"
                                | "upstream_http_502"
                                | "upstream_http_503"
                                | "upstream_http_504"
                        )
                    ) && tokio::time::Instant::now() < deadline
                    {
                        continue;
                    }
                    self.set_cooldown(&id, slot, settings.cooldown().min(Duration::from_secs(120)));
                    break;
                }
            }
        }
        if !verified && all_degraded {
            // 本轮候选均未通过，保留其他模型已经验证的连接。
            let closed = self.pool.evict_warm_slot(&id, slot).await;
            tracing::info!(
                target: "ws_warm",
                account_id = %id,
                attempts = report.attempts,
                gateways = ?report.tried_gateways,
                closed,
                "[ws-warm] all attempts degraded, cooling down"
            );
            self.set_cooldown(&id, slot, settings.cooldown());
        } else if !verified {
            self.pool.evict_warm_slot(&id, slot).await;
            self.set_cooldown(&id, slot, settings.cooldown().min(Duration::from_secs(120)));
        }
        report.held = self.pool.warm_len_for_account(&id);
        if let Ok(mut state) = self.state.lock() {
            let entry = state.accounts.entry(id).or_default();
            entry.last = Some(report);
        }
        // in_flight 的清除交给 WarmInFlight 守卫（任务结束/panic 都清），这里不动。
    }

    fn set_cooldown(&self, account_id: &str, slot: usize, cooldown: Duration) {
        if let Ok(mut state) = self.state.lock() {
            let entry = state.accounts.entry(account_id.to_owned()).or_default();
            entry.cooldown_until.insert(slot, Instant::now() + cooldown);
        }
    }

    /// 发一条 canary 请求（WS 强制、store=false），落进保活 key、读答案判满血。
    /// 返回判决 + 这条连接落的网关（从上游回的 `__oailb` 解出，可能为 None）。
    #[expect(
        clippy::too_many_arguments,
        reason = "候选探测分别携带模型、审批、出口与新路由选择"
    )]
    async fn warm_probe_model(
        &self,
        account: &ProviderAccount,
        slot: usize,
        settings: &WarmPoolSettings,
        model: &str,
        approval: &WarmConnectionApproval,
        proxy_url: &str,
        fresh_route: bool,
    ) -> Result<(Verdict, ProbeCapture), String> {
        // 业务流量会并发改凭据 revision，warmer 缓存的 account 会过期→load_runtime_credential
        // 判 RevisionConflict 报 "credential"（重试第 2 次就中）。每次探针先取当前 account。
        let fresh = self
            .repository
            .store()
            .get_account(account.id())
            .await
            .map_err(|_| "account".to_owned())?
            .ok_or_else(|| "account".to_owned())?;
        let account = &fresh;
        let credential = self
            .repository
            .load_runtime_credential(account)
            .await
            .map_err(|_| "credential".to_owned())?;
        let authorization = credential
            .authentication
            .authorization_header()
            .map_err(|_| "authorization".to_owned())?;
        let cookie_header = crate::provider::build_cookie_header(&credential.cookies)
            .map_err(|_| "cookies".to_owned())?;
        let probe_account = if proxy_url.is_empty() {
            account.clone()
        } else {
            let proxy = gateway_core::account::OutboundProxy::parse(proxy_url)
                .map_err(|_| "dedicated_proxy".to_owned())?;
            account.clone().with_outbound_proxy(Some(proxy))
        };
        let client = self
            .residency_free_client()
            .for_account(&probe_account)
            .map_err(|_| "client".to_owned())?
            .with_authentication(&credential.authentication);

        let prompt = if settings.probe_prompt.trim().is_empty() {
            DEFAULT_PROBE_PROMPT
        } else {
            settings.probe_prompt.as_str()
        };
        let mut body = Map::new();
        body.insert("model".to_owned(), Value::String(model.to_owned()));
        body.insert("instructions".to_owned(), Value::String(String::new()));
        body.insert(
            "input".to_owned(),
            json!([{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": prompt}],
            }]),
        );
        body.insert(
            "reasoning".to_owned(),
            json!({"effort": settings.probe_effort}),
        );
        body.insert("store".to_owned(), Value::Bool(false));
        body.insert("stream".to_owned(), Value::Bool(true));
        let mut request = CodexResponsesRequest::from_body(body);
        request.warm_approval = Some(approval.clone());
        request.discard_mismatched_connection = !self.pins.service().settings().dry_run;
        // conversation 以保活前缀命名 → 连接落进保活 key；带下游标记 → 走 WebSocketNewChain
        // （无 fast-path 预算，强制真正建 WS 而非退回 HTTP）。
        request.local_conversation_id = Some(format!("{WARM_CONVERSATION_PREFIX}{slot}"));
        request.downstream_websocket_connection_id = Some("__cpr_warm__".to_owned());

        let request_id = format!("ws-warm-{}-{slot}", uuid::Uuid::new_v4());
        let cookie_ref = cookie_header.as_ref().map(ExposeSecret::expose_secret);
        let mut context = CodexRequestContext::auxiliary(
            authorization.expose_secret(),
            account.upstream_account_id(),
            &request_id,
            Some(credential.installation_id.as_str()),
        );
        context.cookie_header = if !proxy_url.is_empty() && fresh_route {
            None
        } else {
            cookie_ref
        };

        let timeout = settings.probe_timeout().max(MIN_PROBE_TIMEOUT);
        // pool_account_id 必须用「本地账号 id」，与业务连接池 key 对齐，业务才能领养。
        let local_account_id = account.id().as_str().to_owned();
        let response = client
            .create_response_stream_with_pool_account(
                &request,
                context,
                Some(local_account_id.as_str()),
            )
            .await
            .map_err(probe_transport_error)?;
        // 网关号从上游回的 __oailb（裸开的连接才会带）解出；set_cookie_headers 是响应字段，
        // 在消费 body 前先取。
        let mut capture = ProbeCapture {
            ticket: response.turn_state.clone(),
            headers: response.set_cookie_headers.clone(),
            route: crate::route_pair::RoutePairRef::issued(&response.set_cookie_headers),
            connection_id: response.websocket_connection_id,
            pin_generation: credential.turn_state_pin.clone(),
        };
        let updates = response.response_metadata_updates.clone();
        let verdict = self
            .read_verdict(
                response,
                settings.probe.then_some(settings.probe_expect.as_str()),
                model,
                timeout,
            )
            .await?;
        if let Some(updates) = updates {
            let updates = updates.lock().await;
            capture.ticket = updates.turn_state.clone().or(capture.ticket);
            capture.route = updates.route_pair.clone().or(capture.route);
        }
        Ok((verdict, capture))
    }

    /// 读上游 SSE 流判满血；**必须把流读到自然结束（None）**，上游 WS 连接才会被归还进连接池
    /// 挂住供业务领养——提前 return/break 会丢弃这条连接（held 永远 0）。拿到判决后继续排空。
    /// 只累计文本增量判定，不记明文答案。
    async fn read_verdict(
        &self,
        response: CodexBackendStreamingResponse,
        expect: Option<&str>,
        probe_model: &str,
        timeout: Duration,
    ) -> Result<Verdict, String> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut header_model_seen = response.response_metadata.effective_model.clone();
        let mut body_model_seen = None;
        let metadata_updates = response.response_metadata_updates.clone();
        let mut metadata_mismatch = header_model_seen
            .as_deref()
            .is_some_and(|model| !model.eq_ignore_ascii_case(probe_model));
        let mut body = response.body;
        let mut buf: Vec<u8> = Vec::new();
        let mut answer = String::new();
        // 上游在 created 或终态里声明了别的模型：答对题也不算满血。
        let mut swapped: Option<String> = None;
        let mut verdict: Option<Verdict> = None;
        loop {
            let next = match tokio::time::timeout_at(deadline, body.next()).await {
                Ok(Some(Ok(chunk))) => chunk,
                Ok(Some(Err(_))) => break,
                Ok(None) => break, // 流自然结束 → 连接已归还池
                Err(_) => {
                    verdict.get_or_insert(Verdict::Failed("timeout".to_owned()));
                    break;
                }
            };
            if let Some(updates) = &metadata_updates {
                let updates = updates.lock().await;
                metadata_mismatch |= updates.served_mismatch;
                if let Some(model) = &updates.reported_model {
                    header_model_seen = Some(model.clone());
                }
            }
            // 判决已定：只排空剩余字节让连接归还，不再解析。
            if verdict.is_some() {
                continue;
            }
            buf.extend_from_slice(&next);
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim().strip_prefix("data:") else {
                    continue;
                };
                let Ok(event) = serde_json::from_str::<Value>(data.trim()) else {
                    continue;
                };
                let event_type = event.get("type").and_then(Value::as_str);
                let (header_model, body_model) = event_declared_models(&event);
                if header_model.is_some() {
                    header_model_seen.clone_from(&header_model);
                }
                if body_model.is_some() {
                    body_model_seen.clone_from(&body_model);
                }
                if swapped.is_none()
                    && let Some(declared) = disagreeing_model(
                        header_model.as_deref(),
                        body_model.as_deref(),
                        probe_model,
                    )
                {
                    swapped = Some(declared);
                }
                match event_type {
                    Some("response.output_text.delta") => {
                        if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                            answer.push_str(delta);
                        }
                    }
                    Some("response.completed") => {
                        let served_model = body_model_seen
                            .clone()
                            .or_else(|| header_model_seen.clone());
                        verdict = Some(if metadata_mismatch {
                            Verdict::Degraded(served_model)
                        } else {
                            Self::judge(&answer, expect, probe_model, served_model, swapped.take())
                        });
                        break;
                    }
                    Some("response.failed") | Some("error") => {
                        let code = event
                            .pointer("/error/code")
                            .or_else(|| event.pointer("/response/error/code"))
                            .and_then(Value::as_str)
                            .unwrap_or("upstream_error")
                            .to_owned();
                        verdict = Some(Verdict::Failed(code));
                        break;
                    }
                    _ => {}
                }
            }
        }
        // 没读到终态就断了：答案可能不完整，也没有终态的模型声明，不能据此判满血。
        Ok(verdict.unwrap_or_else(|| {
            Verdict::Failed(
                if answer.trim().is_empty() {
                    "no_answer"
                } else {
                    "no_terminal"
                }
                .to_owned(),
            )
        }))
    }

    fn judge(
        answer: &str,
        expect: Option<&str>,
        probe_model: &str,
        served_model: Option<String>,
        swapped: Option<String>,
    ) -> Verdict {
        if swapped.is_some() {
            return Verdict::Degraded(swapped);
        }
        // 没看到声明，或声明和探针模型不是同一个，答对也不算满血。
        let declared_matches = served_model
            .as_deref()
            .is_some_and(|model| model.eq_ignore_ascii_case(probe_model));
        if !declared_matches || expect.is_some_and(|expected| !answer_matches(answer, expected)) {
            return Verdict::Degraded(served_model);
        }
        Verdict::Verified(served_model)
    }

    fn probe_models(&self, account: &str, settings: &WarmPoolSettings) -> Vec<String> {
        if !settings.probe_model.trim().is_empty() {
            return vec![settings.probe_model.clone()];
        }
        if !settings.models.is_empty() {
            return settings.models.clone();
        }
        let active = self.state.lock().ok().and_then(|mut state| {
            state.accounts.get_mut(account).map(|entry| {
                entry
                    .models
                    .retain(|_, seen| seen.elapsed() < MODEL_ACTIVITY_WINDOW);
                entry.models.keys().cloned().collect::<Vec<_>>()
            })
        });
        active
            .filter(|models| !models.is_empty())
            .unwrap_or_else(|| vec![DEFAULT_WARM_MODEL.to_owned()])
    }
}

impl turn_state::pool::PoolRuntime for WarmPoolService {
    fn snapshot(&self) -> turn_state::pool::PoolSnapshotFuture<'_> {
        Box::pin(async move {
            crate::pool_overview::snapshot(
                &self.repository,
                &self.pins,
                &self.pool,
                self,
                &self.mint,
            )
            .await
        })
    }
}

/// 响应头 `openai-model` / `x-openai-model` 与正文 `response.model` 分开取。
fn event_declared_models(event: &Value) -> (Option<String>, Option<String>) {
    let header = [event.pointer("/response/headers"), event.get("headers")]
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .find_map(|headers| {
            headers.iter().find_map(|(name, value)| {
                if !name.eq_ignore_ascii_case("openai-model")
                    && !name.eq_ignore_ascii_case("x-openai-model")
                {
                    return None;
                }
                value
                    .as_str()
                    .or_else(|| value.as_array()?.first()?.as_str())
                    .map(str::to_owned)
            })
        });
    let body = event
        .pointer("/response/model")
        .and_then(Value::as_str)
        .map(str::to_owned);
    (header, body)
}

/// 两处声明有一处和探针模型不一致就返回那一处。都没声明时返回 None，交给 judge 拒绝满血。
fn disagreeing_model(
    header: Option<&str>,
    body: Option<&str>,
    probe_model: &str,
) -> Option<String> {
    [header, body]
        .into_iter()
        .flatten()
        .find(|model| !model.eq_ignore_ascii_case(probe_model))
        .map(str::to_owned)
}

/// 只接受完整期望值；允许单层强调或行内代码，不把带解释或矛盾结论的前缀判为通过。
fn answer_matches(answer: &str, expect: &str) -> bool {
    let answer = answer.trim();
    let normalized = ["**", "__", "`", "*", "_"]
        .into_iter()
        .find_map(|marker| {
            answer
                .strip_prefix(marker)
                .and_then(|text| text.strip_suffix(marker))
        })
        .unwrap_or(answer)
        .trim();
    normalized == expect.trim()
}

fn verification_valid_for(
    reprobe: Duration,
    ticket_ttl: Duration,
    ticket: Option<&str>,
    captured_at: SystemTime,
    now: SystemTime,
) -> Duration {
    let limit = reprobe.min(ticket_ttl);
    let Some(ticket) = ticket else { return limit };
    turn_state::fernet::resolve_issued_at(ticket, captured_at, now, Duration::from_secs(30))
        .ok()
        .and_then(|issued| issued.at.checked_add(ticket_ttl))
        .and_then(|expires| expires.duration_since(now).ok())
        .map_or(Duration::ZERO, |remaining| remaining.min(limit))
}

/// 代理或上游错误只留下受控标签，不把响应正文、认证信息或代理 URL 写进报告。
fn probe_transport_error(error: crate::transport::CodexClientError) -> String {
    use crate::transport::CodexClientError;
    use crate::transport::websocket::CodexWebSocketExchangeError;
    let status = match &error {
        CodexClientError::Upstream { status, .. } => Some(status.as_u16()),
        CodexClientError::WebSocket(error) => match error.classified() {
            CodexWebSocketExchangeError::Upstream(upstream) => Some(upstream.status_code),
            CodexWebSocketExchangeError::Connect(tungstenite::Error::Http(response))
            | CodexWebSocketExchangeError::Transport(tungstenite::Error::Http(response)) => {
                Some(response.status().as_u16())
            }
            _ => None,
        },
        _ => None,
    };
    status.map_or_else(
        || "upstream_transport".to_owned(),
        |status| format!("upstream_http_{status}"),
    )
}

#[cfg(test)]
mod judge_tests {
    use super::{Verdict, WarmPoolService};

    #[tokio::test(start_paused = true)]
    async fn reconnecting_a_slot_cannot_transfer_the_previous_connections_ticket_proof() {
        use super::{ProbeCapture, WarmConnectionApproval};
        use std::time::Duration;

        let lifetime = Duration::from_secs(240);
        let previous = WarmConnectionApproval::new("model".into(), lifetime);
        previous.publish_scoped(
            true,
            Duration::from_secs(30),
            Some("synthetic-old-ticket"),
            None,
        );
        let old_id = uuid::Uuid::new_v4();
        let previous = (old_id, previous);
        for id in [Some(old_id), Some(uuid::Uuid::new_v4()), None] {
            let capture = ProbeCapture {
                ticket: None,
                headers: Vec::new(),
                route: None,
                connection_id: id,
                pin_generation: None,
            };
            let next = WarmConnectionApproval::new("model".into(), lifetime);
            next.publish_rechecked(
                true,
                lifetime,
                None,
                None,
                None,
                capture.previous_approval(Some(&previous)),
            );
            tokio::time::advance(Duration::from_secs(31)).await;
            assert_eq!(next.checked(), id != Some(old_id));
        }
    }

    #[test]
    fn answer_must_not_contain_a_second_or_contradicting_conclusion() {
        for answer in [
            "21，但最终答案是29",
            "21 or 29",
            "21.0",
            "21\n29",
            "21 apples",
            "210",
        ] {
            assert!(!super::answer_matches(answer, "21"), "accepted {answer:?}");
        }
        for answer in ["21", " **21** ", "`21`", "_21_", "\n21\n"] {
            assert!(super::answer_matches(answer, "21"), "rejected {answer:?}");
        }
    }

    #[test]
    fn probe_proof_cannot_outlive_the_ticket_that_was_issued_before_the_probe_completed() {
        use base64::Engine as _;
        use std::time::{Duration, UNIX_EPOCH};
        let issued = 1_700_000_000u64;
        let mut bytes = vec![0x80];
        bytes.extend_from_slice(&issued.to_be_bytes());
        let ticket = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let at = UNIX_EPOCH + Duration::from_secs(issued);
        let ttl = Duration::from_secs(240);
        assert_eq!(
            super::verification_valid_for(
                Duration::from_secs(300),
                ttl,
                Some(&ticket),
                at,
                at + Duration::from_secs(80)
            ),
            Duration::from_secs(160)
        );
        assert_eq!(
            super::verification_valid_for(
                Duration::from_secs(300),
                ttl,
                Some(&ticket),
                at,
                at + ttl
            ),
            Duration::ZERO
        );
        assert_eq!(
            super::verification_valid_for(
                Duration::from_secs(300),
                ttl,
                Some(&ticket),
                at,
                at - Duration::from_secs(60)
            ),
            Duration::ZERO
        );
    }

    #[test]
    fn a_right_answer_from_a_swapped_model_is_not_verified() {
        let swapped = Some("gpt-5.6-luna".to_owned());
        assert!(matches!(
            WarmPoolService::judge("21", Some("21"), "gpt-6-astra", swapped.clone(), swapped),
            Verdict::Degraded(Some(model)) if model == "gpt-5.6-luna"
        ));
        assert!(matches!(
            WarmPoolService::judge(
                "**21**",
                Some("21"),
                "gpt-6-astra",
                Some("gpt-6-astra".to_owned()),
                None
            ),
            Verdict::Verified(_)
        ));
        assert!(matches!(
            WarmPoolService::judge(
                "21",
                Some("21"),
                "gpt-6-astra",
                Some("GPT-6-ASTRA".to_owned()),
                None
            ),
            Verdict::Verified(_)
        ));
        assert!(matches!(
            WarmPoolService::judge(
                "20",
                Some("21"),
                "gpt-6-astra",
                Some("gpt-6-astra".to_owned()),
                None
            ),
            Verdict::Degraded(_)
        ));
        assert!(matches!(
            WarmPoolService::judge(
                "210",
                Some("21"),
                "gpt-6-astra",
                Some("gpt-6-astra".to_owned()),
                None
            ),
            Verdict::Degraded(_)
        ));
        assert!(matches!(
            WarmPoolService::judge("21", Some("21"), "gpt-6-astra", None, None),
            Verdict::Degraded(None)
        ));
    }

    #[test]
    fn a_header_model_disagrees_even_when_the_body_matches() {
        let event = serde_json::json!({
            "type": "response.created",
            "response": {
                "model": "gpt-6-astra",
                "headers": {"openai-model": "gpt-5.6-luna"}
            }
        });
        let (header, body) = super::event_declared_models(&event);
        assert_eq!(
            super::disagreeing_model(header.as_deref(), body.as_deref(), "gpt-6-astra").as_deref(),
            Some("gpt-5.6-luna")
        );
        let header_only = serde_json::json!({"headers": {"x-openai-model": ["gpt-6-astra"]}});
        let (header, body) = super::event_declared_models(&header_only);
        assert_eq!(header.as_deref(), Some("gpt-6-astra"));
        assert!(body.is_none());
    }
}
