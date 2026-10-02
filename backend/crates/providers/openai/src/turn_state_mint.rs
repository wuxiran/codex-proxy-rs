//! 云端打票：账号缺票时铸票，把每模型的票钉成账号级模板、把路由 cookie 对
//! （`__cflb`/`__oailb`，内嵌目标网关 `unified-N`）写进凭据，让后续业务请求带着 pair 和票
//! 钉在目标网关上；票约 240s 可用，按业务需求补充。
//!
//! 两种后端：
//! - `native`（默认）：cpr 自己经**专用动态代理**向上游发 codex ping 铸票，验收票长、
//!   网关、模型声明（照 `deploy/cloud-mint/index.js` 的 `mintAttemptAccepted`）。
//! - `relay`：交给 `deploy/cloud-mint` 的 relay（阿里 FC 或 89 上的容器），出口是 relay 的。
//!
//! 触发：请求侧钩子发现桶里没票（`PinAttempt::needs_template`）→ 异步预热一次；
//! 后台每 20s 检查活跃账号的路由对是否需要更新，票据按需补充。
//! 账号级去重：同一账号同一时刻只有一次打票在途，失败后冷却 `cooldown_seconds`。
//! 票值、cookie 值、relay 密钥永不进日志。

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

use base64::Engine as _;
use chrono::{DateTime, Utc};
use gateway_core::account::{CredentialRevision, ProviderAccount, ProviderAccountId};
use reqwest::header::{ACCEPT, CONTENT_TYPE, COOKIE, HeaderValue};
use secrecy::ExposeSecret as _;
use serde::Deserialize;
use turn_state::{CloudMintSettings, MintMode};

use crate::credential::{
    CODEX_AUTHENTICATION_KIND_OAUTH, CodexCookie, CodexCredentialRepository,
    CodexRuntimeAuthentication, CredentialRepositoryError,
};
use crate::route_pair::RoutePairRef;
use crate::transport::profile::CodexWireProfileState;

const RELAY_TIMEOUT: Duration = Duration::from_secs(150);
const MAX_RELAY_RESPONSE_BYTES: usize = 512 * 1024;
/// 最近有请求的账号才续打；闲置账号不烧配额。
const ACTIVE_WINDOW: Duration = Duration::from_secs(600);
/// 路由 pair 剩余不足这么久就续。票不定时续：过期后由下一个业务请求按需打。
const PAIR_RENEW_MARGIN: Duration = Duration::from_secs(120);
/// 原生打票：只扫响应体前 16 KiB 找 `response.created` 的模型声明，拿到判决就拆流。
const NATIVE_BODY_SCAN_BYTES: usize = 16 * 1024;
const NATIVE_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(60);
const NATIVE_MAX_TOTAL_ATTEMPTS: u64 = 24;
const NATIVE_TOTAL_TIMEOUT: Duration = Duration::from_secs(75);
const NATIVE_ATTEMPT_GAP: Duration = Duration::from_millis(1500);
/// 多账号同时缺票时仍共享预算，避免每个账号各自启动一组代理连接。
const MAX_CONCURRENT_MINTS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum MintError {
    #[error("cloud mint is disabled")]
    Disabled,
    #[error("a dedicated mint proxy is required")]
    ProxyRequired,
    #[error("dedicated mint proxy is invalid")]
    InvalidProxy,
    #[error("account is not eligible for cloud mint")]
    NotEligible,
    #[error("a mint for this account is already in flight")]
    Busy,
    #[error("account is cooling down after a failed mint")]
    CoolingDown,
    #[error("mint backend is unreachable")]
    Unreachable,
    #[error("no acceptable ticket was minted")]
    Rejected,
    #[error("mint response is invalid")]
    InvalidResponse,
    #[error("credential store is unavailable")]
    Store,
    #[error("account credentials or route changed while minting")]
    Stale,
}

/// 一次打票的结果摘要；不含票值和 cookie 值。
#[derive(Debug, Clone)]
pub(crate) struct MintReport {
    pub(crate) at: SystemTime,
    pub(crate) ok: bool,
    pub(crate) observe_only: bool,
    pub(crate) gateway: Option<String>,
    pub(crate) attempts: u64,
    pub(crate) tickets: Vec<MintedTicket>,
    pub(crate) pair_written: bool,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct MintedTicket {
    pub(crate) model: String,
    pub(crate) length: usize,
    pub(crate) served_model: Option<String>,
    pub(crate) expires_at: SystemTime,
}

/// 两种后端归一化后的产出：票与签发它们的完整 pair。
struct MintOutcome {
    gateway: Option<String>,
    /// 包括沿用的 seed pair；缺少完整 pair 的票不能发布。
    pair: Option<RoutePair>,
    attempts: u64,
    tickets: Vec<RawTicket>,
}

struct RawTicket {
    model: String,
    value: String,
    served_model: Option<String>,
    issued_at: SystemTime,
    ttl: Duration,
}

#[derive(Clone)]
struct RoutePair {
    cflb: String,
    oailb: String,
    gateway: Option<String>,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct RelayResponse {
    gateway: Option<String>,
    cookies: Option<BTreeMap<String, String>>,
    expires_at: Option<String>,
    #[serde(default)]
    attempts: u64,
    #[serde(default)]
    tickets: BTreeMap<String, RelayTicket>,
    error: Option<RelayError>,
}

#[derive(Deserialize)]
struct RelayTicket {
    turn_state: String,
    served_model: Option<String>,
    issued_at: Option<String>,
    expires_at: Option<String>,
}

#[derive(Deserialize)]
struct RelayError {
    message: Option<String>,
    code: Option<String>,
}

struct ActiveAccount {
    models: BTreeSet<String>,
    last_request: Instant,
}

#[derive(Default)]
struct State {
    in_flight: HashSet<String>,
    cooldown_until: HashMap<String, Instant>,
    active: HashMap<String, ActiveAccount>,
    last: HashMap<String, MintReport>,
}

/// 预热开始时冻结发布前提，避免失败候选或过时凭据覆盖正在使用的路由。
pub(crate) struct WarmPublication {
    account: ProviderAccount,
    binding: String,
    original_route: Option<RoutePairRef>,
    seed: Option<RoutePair>,
    original_cookies: Vec<CodexCookie>,
}

/// 不实现 Debug：持有仓库句柄，且报告里有网关名以外的敏感上下文。
pub(crate) struct CloudMintService {
    repository: CodexCredentialRepository,
    pins: crate::turn_state_pin::TurnStatePins,
    profile: CodexWireProfileState,
    /// 原生打票直接打的上游地址（与业务请求同一个 base_url）。
    base_url: String,
    /// pair cookie 写进凭据时用的 domain：生产是 `chatgpt.com`，本机联调假上游时是回环地址。
    cookie_domain: String,
    /// 生产上游是 https，pair 按 Secure 写；本机 http 假上游时不能标 Secure，否则不会回放。
    cookie_secure: bool,
    state: Mutex<State>,
    relay_client: Mutex<Option<(String, reqwest::Client)>>,
}

impl CloudMintService {
    pub(crate) async fn warm_publication(
        &self,
        account_id: &str,
    ) -> Result<WarmPublication, MintError> {
        let (account, binding) = self.load_binding(account_id).await?;
        let data = self
            .repository
            .load_complete_data(&account)
            .await
            .map_err(|_| MintError::Store)?;
        let original_route = RoutePairRef::stored(data.cookies());
        let seed = self.stored_pair(&account, &self.settings()).await;
        Ok(WarmPublication {
            account,
            binding,
            original_route,
            seed,
            original_cookies: data
                .cookies()
                .iter()
                .filter(|cookie| is_root_route_cookie(cookie))
                .cloned()
                .collect(),
        })
    }

    /// 仅在完整探针通过后提交同一连接的路由和当前模型票；调用方随后才可发布连接。
    pub(crate) async fn publish_warm(
        &self,
        snapshot: &WarmPublication,
        headers: &[String],
        actual_route: &RoutePairRef,
        ticket: Option<(&str, &str)>,
    ) -> Result<(), MintError> {
        let settings = self.pins.service().settings();
        if settings.dry_run
            || !settings.warm_pool.enabled
            || !settings.warm_pool.business_reuse
            || settings.cloud_mint.observe_only
        {
            return Err(MintError::Disabled);
        }
        // 同路由复探可以不签新票；换路由则必须连同新票发布，不能留下旧路由的票。
        if ticket.is_none() && snapshot.original_route.as_ref() != Some(actual_route) {
            return Err(MintError::InvalidResponse);
        }
        let pair = RoutePair::from_set_cookie(headers)
            .or_else(|| snapshot.seed.clone())
            .ok_or(MintError::InvalidResponse)?;
        if !pair.live(mint_target(&settings.cloud_mint.gateway).as_deref())
            || RoutePairRef::of(&pair.cflb, &pair.oailb).as_ref() != Some(actual_route)
        {
            return Err(MintError::InvalidResponse);
        }
        let now = SystemTime::now();
        let mut pin = ticket.map(|(model, value)| turn_state::AccountWidePin {
            account: snapshot.account.id().as_str(),
            binding: snapshot.binding.clone(),
            model,
            egress: crate::turn_state_pin::egress_fingerprint(
                snapshot
                    .account
                    .outbound_proxy()
                    .map(|proxy| proxy.expose_url()),
            ),
            value,
            captured_at: now,
            now,
            source: turn_state::Source::Mint,
            ttl: Some(settings.cloud_mint.ticket_ttl()),
            gateway: pair.gateway.clone(),
        });
        if let Some(pin) = &pin {
            self.pins
                .service()
                .validate_account_wide(pin)
                .map_err(|_| MintError::InvalidResponse)?;
        }
        let (account, written_revision) = self
            .write_route_pair(
                &snapshot.account,
                &snapshot.binding,
                snapshot.original_route.as_ref(),
                &pair,
            )
            .await?;
        let current = self.pins.service().settings();
        let outcome = if current.dry_run
            || current.warm_pool != settings.warm_pool
            || current.cloud_mint != settings.cloud_mint
        {
            Err(MintError::Disabled)
        } else if let Some(mut pin) = pin.take() {
            pin.egress = crate::turn_state_pin::egress_fingerprint(
                account.outbound_proxy().map(|proxy| proxy.expose_url()),
            );
            // CAS 可能耗时，重新检查此刻票是否已经到期。
            pin.now = SystemTime::now();
            self.pins
                .service()
                .pin_minted_account_wide(pin, &actual_route.fingerprint)
                .map(|_| ())
                .map_err(|_| MintError::InvalidResponse)
        } else {
            Ok(())
        };
        if outcome.is_err()
            && let Some(revision) = written_revision
        {
            self.restore_warm_route(snapshot, actual_route, revision)
                .await?;
        }
        outcome
    }

    /// 只撤回本次 CAS 写入，凭据或路由已经被其他请求更新时让步。
    async fn restore_warm_route(
        &self,
        snapshot: &WarmPublication,
        published: &RoutePairRef,
        revision: CredentialRevision,
    ) -> Result<(), MintError> {
        let current = self.load_account(snapshot.account.id().as_str()).await?;
        if current.revision() != revision {
            return Ok(());
        }
        let mut data = self
            .repository
            .load_complete_data(&current)
            .await
            .map_err(|_| MintError::Store)?;
        if RoutePairRef::stored(data.cookies()).as_ref() != Some(published) {
            return Ok(());
        }
        let cookies = data.cookies_mut().ok_or(MintError::NotEligible)?;
        cookies.retain(|cookie| !is_root_route_cookie(cookie));
        cookies.extend(snapshot.original_cookies.iter().cloned());
        match self.repository.compare_and_swap_data(&current, data).await {
            Ok(_) | Err(CredentialRepositoryError::RevisionConflict) => Ok(()),
            Err(_) => Err(MintError::Store),
        }
    }

    pub(crate) fn new(
        repository: CodexCredentialRepository,
        pins: crate::turn_state_pin::TurnStatePins,
        profile: CodexWireProfileState,
        base_url: &str,
    ) -> Self {
        let parsed = url::Url::parse(base_url).ok();
        let cookie_secure = parsed.as_ref().is_some_and(|url| url.scheme() == "https");
        let cookie_domain = parsed
            .as_ref()
            .and_then(|url| {
                let host = url.host_str()?.to_owned();
                let loopback = host == "localhost"
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback());
                loopback.then_some(host)
            })
            .unwrap_or_else(|| "chatgpt.com".to_owned());
        Self {
            repository,
            pins,
            profile,
            base_url: base_url.to_owned(),
            cookie_domain,
            cookie_secure,
            state: Mutex::new(State::default()),
            relay_client: Mutex::new(None),
        }
    }

    fn settings(&self) -> CloudMintSettings {
        self.pins.service().settings().cloud_mint
    }

    pub(crate) fn enabled(&self) -> bool {
        self.settings().enabled && !self.pins.service().settings().dry_run
    }

    /// 请求侧：记下这个账号在用哪些模型，续打只照顾最近活跃的账号。
    pub(crate) fn note_request(&self, account_id: &str, model: &str) {
        if let Ok(mut state) = self.state.lock() {
            let entry = state
                .active
                .entry(account_id.to_owned())
                .or_insert_with(|| ActiveAccount {
                    models: BTreeSet::new(),
                    last_request: Instant::now(),
                });
            entry.models.insert(model.to_owned());
            entry.last_request = Instant::now();
        }
    }

    /// 缺票预热：后台打一次，不阻塞当前请求（当前请求照常裸发）。
    pub(crate) fn prefetch(self: &Arc<Self>, account_id: &str, model: &str) {
        if !self.enabled()
            || self
                .pins
                .service()
                .settings()
                .warm_pool
                .requires_verified_connections()
        {
            return;
        }
        let this = Arc::clone(self);
        let account_id = account_id.to_owned();
        let model = model.to_owned();
        tokio::spawn(async move {
            match this.mint_account(&account_id, vec![model]).await {
                Ok(_) | Err(MintError::Busy | MintError::CoolingDown | MintError::Disabled) => {}
                Err(error) => tracing::info!(
                    target: "turn_state",
                    account_id = account_id.as_str(),
                    error = %error,
                    "[turn-state] mint prefetch failed"
                ),
            }
        });
    }

    pub(crate) fn last_report(&self, account_id: &str) -> Option<MintReport> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.last.get(account_id).cloned())
    }

    /// 后台续 pair：最近有流量的账号，路由 pair 缺失或剩余不足两分钟时裸打一次换一对新的
    /// （顺带得到的票照常钉住）。票本身不在这里续。
    pub(crate) async fn renew_cycle(&self) -> usize {
        let settings = self.settings();
        if !self.enabled()
            || self
                .pins
                .service()
                .settings()
                .warm_pool
                .requires_verified_connections()
        {
            return 0;
        }
        let due: Vec<(String, Vec<String>)> = {
            let Ok(mut state) = self.state.lock() else {
                return 0;
            };
            state
                .active
                .retain(|_, active| active.last_request.elapsed() < ACTIVE_WINDOW);
            state
                .active
                .iter()
                .map(|(account, active)| {
                    let mut models: BTreeSet<String> = active.models.clone();
                    models.extend(settings.models.iter().cloned());
                    (account.clone(), models.into_iter().collect())
                })
                .collect()
        };
        let mut minted = 0;
        for (account_id, models) in due {
            if models.is_empty() || !self.pair_needs_renewal(&account_id, &settings).await {
                continue;
            }
            if self.mint(&account_id, models, true).await.is_ok() {
                minted += 1;
            }
        }
        minted
    }

    /// 凭据里没有目标网关上的 pair，或它将在 [`PAIR_RENEW_MARGIN`] 内到期。
    async fn pair_needs_renewal(&self, account_id: &str, settings: &CloudMintSettings) -> bool {
        let Ok((account, _)) = self.load_binding(account_id).await else {
            return false;
        };
        self.stored_pair(&account, settings)
            .await
            .is_none_or(|pair| {
                pair.expires_at.is_some_and(|expires| {
                    expires
                        <= Utc::now()
                            + chrono::Duration::from_std(PAIR_RENEW_MARGIN).unwrap_or_default()
                })
            })
    }

    async fn load_account(&self, account_id: &str) -> Result<ProviderAccount, MintError> {
        let id =
            ProviderAccountId::new(account_id.to_owned()).map_err(|_| MintError::NotEligible)?;
        let account = self
            .repository
            .store()
            .get_account(&id)
            .await
            .map_err(|_| MintError::Store)?
            .ok_or(MintError::NotEligible)?;
        if account.provider().as_str() != "openai"
            || account.authentication_kind() != CODEX_AUTHENTICATION_KIND_OAUTH
            || !account.enabled()
        {
            return Err(MintError::NotEligible);
        }
        Ok(account)
    }

    /// 账号 + 当前凭据绑定；没开「固定自身 state」的账号不打票（打了也没处注入）。
    async fn load_binding(&self, account_id: &str) -> Result<(ProviderAccount, String), MintError> {
        let account = self.load_account(account_id).await?;
        let credential = self
            .repository
            .load_runtime_credential(&account)
            .await
            .map_err(|_| MintError::NotEligible)?;
        let generation = credential
            .turn_state_pin
            .as_deref()
            .ok_or(MintError::NotEligible)?;
        let CodexRuntimeAuthentication::OAuth(secret) = &credential.authentication else {
            return Err(MintError::NotEligible);
        };
        let binding = crate::turn_state_pin::credential_binding(
            generation,
            secret.access_token.expose_secret(),
        );
        Ok((account, binding))
    }

    fn relay_client(&self, proxy_url: &str) -> Result<reqwest::Client, MintError> {
        let mut cached = self
            .relay_client
            .lock()
            .map_err(|_| MintError::Unreachable)?;
        if let Some((key, client)) = cached.as_ref()
            && key == proxy_url
        {
            return Ok(client.clone());
        }
        let mut builder = reqwest::Client::builder().timeout(RELAY_TIMEOUT);
        builder = if proxy_url.is_empty() {
            builder.no_proxy()
        } else {
            builder.proxy(reqwest::Proxy::all(proxy_url).map_err(|_| MintError::Unreachable)?)
        };
        let client = builder.build().map_err(|_| MintError::Unreachable)?;
        *cached = Some((proxy_url.to_owned(), client.clone()));
        Ok(client)
    }

    /// 打一次票：为 `models` 铸票，钉住并写 pair。同账号并发去重、失败冷却。
    pub(crate) async fn mint_account(
        &self,
        account_id: &str,
        models: Vec<String>,
    ) -> Result<MintReport, MintError> {
        self.mint(account_id, models, false).await
    }

    /// `fresh_pair`：不带既有 pair 裸打，让边缘重新分配节点并签发一对新的（续 pair 用）。
    /// relay 侧的 pair 缓存在 relay 进程里，靠 `x-mint-fresh-pair: 1` 跳过并删掉那条缓存。
    async fn mint(
        &self,
        account_id: &str,
        models: Vec<String>,
        fresh_pair: bool,
    ) -> Result<MintReport, MintError> {
        let settings = self.settings();
        if !self.enabled() {
            return Err(MintError::Disabled);
        }
        let models: Vec<String> = if settings.models.is_empty() {
            models
        } else {
            settings.models.clone()
        };
        if models.is_empty() {
            return Err(MintError::NotEligible);
        }
        let _guard = InFlight::acquire(self, account_id)?;
        let outcome = self
            .mint_inner(account_id, &models, &settings, fresh_pair)
            .await;
        let report = match &outcome {
            Ok(report) => report.clone(),
            Err(error) => MintReport {
                at: SystemTime::now(),
                ok: false,
                observe_only: settings.observe_only,
                gateway: None,
                attempts: 0,
                tickets: Vec::new(),
                pair_written: false,
                error: Some(error.to_string()),
            },
        };
        if let Ok(mut state) = self.state.lock() {
            if !report.ok {
                state
                    .cooldown_until
                    .insert(account_id.to_owned(), Instant::now() + settings.cooldown());
            }
            state.last.insert(account_id.to_owned(), report);
        }
        outcome
    }

    async fn mint_inner(
        &self,
        account_id: &str,
        models: &[String],
        settings: &CloudMintSettings,
        fresh_pair: bool,
    ) -> Result<MintReport, MintError> {
        let (account, binding) = self.load_binding(account_id).await?;
        let original_data = self
            .repository
            .load_complete_data(&account)
            .await
            .map_err(|_| MintError::Store)?;
        let original_route = RoutePairRef::stored(original_data.cookies());
        let credential = self
            .repository
            .load_runtime_credential(&account)
            .await
            .map_err(|_| MintError::NotEligible)?;
        let authorization = credential
            .authentication
            .authorization_header()
            .map_err(|_| MintError::NotEligible)?;
        let outcome = match settings.mode {
            MintMode::Relay => {
                self.mint_via_relay(
                    &account,
                    authorization.expose_secret(),
                    models,
                    settings,
                    fresh_pair,
                )
                .await?
            }
            MintMode::Native => {
                let seed = if fresh_pair {
                    None
                } else {
                    self.stored_pair(&account, settings).await
                };
                self.mint_native(
                    &account,
                    authorization.expose_secret(),
                    models,
                    settings,
                    seed,
                )
                .await?
            }
        };
        let gateway = outcome
            .gateway
            .clone()
            .or_else(|| outcome.pair.as_ref().and_then(|pair| pair.gateway.clone()));
        let pair = outcome.pair.as_ref().ok_or(MintError::InvalidResponse)?;
        if outcome.tickets.is_empty() || !pair.live(mint_target(&settings.gateway).as_deref()) {
            return Err(MintError::Rejected);
        }
        let route = RoutePairRef::of(&pair.cflb, &pair.oailb).ok_or(MintError::InvalidResponse)?;
        let now = SystemTime::now();
        let current_settings = self.pins.service().settings();
        let observe_only = settings.observe_only
            || current_settings.cloud_mint.observe_only
            || current_settings.dry_run
            || !current_settings.cloud_mint.enabled;
        // 先条件提交路由，再发布绑定该路由的票。并发请求握着旧凭据快照时不能读到新路由票。
        let (account, pair_written) = if observe_only {
            (account, None)
        } else {
            self.write_route_pair(&account, &binding, original_route.as_ref(), pair)
                .await?
        };
        let pair_written = pair_written.is_some();
        // 专用打票代理不修改账号的业务出口绑定。
        let egress = crate::turn_state_pin::egress_fingerprint(
            account.outbound_proxy().map(|proxy| proxy.expose_url()),
        );
        let mut tickets = Vec::new();
        for ticket in &outcome.tickets {
            if observe_only {
                tickets.push(MintedTicket {
                    model: ticket.model.clone(),
                    length: ticket.value.len(),
                    served_model: ticket.served_model.clone(),
                    expires_at: ticket.issued_at + ticket.ttl,
                });
                continue;
            }
            match self.pins.service().pin_minted_account_wide(
                turn_state::AccountWidePin {
                    account: account.id().as_str(),
                    binding: binding.clone(),
                    model: &ticket.model,
                    egress: egress.clone(),
                    value: &ticket.value,
                    captured_at: ticket.issued_at,
                    now,
                    source: turn_state::Source::Mint,
                    ttl: Some(ticket.ttl),
                    gateway: gateway.clone(),
                },
                &route.fingerprint,
            ) {
                Ok(effective) => tickets.push(MintedTicket {
                    model: ticket.model.clone(),
                    length: effective.length,
                    served_model: ticket.served_model.clone(),
                    expires_at: effective.expires_at,
                }),
                Err(rejected) => tracing::info!(
                    target: "turn_state",
                    account_id,
                    model = ticket.model.as_str(),
                    len = ticket.value.len(),
                    ?rejected,
                    "[turn-state] minted ticket not pinned"
                ),
            }
        }
        let report = MintReport {
            at: now,
            ok: !tickets.is_empty(),
            observe_only,
            gateway: gateway.clone(),
            attempts: outcome.attempts,
            tickets,
            pair_written,
            error: None,
        };
        tracing::info!(
            target: "turn_state",
            account_id,
            mode = ?settings.mode,
            gateway = gateway.as_deref().unwrap_or("-"),
            tickets = report.tickets.len(),
            attempts = report.attempts,
            pair_written,
            observe_only = observe_only,
            "[turn-state] mint"
        );
        if report.ok {
            Ok(report)
        } else {
            Err(MintError::Rejected)
        }
    }

    // ---- relay 后端 --------------------------------------------------------

    async fn mint_via_relay(
        &self,
        account: &ProviderAccount,
        authorization: &str,
        models: &[String],
        settings: &CloudMintSettings,
        fresh_pair: bool,
    ) -> Result<MintOutcome, MintError> {
        let client = self.relay_client(&settings.proxy_url)?;
        let mut request = client
            .get(settings.relay_url.trim_end_matches('/').to_owned() + "/")
            .header("x-relay-key", settings.relay_key.as_str())
            .header("x-relay-mint", "1")
            .header("x-mint-models", models.join(","))
            .header("x-mint-len", settings.ticket_len.to_string())
            .header("x-mint-ttl", settings.ticket_ttl_seconds.to_string())
            .header("x-mint-transport", settings.transport.as_str())
            .header("authorization", authorization);
        if fresh_pair {
            // relay 自己的 pair 缓存不看调用方是否还握着旧 pair。带上这个头才会裸打。
            request = request.header("x-mint-fresh-pair", "1");
        }
        if !settings.gateway.trim().is_empty() {
            request = request.header("x-mint-gateway", settings.gateway.trim());
        }
        if let Some(upstream_account_id) = account.upstream_account_id() {
            request = request.header("chatgpt-account-id", upstream_account_id);
        }
        let started = SystemTime::now();
        let response = request.send().await.map_err(|_| MintError::Unreachable)?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(|_| MintError::Unreachable)?;
        if bytes.len() > MAX_RELAY_RESPONSE_BYTES {
            return Err(MintError::InvalidResponse);
        }
        let parsed: RelayResponse =
            serde_json::from_slice(&bytes).map_err(|_| MintError::InvalidResponse)?;
        if !status.is_success() {
            let error = parsed.error.as_ref();
            tracing::warn!(
                target: "turn_state",
                account_id = account.id().as_str(),
                status = status.as_u16(),
                code = error.and_then(|e| e.code.as_deref()).unwrap_or("-"),
                message = error.and_then(|e| e.message.as_deref()).unwrap_or("-"),
                "[turn-state] mint rejected by relay"
            );
            return Err(MintError::Rejected);
        }
        let gateway = parsed.gateway.as_deref().and_then(safe_label);
        let pair = parsed.cookies.as_ref().and_then(|cookies| {
            let cflb = cookies.get("__cflb")?.clone();
            let oailb = cookies.get("__oailb")?.clone();
            (!cflb.is_empty() && !oailb.is_empty()).then(|| RoutePair {
                cflb,
                oailb,
                gateway: gateway.clone(),
                expires_at: parsed.expires_at.as_deref().and_then(parse_chrono),
            })
        });
        let tickets = parsed
            .tickets
            .iter()
            .filter(|(model, ticket)| {
                models.contains(model)
                    && ticket
                        .served_model
                        .as_deref()
                        .is_some_and(|served| served.eq_ignore_ascii_case(model))
                    && (settings.ticket_len == 0 || ticket.turn_state.len() == settings.ticket_len)
            })
            .map(|(model, ticket)| {
                let issued_at = ticket
                    .issued_at
                    .as_deref()
                    .and_then(parse_time)
                    .unwrap_or(started);
                let ttl = ticket
                    .expires_at
                    .as_deref()
                    .and_then(parse_time)
                    .and_then(|expires| expires.duration_since(issued_at).ok())
                    .filter(|ttl| !ttl.is_zero())
                    .unwrap_or_else(|| settings.ticket_ttl());
                RawTicket {
                    model: model.clone(),
                    value: ticket.turn_state.clone(),
                    served_model: ticket.served_model.clone(),
                    issued_at,
                    ttl,
                }
            })
            .collect();
        Ok(MintOutcome {
            gateway,
            pair,
            attempts: parsed.attempts,
            tickets,
        })
    }

    // ---- 原生后端：经专用动态代理打票 ------------------------------------------

    /// 凭据里已有的、未过期且在目标网关上的 pair；用作定向打的种子。
    async fn stored_pair(
        &self,
        account: &ProviderAccount,
        settings: &CloudMintSettings,
    ) -> Option<RoutePair> {
        let data = self.repository.load_complete_data(account).await.ok()?;
        let cookies = data.cookies();
        let find = |name: &str| {
            cookies
                .iter()
                .find(|cookie| cookie.name == name)
                .map(|cookie| cookie.value.clone())
        };
        let pair = RoutePair::from_values(find("__cflb")?, find("__oailb")?);
        pair.live(mint_target(&settings.gateway).as_deref())
            .then_some(pair)
    }

    async fn mint_native(
        &self,
        account: &ProviderAccount,
        authorization: &str,
        models: &[String],
        settings: &CloudMintSettings,
        seed: Option<RoutePair>,
    ) -> Result<MintOutcome, MintError> {
        // 打票与业务出口隔离，缺配置或代理失败时绝不回退直连或账号代理。
        let proxy = mint_proxy(&settings.upstream_proxy_url)?;
        let deadline = tokio::time::Instant::now() + NATIVE_TOTAL_TIMEOUT;
        let url = crate::transport::endpoints::endpoint_url(
            &self.base_url,
            crate::transport::endpoints::CODEX_RESPONSES_PATH,
        );
        let target = mint_target(&settings.gateway);
        let mut pair = seed;
        let mut tickets = Vec::new();
        let mut ticket_pair: Option<RoutePair> = None;
        let mut attempts = 0u64;
        for model in models {
            for _ in 0..settings.max_attempts {
                if !self.enabled()
                    || attempts >= NATIVE_MAX_TOTAL_ATTEMPTS
                    || tokio::time::Instant::now() >= deadline
                {
                    break;
                }
                if attempts > 0
                    && tokio::time::timeout_at(deadline, tokio::time::sleep(NATIVE_ATTEMPT_GAP))
                        .await
                        .is_err()
                {
                    break;
                }
                attempts += 1;
                // 有活的目标 pair 就定向打（票铸在该节点上）；没有就裸打让边缘重新分配。
                let steered = pair
                    .as_ref()
                    .filter(|pair| pair.live(target.as_deref()))
                    .cloned();
                // 每次尝试新建客户端，不复用上一次的 TCP 连接，动态出口由代理服务分配。
                let client = mint_http_client(&proxy)?;
                let attempt = tokio::time::timeout_at(
                    deadline,
                    self.native_attempt(
                        &client,
                        &url,
                        authorization,
                        account,
                        model,
                        steered.as_ref(),
                    ),
                )
                .await;
                let attempt = match attempt {
                    Ok(attempt) => attempt,
                    Err(_) => break,
                };
                let attempt = match attempt {
                    Ok(attempt) => attempt,
                    Err(MintError::Unreachable) => continue,
                    Err(error) => return Err(error),
                };
                if matches!(attempt.status, 401 | 403) {
                    tracing::warn!(
                        target: "turn_state",
                        account_id = account.id().as_str(),
                        status = attempt.status,
                        "[turn-state] upstream rejected the mint credentials"
                    );
                    return Err(MintError::Rejected);
                }
                if let Some(issued) = attempt.pair.clone() {
                    // 上游新签了 pair：在目标上就采纳并定向；不在目标上不入库，下一发回裸打。
                    if issued.live(target.as_deref()) {
                        pair = Some(issued);
                    } else {
                        pair = None;
                    }
                }
                if matches!(attempt.status, 400 | 404 | 422) || attempt.terminal_error {
                    break;
                }
                let node = if attempt.pair.is_some() {
                    attempt.pair.as_ref().and_then(|p| p.gateway.clone())
                } else {
                    steered.as_ref().and_then(|p| p.gateway.clone())
                };
                let accepted = attempt.status == 200
                    && attempt
                        .pair
                        .as_ref()
                        .or(steered.as_ref())
                        .is_some_and(|pair| pair.live(target.as_deref()))
                    && !attempt.ticket.is_empty()
                    && (settings.ticket_len == 0 || attempt.ticket.len() == settings.ticket_len)
                    && target
                        .as_deref()
                        .is_none_or(|wanted| node.as_deref() == Some(wanted))
                    && attempt
                        .served
                        .as_deref()
                        .is_some_and(|served| served.eq_ignore_ascii_case(model));
                tracing::info!(
                    target: "turn_state",
                    account_id = account.id().as_str(),
                    model = model.as_str(),
                    status = attempt.status,
                    len = attempt.ticket.len(),
                    steered = steered.is_some(),
                    node = node.as_deref().unwrap_or("-"),
                    served = attempt.served.as_deref().unwrap_or("-"),
                    accepted,
                    "[turn-state] mint attempt"
                );
                if !accepted && tickets.is_empty() {
                    pair = None;
                }
                if accepted {
                    let route = attempt.pair.as_ref().or(steered.as_ref()).cloned();
                    // 一批返回的票必须随同一对路由使用，动态出口换到新 pair 后不发布旧 pair 的票。
                    if !tickets.is_empty() && !same_mint_route(route.as_ref(), ticket_pair.as_ref())
                    {
                        tickets.clear();
                    }
                    ticket_pair = route;
                    let issued_at =
                        turn_state::fernet::issued_at(&attempt.ticket).unwrap_or(attempt.started);
                    tickets.push(RawTicket {
                        model: model.clone(),
                        value: attempt.ticket,
                        served_model: attempt.served,
                        issued_at,
                        ttl: settings.ticket_ttl(),
                    });
                    break;
                }
            }
        }
        let gateway = ticket_pair
            .as_ref()
            .filter(|pair| pair.live(target.as_deref()))
            .and_then(|pair| pair.gateway.clone());
        Ok(MintOutcome {
            gateway,
            pair: ticket_pair,
            attempts,
            tickets,
        })
    }

    async fn native_attempt(
        &self,
        client: &reqwest::Client,
        url: &str,
        authorization: &str,
        account: &ProviderAccount,
        model: &str,
        steered: Option<&RoutePair>,
    ) -> Result<NativeAttempt, MintError> {
        let profile = self.profile.snapshot();
        let mut headers = crate::transport::headers::build_codex_model_headers(
            &profile,
            authorization,
            account.upstream_account_id(),
        )
        .map_err(|_| MintError::InvalidResponse)?;
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        // 2026-09-25 在 89 实测：带 `x-openai-internal-codex-residency: us` 时上游只签 `__cf_bm`，
        // 不签路由对 `__cflb/__oailb`；裸打去掉它才拿得到 pair（巴西家宽→unified-83，89 直连→unified-119）。
        headers.remove("x-openai-internal-codex-residency");
        if let Some(pair) = steered {
            headers.insert(
                COOKIE,
                HeaderValue::from_str(&format!("__cflb={}; __oailb={}", pair.cflb, pair.oailb))
                    .map_err(|_| MintError::InvalidResponse)?,
            );
        }
        let payload = serde_json::json!({
            "model": model,
            "instructions": "",
            "stream": true,
            "store": false,
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "ping"}]}],
            "reasoning": {"effort": "low"},
            "tool_choice": "auto",
            "parallel_tool_calls": false,
        });
        let started = SystemTime::now();
        let response = client
            .post(url)
            .headers(headers)
            .timeout(NATIVE_ATTEMPT_TIMEOUT)
            .json(&payload)
            .send()
            .await
            .map_err(|_| MintError::Unreachable)?;
        let status = response.status().as_u16();
        let ticket = response
            .headers()
            .get("x-codex-turn-state")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let set_cookie: Vec<String> = response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok().map(str::to_owned))
            .collect();
        let pair = RoutePair::from_set_cookie(&set_cookie);
        let mut body = Vec::new();
        let mut served = None;
        let mut terminal_error = false;
        let mut response = response;
        if status == 200 {
            while let Ok(Some(chunk)) = response.chunk().await {
                body.extend_from_slice(
                    &chunk[..chunk.len().min(NATIVE_BODY_SCAN_BYTES - body.len())],
                );
                match sse_decision(&body) {
                    SseDecision::Pending if body.len() < NATIVE_BODY_SCAN_BYTES => continue,
                    SseDecision::Pending => break,
                    SseDecision::Created(model) => {
                        served = Some(model);
                        break;
                    }
                    SseDecision::Error { terminal } => {
                        terminal_error = terminal;
                        break;
                    }
                }
            }
        }
        drop(response);
        Ok(NativeAttempt {
            status,
            ticket,
            pair,
            served,
            terminal_error,
            started,
        })
    }

    /// 只在凭据绑定及原路由仍匹配时发布；CAS 冲突重读一次并重新检查。
    async fn write_route_pair(
        &self,
        account: &ProviderAccount,
        expected_binding: &str,
        original_route: Option<&RoutePairRef>,
        pair: &RoutePair,
    ) -> Result<(ProviderAccount, Option<CredentialRevision>), MintError> {
        let desired =
            RoutePairRef::of(&pair.cflb, &pair.oailb).ok_or(MintError::InvalidResponse)?;
        let values = [("__cflb", &pair.cflb), ("__oailb", &pair.oailb)];
        for (_, value) in &values {
            if value.len() > 4096 || !value.bytes().all(|b| b.is_ascii_graphic() && b != b';') {
                return Err(MintError::InvalidResponse);
            }
        }
        for attempt in 0..2 {
            let current = self.load_account(account.id().as_str()).await?;
            let mut data = match self.repository.load_complete_data(&current).await {
                Ok(data) => data,
                Err(CredentialRepositoryError::RevisionConflict) if attempt == 0 => continue,
                Err(_) => return Err(MintError::Store),
            };
            let oauth = data.oauth().ok_or(MintError::NotEligible)?;
            let generation = oauth.turn_state_pin.as_deref().ok_or(MintError::Stale)?;
            if crate::turn_state_pin::credential_binding(generation, &oauth.access_token)
                != expected_binding
            {
                return Err(MintError::Stale);
            }
            let current_route = RoutePairRef::stored(data.cookies());
            if current_route.as_ref() == Some(&desired) {
                return Ok((current, None));
            }
            if current_route.as_ref() != original_route {
                return Err(MintError::Stale);
            }
            let stored = data.cookies_mut().ok_or(MintError::NotEligible)?;
            for (name, value) in &values {
                stored.retain(|cookie| !(cookie.name == *name && cookie.path == "/"));
                stored.push(CodexCookie {
                    name: (*name).to_owned(),
                    value: (*value).clone(),
                    domain: self.cookie_domain.clone(),
                    path: "/".to_owned(),
                    host_only: false,
                    secure: self.cookie_secure,
                    expires_at: pair.expires_at,
                });
            }
            match self.repository.compare_and_swap_data(&current, data).await {
                Ok(revision) => return Ok((current, Some(revision))),
                Err(CredentialRepositoryError::RevisionConflict) if attempt == 0 => {}
                Err(_) => return Err(MintError::Store),
            }
        }
        Err(MintError::Store)
    }
}

fn is_root_route_cookie(cookie: &CodexCookie) -> bool {
    cookie.path == "/" && matches!(cookie.name.as_str(), "__cflb" | "__oailb")
}

struct NativeAttempt {
    status: u16,
    ticket: String,
    pair: Option<RoutePair>,
    served: Option<String>,
    terminal_error: bool,
    started: SystemTime,
}

impl RoutePair {
    fn from_values(cflb: String, oailb: String) -> Self {
        let claims = jwt_claims(&oailb);
        let gateway = gateway_label(&cflb, &oailb);
        let expires_at = claims
            .as_ref()
            .and_then(|claims| claims.get("exp"))
            .and_then(serde_json::Value::as_i64)
            .and_then(|exp| DateTime::<Utc>::from_timestamp(exp, 0));
        Self {
            cflb,
            oailb,
            gateway,
            expires_at,
        }
    }

    fn from_set_cookie(headers: &[String]) -> Option<Self> {
        let mut cflb = None;
        let mut oailb = None;
        for header in headers {
            let (name, rest) = header.split_once('=')?;
            let value = rest.split(';').next().unwrap_or("").trim().to_owned();
            match name.trim().to_ascii_lowercase().as_str() {
                "__cflb" => cflb = Some(value),
                "__oailb" => oailb = Some(value),
                _ => {}
            }
        }
        let pair = Self::from_values(cflb?, oailb?);
        (!pair.cflb.is_empty() && !pair.oailb.is_empty()).then_some(pair)
    }

    /// 未过期，且（有目标时）在目标网关上。
    fn live(&self, target: Option<&str>) -> bool {
        let unexpired = self.expires_at.is_none_or(|expires| expires > Utc::now());
        unexpired && target.is_none_or(|wanted| self.gateway.as_deref() == Some(wanted))
    }
}

/// 目标网关写法规范化：空/`any`/`*` → 不查；`88`、`unified_88` → `unified-88`。
fn mint_target(raw: &str) -> Option<String> {
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() || value == "any" || value == "*" {
        return None;
    }
    if let Some(number) = value
        .strip_prefix("unified")
        .map(|rest| rest.trim_start_matches(['-', '_', '.']))
        .filter(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
    {
        return Some(format!("unified-{number}"));
    }
    if value.bytes().all(|b| b.is_ascii_digit()) {
        return Some(format!("unified-{value}"));
    }
    Some(value)
}

pub(crate) fn jwt_claims(token: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    let payload = token.split('.').nth(1)?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice::<serde_json::Value>(&raw)
        .ok()?
        .as_object()
        .cloned()
}

/// pair 的节点名：先查 `__oailb` 的 JWT 载荷，再退到两个值的明文；规范化成 `unified-N`。
pub(crate) fn gateway_label(cflb: &str, oailb: &str) -> Option<String> {
    let payload = jwt_claims(oailb)
        .map(|claims| serde_json::Value::Object(claims).to_string())
        .unwrap_or_default();
    [payload.as_str(), oailb, cflb]
        .into_iter()
        .find_map(find_gateway_label)
}

fn find_gateway_label(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let mut search = lower.as_str();
    while let Some(index) = search.find("unified") {
        let rest = search[index + "unified".len()..].trim_start_matches(['-', '_', '.']);
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() {
            return Some(format!("unified-{digits}"));
        }
        search = &search[index + "unified".len()..];
    }
    None
}

fn safe_label(value: &str) -> Option<String> {
    (!value.is_empty() && value.len() <= 64 && value.bytes().all(|b| b.is_ascii_graphic()))
        .then(|| value.to_owned())
}

enum SseDecision {
    Pending,
    Created(String),
    Error { terminal: bool },
}

/// 只认完整 SSE 事件里的 `response.created`（带 id 与 model）或 error/response.failed。
fn sse_decision(body: &[u8]) -> SseDecision {
    let text = String::from_utf8_lossy(body);
    let mut event_name = "";
    let mut data = String::new();
    let mut lines: Vec<&str> = text.split('\n').collect();
    lines.pop(); // 未终止的一行不能参与事件判定
    for line in lines {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            if !data.is_empty()
                && let Ok(event) = serde_json::from_str::<serde_json::Value>(&data)
            {
                let kind = event
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                if !event_name.is_empty() && event_name != kind {
                    event_name = "";
                    data.clear();
                    continue;
                }
                match kind {
                    "response.created" => {
                        let response = event.get("response");
                        let id = response
                            .and_then(|r| r.get("id"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("");
                        let model = response
                            .and_then(|r| r.get("model"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("");
                        if !id.trim().is_empty() && !model.trim().is_empty() {
                            return SseDecision::Created(model.to_owned());
                        }
                    }
                    "error" | "response.failed" => {
                        let error = event
                            .get("error")
                            .or_else(|| event.get("response").and_then(|r| r.get("error")))
                            .unwrap_or(&event);
                        let code = error
                            .get("code")
                            .or_else(|| error.get("type"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown");
                        let terminal = matches!(
                            code,
                            "invalid_request_error"
                                | "invalid_request"
                                | "invalid_argument"
                                | "model_not_found"
                                | "unsupported_model"
                                | "invalid_model"
                                | "authentication_error"
                                | "invalid_api_key"
                                | "permission_denied"
                                | "insufficient_quota"
                        );
                        return SseDecision::Error { terminal };
                    }
                    _ => {}
                }
            }
            event_name = "";
            data.clear();
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => event_name = value,
            "data" => {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value);
            }
            _ => {}
        }
    }
    SseDecision::Pending
}

/// 同账号打票在途标记；离开作用域即释放。
struct InFlight<'a> {
    service: &'a CloudMintService,
    account_id: String,
}

impl<'a> InFlight<'a> {
    fn acquire(service: &'a CloudMintService, account_id: &str) -> Result<Self, MintError> {
        let mut state = service.state.lock().map_err(|_| MintError::Busy)?;
        if state
            .cooldown_until
            .get(account_id)
            .is_some_and(|until| *until > Instant::now())
        {
            return Err(MintError::CoolingDown);
        }
        if state.in_flight.len() >= MAX_CONCURRENT_MINTS
            || !state.in_flight.insert(account_id.to_owned())
        {
            return Err(MintError::Busy);
        }
        Ok(Self {
            service,
            account_id: account_id.to_owned(),
        })
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.service.state.lock() {
            state.in_flight.remove(&self.account_id);
        }
    }
}

fn parse_chrono(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

fn parse_time(value: &str) -> Option<SystemTime> {
    parse_chrono(value).map(SystemTime::from)
}

/// 同一批票只能附带签发它们的路由对，失败探测的新 cookie 不能替换成功票的路由。
fn same_mint_route(left: Option<&RoutePair>, right: Option<&RoutePair>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.cflb == right.cflb && left.oailb == right.oailb,
        (None, None) => true,
        _ => false,
    }
}

/// 只认显式配置的专用出口，账号业务代理和环境代理都不能代替它。
fn mint_proxy(value: &str) -> Result<gateway_core::account::OutboundProxy, MintError> {
    if value.trim().is_empty() {
        return Err(MintError::ProxyRequired);
    }
    gateway_core::account::OutboundProxy::parse(value.trim()).map_err(|_| MintError::InvalidProxy)
}

fn mint_http_client(
    proxy: &gateway_core::account::OutboundProxy,
) -> Result<reqwest::Client, MintError> {
    let builder = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(proxy.expose_url()).map_err(|_| MintError::InvalidProxy)?)
        .redirect(reqwest::redirect::Policy::none())
        .pool_max_idle_per_host(0)
        .connect_timeout(Duration::from_secs(15));
    crate::transport::tls::build_reqwest_client_with_custom_ca(builder)
        .map_err(|_| MintError::Unreachable)
}

#[cfg(test)]
mod dedicated_proxy_tests {
    use super::{MintError, mint_http_client, mint_proxy};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn a_missing_or_invalid_mint_proxy_cannot_select_a_business_exit() {
        assert_eq!(mint_proxy("").unwrap_err(), MintError::ProxyRequired);
        assert_eq!(
            mint_proxy("file:///tmp/proxy").unwrap_err(),
            MintError::InvalidProxy
        );
        assert_eq!(
            mint_proxy("http://user:pass@localhost:0").unwrap_err(),
            MintError::InvalidProxy
        );
        assert!(mint_proxy("socks5h://example-user:example-password@proxy.example:1080").is_ok());
    }

    #[tokio::test]
    async fn consecutive_mint_attempts_open_new_connections_to_the_dedicated_proxy() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = mint_proxy(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            let mut connections = Vec::new();
            for _ in 0..2 {
                let (mut socket, _) =
                    tokio::time::timeout(Duration::from_secs(5), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut request = Vec::new();
                let mut part = [0; 1024];
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let n = socket.read(&mut part).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&part[..n]);
                }
                assert!(
                    request.starts_with(b"POST http://mint.invalid/codex/responses HTTP/1.1\r\n")
                );
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nOK",
                    )
                    .await
                    .unwrap();
                connections.push(socket);
            }
            connections.len()
        });
        for _ in 0..2 {
            let response = mint_http_client(&proxy)
                .unwrap()
                .post("http://mint.invalid/codex/responses")
                .send()
                .await
                .unwrap();
            assert_eq!(response.text().await.unwrap(), "OK");
        }
        assert_eq!(server.await.unwrap(), 2);
    }

    #[tokio::test]
    async fn a_failed_dedicated_proxy_does_not_fall_back_to_direct() {
        let upstream = wiremock::MockServer::start().await;
        let unavailable = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = mint_proxy(&format!("http://{}", unavailable.local_addr().unwrap())).unwrap();
        drop(unavailable);
        let result = mint_http_client(&proxy)
            .unwrap()
            .post(upstream.uri())
            .timeout(Duration::from_secs(2))
            .send()
            .await;
        assert!(result.is_err());
        assert!(upstream.received_requests().await.unwrap().is_empty());
    }
}
