//! WS 保活暖池：为每个合格账号在"健康期"内预建并挂住若干条上游 WebSocket，
//! 用 canary 探针（默认糖果题）验满血；业务新对话由连接池领养这些满血连接
//! （见 `transport/websocket/pool` 的 `adopt_warm_locked`），从而不被后续坏路由拖降。
//!
//! 打开一条保活连接不需要专门的握手代码：直接用业务同一条客户端路径
//! `create_response_stream_with_pool_account` 发一条 conversation 以 `__cpr_warm__:{slot}`
//! 命名的请求即可——连接落进池里的保活 key，业务据此领养。探针答案只读开头、判满血，
//! 不落明文；降智则关掉该账号的保活连接并冷却，避免业务领养到降智连接、也避免空转烧配额。

use std::{
    collections::{BTreeSet, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use futures::StreamExt as _;
use gateway_core::account::ProviderAccount;
use secrecy::ExposeSecret;
use serde_json::{Map, Value, json};
use tokio::sync::Notify;
use turn_state::WarmPoolSettings;

use crate::credential::{CODEX_AUTHENTICATION_KIND_OAUTH, CodexCredentialRepository};
use crate::transport::protocol::responses::CodexResponsesRequest;
use crate::transport::websocket::WARM_CONVERSATION_PREFIX;
use crate::transport::{
    CodexBackendClient, CodexBackendStreamingResponse, CodexRequestContext, CodexWebSocketPool,
};

/// 最近有请求的账号优先补齐 / 续探。
const ACTIVE_WINDOW: Duration = Duration::from_secs(600);
/// 刚导入的账号视为"在健康期内"，立刻把所有 slot 一次性补满以抢窗口。
const IMPORT_BURST_WINDOW: Duration = Duration::from_secs(600);
/// 探针/开连接的整次调用硬上限的下限保护。
const MIN_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
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
}

#[derive(Default)]
struct AccountState {
    imported_at: Option<Instant>,
    last_request: Option<Instant>,
    models: BTreeSet<String>,
    cooldown_until: Option<Instant>,
    /// 每个 slot 上一次探针通过的时间；用于低频复探。
    verified_at: HashMap<usize, Instant>,
    /// 是否已经至少尝试过一次开连接（首次见到的启用账号 bootstrap 一次）。
    bootstrapped: bool,
    in_flight: bool,
    last: Option<WarmReport>,
}

#[derive(Default)]
struct State {
    accounts: HashMap<String, AccountState>,
}

/// 探针判决。
enum Verdict {
    /// 满血：答案开头匹配期望值。
    Verified(Option<String>),
    /// 降智：连上了、答出来了，但不是满血答案。
    Degraded(Option<String>),
    /// 没答成：上游报错 / 连接断 / 超时。
    Failed(String),
}

/// 不实现 Debug：持有仓库句柄与账号上下文。
pub(crate) struct WarmPoolService {
    repository: CodexCredentialRepository,
    pins: crate::turn_state_pin::TurnStatePins,
    /// 带连接池的基础客户端；`for_account` 后即得到账号出口 + 池。
    client: CodexBackendClient,
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
    pub(crate) fn new(
        repository: CodexCredentialRepository,
        pins: crate::turn_state_pin::TurnStatePins,
        client: CodexBackendClient,
        pool: Arc<CodexWebSocketPool>,
    ) -> Self {
        Self {
            repository,
            pins,
            client,
            pool,
            wake: Notify::new(),
            in_flight_total: AtomicUsize::new(0),
            state: Mutex::new(State::default()),
        }
    }

    fn settings(&self) -> WarmPoolSettings {
        self.pins.service().settings().warm_pool
    }

    pub(crate) fn enabled(&self) -> bool {
        self.settings().enabled
    }

    /// 请求侧：记下账号在用哪些模型、最近何时有流量（补齐/续探优先照顾活跃账号）。
    pub(crate) fn note_request(&self, account_id: &str, model: &str) {
        if let Ok(mut state) = self.state.lock() {
            let entry = state.accounts.entry(account_id.to_owned()).or_default();
            entry.last_request = Some(Instant::now());
            if !model.is_empty() {
                entry.models.insert(model.to_owned());
            }
        }
    }

    /// 账号导入/变更：标记为刚导入（在健康期内），并唤醒 worker 立刻补齐。
    pub(crate) fn notify_accounts_changed(&self, account_ids: &[String]) {
        if let Ok(mut state) = self.state.lock() {
            let now = Instant::now();
            for id in account_ids {
                state.accounts.entry(id.clone()).or_default().imported_at = Some(now);
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
            })),
        })
    }

    /// 一轮保活：刷新业务复用开关、补齐/续探每个合格账号的保活连接。
    pub(crate) async fn run_cycle(self: &Arc<Self>) {
        let settings = self.settings();
        self.pool
            .set_warm_reuse(settings.enabled && settings.business_reuse);
        if !settings.enabled {
            return;
        }
        let accounts = match self.repository.list_for_provider().await {
            Ok(accounts) => accounts,
            Err(_) => return,
        };
        let now = Instant::now();
        for account in accounts {
            if !Self::eligible(&account) {
                continue;
            }
            let id = account.id().as_str().to_owned();
            let want = settings.connections_per_account as usize;
            // 池是「哪些 slot 有活连接」的唯一真相（可能被业务领养/被 evict 掉）。
            // 先在池锁外取占用序号，再进 warmer 状态锁，避免同时持两把锁。
            let occupied = self.pool.warm_slots_for_account(&id);

            // 挑一个要开/要复探的 slot（open 用空闲序号、reprobe 用已占用且到期的序号）。
            let (slot, reason) = {
                let mut state = Self::lock_state(&self.state);
                let entry = state.accounts.entry(id.clone()).or_default();
                // 清理已不再占用的 slot 的复探时间戳（被领养/evict 的连接）。
                entry.verified_at.retain(|slot, _| occupied.contains(slot));
                if entry.in_flight {
                    continue;
                }
                if entry.cooldown_until.is_some_and(|until| until > now) {
                    continue;
                }
                let imported_recently = entry
                    .imported_at
                    .is_some_and(|at| at.elapsed() < IMPORT_BURST_WINDOW);
                let recent_traffic = entry
                    .last_request
                    .is_some_and(|at| at.elapsed() < ACTIVE_WINDOW);
                let active = imported_recently || recent_traffic;
                let pick = if occupied.len() < want {
                    // 补一条：挑第一个**空闲**序号（不是用计数当序号，否则会反复命中已占用的 slot）。
                    // 活跃/刚导入的账号补，首次见到的启用账号也 bootstrap 一条。
                    (active || !entry.bootstrapped)
                        .then(|| (0..want).find(|slot| !occupied.contains(slot)))
                        .flatten()
                        .map(|slot| (slot, "open"))
                } else if settings.probe && active {
                    // 补满了：挑一个**已占用**且到复探点的序号，在其上复用同一条连接复探。
                    occupied
                        .iter()
                        .find(|slot| {
                            entry
                                .verified_at
                                .get(slot)
                                .is_none_or(|at| at.elapsed() >= settings.reprobe())
                        })
                        .map(|slot| (*slot, "reprobe"))
                } else {
                    None
                };
                match pick {
                    Some(pick) => {
                        entry.in_flight = true;
                        if pick.1 == "open" {
                            entry.bootstrapped = true;
                        }
                        pick
                    }
                    None => continue,
                }
            };

            // 全局在途上限：在途 + 进程内所有账号的保活连接总数。
            if self.in_flight_total.load(Ordering::Acquire) + self.pool.warm_len_total()
                >= settings.max_total_connections as usize
            {
                self.clear_in_flight(&id);
                continue;
            }
            let Some(permit) = self.pool.try_connect_permit() else {
                self.clear_in_flight(&id);
                continue;
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
                let _permit = permit;
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

    /// 为一个账号的一个 slot 开/复探一条保活连接。
    async fn warm_one(
        &self,
        account: ProviderAccount,
        slot: usize,
        reason: &'static str,
        settings: &WarmPoolSettings,
    ) {
        let id = account.id().as_str().to_owned();
        let outcome = self.warm_probe(&account, slot, settings).await;
        let now = Instant::now();
        let mut report = WarmReport {
            at: SystemTime::now(),
            held: 0,
            opened: reason == "open",
            verdict: None,
            served_model: None,
            error: None,
        };
        match outcome {
            Ok(Verdict::Verified(model)) => {
                report.verdict = Some("verified");
                report.served_model = model;
                if let Ok(mut state) = self.state.lock() {
                    let entry = state.accounts.entry(id.clone()).or_default();
                    entry.verified_at.insert(slot, now);
                }
            }
            Ok(Verdict::Degraded(model)) => {
                report.verdict = Some("degraded");
                report.served_model = model;
                // 降智：关掉该账号所有保活连接，别让业务领养；冷却，别空转续开。
                let closed = self.pool.evict_warm_for_account(&id).await;
                tracing::info!(
                    target: "ws_warm",
                    account_id = %id,
                    closed,
                    "[ws-warm] degraded connection evicted"
                );
                self.set_cooldown(&id, settings.cooldown());
            }
            Ok(Verdict::Failed(code)) => {
                report.verdict = Some("failed");
                report.error = Some(code);
                self.set_cooldown(&id, settings.cooldown().min(Duration::from_secs(120)));
            }
            Err(error) => {
                report.error = Some(error);
                self.set_cooldown(&id, settings.cooldown().min(Duration::from_secs(120)));
            }
        }
        report.held = self.pool.warm_len_for_account(&id);
        if let Ok(mut state) = self.state.lock() {
            let entry = state.accounts.entry(id).or_default();
            entry.last = Some(report);
        }
        // in_flight 的清除交给 WarmInFlight 守卫（任务结束/panic 都清），这里不动。
    }

    fn set_cooldown(&self, account_id: &str, cooldown: Duration) {
        if let Ok(mut state) = self.state.lock() {
            let entry = state.accounts.entry(account_id.to_owned()).or_default();
            entry.cooldown_until = Some(Instant::now() + cooldown);
        }
    }

    /// 发一条 canary 请求（WS 强制、store=false），落进保活 key、读答案判满血。
    async fn warm_probe(
        &self,
        account: &ProviderAccount,
        slot: usize,
        settings: &WarmPoolSettings,
    ) -> Result<Verdict, String> {
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
        let client = self
            .client
            .for_account(account)
            .map_err(|_| "client".to_owned())?
            .with_authentication(&credential.authentication);

        let model = self.probe_model(settings);
        let prompt = if settings.probe_prompt.trim().is_empty() {
            DEFAULT_PROBE_PROMPT
        } else {
            settings.probe_prompt.as_str()
        };
        let mut body = Map::new();
        body.insert("model".to_owned(), Value::String(model));
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
        context.cookie_header = cookie_ref;

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
            .map_err(|error| error.to_string())?;
        self.read_verdict(response, settings.probe_expect.as_str(), timeout)
            .await
    }

    /// 读上游 SSE 流，只累计文本增量到能判满血为止；不记明文答案。
    async fn read_verdict(
        &self,
        response: CodexBackendStreamingResponse,
        expect: &str,
        timeout: Duration,
    ) -> Result<Verdict, String> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut body = response.body;
        let mut buf: Vec<u8> = Vec::new();
        let mut answer = String::new();
        let mut served_model: Option<String> = None;
        loop {
            let next = match tokio::time::timeout_at(deadline, body.next()).await {
                Ok(Some(Ok(chunk))) => chunk,
                Ok(Some(Err(_))) => break,
                Ok(None) => break,
                Err(_) => return Ok(Verdict::Failed("timeout".to_owned())),
            };
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
                match event.get("type").and_then(Value::as_str) {
                    Some("response.output_text.delta") => {
                        if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                            answer.push_str(delta);
                        }
                    }
                    Some("response.completed") => {
                        served_model = event
                            .pointer("/response/model")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        return Ok(Self::judge(&answer, expect, served_model));
                    }
                    Some("response.failed") | Some("error") => {
                        let code = event
                            .pointer("/error/code")
                            .or_else(|| event.pointer("/response/error/code"))
                            .and_then(Value::as_str)
                            .unwrap_or("upstream_error")
                            .to_owned();
                        return Ok(Verdict::Failed(code));
                    }
                    _ => {}
                }
            }
            // 答案够判就早停（省得等完整长文本）。
            if answer.trim().len() >= expect.len() && !answer.trim().is_empty() {
                return Ok(Self::judge(&answer, expect, served_model));
            }
        }
        if answer.trim().is_empty() {
            Ok(Verdict::Failed("no_answer".to_owned()))
        } else {
            Ok(Self::judge(&answer, expect, served_model))
        }
    }

    fn judge(answer: &str, expect: &str, served_model: Option<String>) -> Verdict {
        let trimmed = answer.trim_start_matches(|c: char| c == '*' || c.is_whitespace());
        if trimmed.starts_with(expect) {
            Verdict::Verified(served_model)
        } else {
            Verdict::Degraded(served_model)
        }
    }

    fn probe_model(&self, settings: &WarmPoolSettings) -> String {
        if !settings.probe_model.trim().is_empty() {
            return settings.probe_model.clone();
        }
        if let Some(first) = settings.models.first() {
            return first.clone();
        }
        DEFAULT_WARM_MODEL.to_owned()
    }
}
