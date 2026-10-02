//! 门面：宿主只和这里打交道。请求侧一个钩子（[`TurnStateService::begin_request`]），
//! 响应侧两个（[`Attempt::observe`] / [`Attempt::completed`]），其余是管理端读写。

use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use serde::Serialize;

use crate::{
    classify,
    decision::{self, Decision},
    fernet::{self, IssuedAtSource},
    observe::{ObservationInput, ObservationSnapshot, Observations},
    record::{BucketRecord, Source, TtlCaps, unix_seconds},
    served::ServedMatch,
    settings::{SetSettingsError, Settings, SettingsStore},
    store::{PinStore, Scope, StoreError},
};

/// 容忍的时钟偏差：票据签发时间超过 `now + skew` 视为伪造。
const FUTURE_SKEW: Duration = Duration::from_secs(120);
/// 缺票记录的条数上限；满了先丢最旧的。
const MAX_DEMANDS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TurnStateError {
    #[error("turn state storage is unavailable")]
    Io,
    #[error(transparent)]
    Settings(crate::settings::SettingsError),
}

/// 账号级钉住被拒的原因；不携带 state 值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinRejected {
    Length,
    Expired,
    FutureStamped,
    Full,
    Io,
}

/// 请求侧钩子的输入。
pub struct RequestFacts<'a> {
    pub account: &'a str,
    pub binding: String,
    pub model: &'a str,
    pub client: &'a str,
    /// 本次请求实际走的出口指纹。
    pub egress: &'a str,
    /// 客户端自带的 state（已进请求体/头的那个值）。
    pub carried: Option<&'a str>,
    pub now: SystemTime,
}

/// 管理员/续期钉住账号级模板的输入。
pub struct AccountWidePin<'a> {
    pub account: &'a str,
    pub binding: String,
    pub model: &'a str,
    pub egress: String,
    pub value: &'a str,
    pub captured_at: SystemTime,
    pub now: SystemTime,
    pub source: Source,
    /// 覆盖设置里的模板寿命（云端打票的票只有 ~240s）。
    pub ttl: Option<Duration>,
    /// 票所属网关节点名（云端打票时从路由 cookie 对解出）。
    pub gateway: Option<String>,
}

/// 展示用的 pin 状态；不含值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinStatus {
    pub model: String,
    pub length: usize,
    pub captured_at: SystemTime,
    pub issued_at: SystemTime,
    pub issued_at_source: IssuedAtSource,
    pub expires_at: SystemTime,
    pub hits: u64,
    pub account_wide: bool,
    pub source: Source,
    pub gateway: Option<String>,
}

/// 管理端桶列表的一行；不含值。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BucketSummary {
    pub account: String,
    pub model: String,
    pub scope: &'static str,
    pub len: usize,
    pub issued_at: u64,
    pub issued_at_source: IssuedAtSource,
    pub captured_at: u64,
    pub expires_at: u64,
    pub source: Source,
    /// 出口指纹前 12 位；直连或客户端级为 `None`。
    pub egress: Option<String>,
    pub hits: u64,
    pub gateway: Option<String>,
}

struct ServiceInner {
    store: PinStore,
    settings: SettingsStore,
    observations: Observations,
    /// 「账号 × 模型」最近一次业务请求来了却没有可用模板的时刻。只在本进程内存里：
    /// 票不定时续，过期后由这条记录触发按需补票；重启后由下一个缺票的请求重新记上。
    demands: Mutex<BTreeMap<(String, String), SystemTime>>,
}

/// 可廉价克隆的服务句柄；不实现 Debug。
#[derive(Clone)]
pub struct TurnStateService(Arc<ServiceInner>);

impl TurnStateService {
    /// 纯内存实例：测试与没有运行目录的宿主用。
    pub fn in_memory() -> Self {
        Self(Arc::new(ServiceInner {
            store: PinStore::in_memory(),
            settings: SettingsStore::in_memory(),
            observations: Observations::in_memory(),
            demands: Mutex::new(BTreeMap::new()),
        }))
    }

    /// 打开运行目录：`<dir>/buckets/`、`settings.json`、`index.json`、`observations.json`。
    pub fn open(dir: &Path) -> Result<Self, TurnStateError> {
        crate::fs_util::ensure_dir(dir).map_err(|_| TurnStateError::Io)?;
        Ok(Self(Arc::new(ServiceInner {
            store: PinStore::open(dir).map_err(|_| TurnStateError::Io)?,
            settings: SettingsStore::open(dir),
            observations: Observations::open(dir),
            demands: Mutex::new(BTreeMap::new()),
        })))
    }

    pub fn settings(&self) -> Settings {
        self.0.settings.get()
    }

    pub fn update_settings(&self, settings: Settings) -> Result<Settings, TurnStateError> {
        self.0.settings.set(settings).map_err(|error| match error {
            SetSettingsError::Invalid(error) => TurnStateError::Settings(error),
            SetSettingsError::Io => TurnStateError::Io,
        })
    }

    pub fn ttl(&self) -> Duration {
        self.settings().ttl()
    }

    fn caps(&self) -> TtlCaps {
        caps_of(&self.settings())
    }

    fn note_demand(&self, account: &str, model: &str, now: SystemTime) {
        let Ok(mut demands) = self.0.demands.lock() else {
            return;
        };
        if demands.len() >= MAX_DEMANDS
            && let Some(oldest) = demands
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(key, _)| key.clone())
        {
            demands.remove(&oldest);
        }
        demands.insert((account.to_owned(), model.to_owned()), now);
    }

    /// `window` 之内是否有业务请求在这个桶上缺过票。按需补票据此判断，闲置账号不补。
    pub fn demanded_within(
        &self,
        account: &str,
        model: &str,
        now: SystemTime,
        window: Duration,
    ) -> bool {
        self.0.demands.lock().is_ok_and(|demands| {
            demands
                .get(&(account.to_owned(), model.to_owned()))
                .is_some_and(|at| now.duration_since(*at).is_ok_and(|age| age <= window))
        })
    }

    /// 请求侧钩子：查桶、做决策、打一行日志。返回的 [`Attempt`] 要活到响应结束。
    pub fn begin_request(&self, facts: RequestFacts<'_>) -> Attempt {
        self.begin_request_on_route(facts, None)
    }

    /// 路由来自本次请求的凭据快照，不能在查票后再改成另一个账号版本。
    pub fn begin_request_on_route(
        &self,
        facts: RequestFacts<'_>,
        route_fingerprint: Option<&str>,
    ) -> Attempt {
        let settings = self.settings();
        let scope = Scope {
            account: facts.account.to_owned(),
            binding: facts.binding,
            model: facts.model.to_owned(),
            client: Some(facts.client.to_owned()),
        };
        let found = self.0.store.lookup_on_route(
            &scope,
            facts.egress,
            facts.now,
            caps_of(&settings),
            route_fingerprint,
        );
        let verdict = decision::decide(
            found.as_ref().map(|(value, _)| value.as_str()),
            facts.carried,
            &settings,
        );
        let value = if settings.dry_run {
            None
        } else {
            verdict.replacement
        };
        if value.is_some()
            && let Some((_, used)) = found.as_ref()
        {
            self.0.store.hit(used);
        }
        decision::log(
            &settings,
            verdict.decision,
            facts.account,
            facts.model,
            facts.carried.map(str::len),
            verdict.reason,
        );
        let injected_route = value
            .as_ref()
            .and(found.as_ref())
            .filter(|(_, found_scope)| found_scope.binding != scope.binding)
            .and(route_fingerprint)
            .map(str::to_owned);
        let reused = value.is_some();
        let had_template = found.is_some();
        if !had_template {
            self.note_demand(facts.account, facts.model, facts.now);
        }
        // 「改没改请求」和「这次请求用的是不是桶里这张模板」是两回事：客户端自带的 state
        // 恰好就是桶里这张时不必改写请求，但换模型时要作废的仍是它。
        let used = found
            .filter(|(template, _)| reused || facts.carried == Some(template.as_str()))
            .map(|(template, scope)| (scope, template));
        Attempt {
            service: self.clone(),
            settings,
            scope,
            egress: facts.egress.to_owned(),
            decision: verdict.decision,
            value,
            had_template,
            injected_route,
            reused,
            used,
            rejected: false,
            candidate: None,
            upstream_len: None,
            observed: false,
            started_at: facts.now,
        }
    }

    pub fn status(&self, account: &str, binding: &str, now: SystemTime) -> Vec<PinStatus> {
        self.0
            .store
            .status(account, binding, now, self.caps())
            .into_iter()
            .map(|record| PinStatus {
                model: record.model,
                length: record.len,
                captured_at: record.captured_at,
                issued_at: record.issued_at,
                issued_at_source: record.issued_at_source,
                expires_at: record.expires_at,
                hits: record.hits,
                account_wide: record.client.is_none(),
                source: record.source,
                gateway: record.gateway,
            })
            .collect()
    }

    /// 钉住一个刚在目标出口上观测到的账号级 state；返回生效模板的到期时间。
    pub fn pin_account_wide(&self, pin: AccountWidePin<'_>) -> Result<SystemTime, PinRejected> {
        let settings = self.settings();
        let (issued, expires_at) = Self::account_wide_times(&pin, &settings)?;
        let record = BucketRecord {
            account: pin.account.to_owned(),
            model: pin.model.to_owned(),
            value: pin.value.to_owned(),
            len: pin.value.len(),
            issued_at: issued.at,
            issued_at_source: issued.source,
            captured_at: pin.captured_at,
            expires_at,
            binding: pin.binding,
            egress: Some(pin.egress),
            client: None,
            source: pin.source,
            hits: 0,
            gateway: pin.gateway,
        };
        self.0
            .store
            .pin_account_wide(record, pin.now, caps_of(&settings))
            .map_err(|error| match error {
                StoreError::Full => PinRejected::Full,
                StoreError::Io | StoreError::Path => PinRejected::Io,
            })
    }

    /// 发布路由前检查票据；不写入模板，磁盘和容量错误仍需由发布方处理。
    pub fn validate_account_wide(&self, pin: &AccountWidePin<'_>) -> Result<(), PinRejected> {
        Self::account_wide_times(pin, &self.settings()).map(|_| ())
    }

    fn account_wide_times(
        pin: &AccountWidePin<'_>,
        settings: &Settings,
    ) -> Result<(fernet::ResolvedIssuedAt, SystemTime), PinRejected> {
        if !classify::storable(pin.value, settings) {
            return Err(PinRejected::Length);
        }
        let issued = fernet::resolve_issued_at(pin.value, pin.captured_at, pin.now, FUTURE_SKEW)
            .map_err(|_| PinRejected::FutureStamped)?;
        let expires_at = issued.at + pin.ttl.unwrap_or_else(|| settings.ttl());
        if expires_at <= pin.now {
            return Err(PinRejected::Expired);
        }
        Ok((issued, expires_at))
    }

    /// 云端票与签发路由共同构成模板身份，防止同一秒的新路由误保留旧路由的票。
    /// 返回实际生效模板的摘要，不能把保留的更新模板谎报为本次新票。
    pub fn pin_minted_account_wide(
        &self,
        mut pin: AccountWidePin<'_>,
        route_fingerprint: &str,
    ) -> Result<PinStatus, PinRejected> {
        if route_fingerprint.is_empty() {
            return Err(PinRejected::Io);
        }
        let binding = crate::binding::mint_binding(&pin.binding, route_fingerprint);
        let (account, model, now, egress) = (pin.account, pin.model, pin.now, pin.egress.clone());
        pin.binding = binding.clone();
        pin.source = Source::Mint;
        self.pin_account_wide(pin)?;
        self.account_wide(account, &binding, model, &egress, now)
            .ok_or(PinRejected::Io)
    }

    /// 当前出口上的账号级模板状态（不含值）。
    pub fn account_wide(
        &self,
        account: &str,
        binding: &str,
        model: &str,
        egress: &str,
        now: SystemTime,
    ) -> Option<PinStatus> {
        self.0
            .store
            .account_wide_record(account, binding, model, egress, now, self.caps())
            .map(|record| PinStatus {
                model: record.model,
                length: record.len,
                captured_at: record.captured_at,
                issued_at: record.issued_at,
                issued_at_source: record.issued_at_source,
                expires_at: record.expires_at,
                hits: record.hits,
                account_wide: true,
                source: record.source,
                gateway: record.gateway,
            })
    }

    /// 当前出口上账号级模板的到期时间；自动续期据此判断是否临近到期。
    pub fn account_wide_expires_at(
        &self,
        account: &str,
        binding: &str,
        model: &str,
        egress: &str,
        now: SystemTime,
    ) -> Option<SystemTime> {
        self.0
            .store
            .account_wide_record(account, binding, model, egress, now, self.caps())
            .map(|record| record.expires_at)
    }

    pub fn clear(&self, account: &str) {
        self.0.store.clear_account(account);
    }

    pub fn clear_bucket(&self, account: &str, model: Option<&str>) -> usize {
        self.0.store.clear_bucket(account, model)
    }

    pub fn buckets(&self, now: SystemTime) -> Vec<BucketSummary> {
        self.0
            .store
            .records(now, self.caps())
            .into_iter()
            .map(|record| BucketSummary {
                scope: if record.client.is_none() {
                    "account"
                } else {
                    "client"
                },
                egress: record
                    .egress
                    .as_deref()
                    .map(|fp| fp.chars().take(12).collect()),
                account: record.account,
                model: record.model,
                len: record.len,
                issued_at: unix_seconds(record.issued_at),
                issued_at_source: record.issued_at_source,
                captured_at: unix_seconds(record.captured_at),
                expires_at: unix_seconds(record.expires_at),
                source: record.source,
                hits: record.hits,
                gateway: record.gateway,
            })
            .collect()
    }

    pub fn observations(&self, now: SystemTime) -> ObservationSnapshot {
        self.0.observations.snapshot(now)
    }

    /// 记一次业务请求的模型对照结果；与账号是否启用固定 state 无关。
    pub fn record_served(&self, account: &str, model: &str, served: ServedMatch, at: SystemTime) {
        self.0
            .observations
            .record_served(account, model, served, at);
    }
}

fn caps_of(settings: &Settings) -> TtlCaps {
    TtlCaps {
        template: settings.ttl(),
        mint: settings.cloud_mint.ticket_ttl(),
    }
}

/// 一次请求的 turn-state 生命周期：注入值、上游响应观测、完成后的被动捕获。
pub struct Attempt {
    service: TurnStateService,
    settings: Settings,
    scope: Scope,
    egress: String,
    decision: Decision,
    value: Option<String>,
    /// 查桶时有没有有效模板；没有就是「缺票」，宿主据此触发云端打票预热。
    had_template: bool,
    injected_route: Option<String>,
    /// 本请求复用了一个模板（含账号级回退）：完成后不再另立客户端级 pin。
    reused: bool,
    /// 这次请求实际带出去的桶内模板（作用域与值）：注入的，或客户端自带且与桶内相同的。
    /// 换模型时据此条件失效那一张。
    used: Option<(Scope, String)>,
    /// 上游这一轮换了模型：签发的票不入库。
    rejected: bool,
    candidate: Option<String>,
    upstream_len: Option<usize>,
    observed: bool,
    started_at: SystemTime,
}

impl Attempt {
    /// 只有本次确实注入云端票时才返回其路由约束。
    pub fn injected_route(&self) -> Option<&str> {
        self.injected_route.as_deref()
    }

    pub const fn decision(&self) -> Decision {
        self.decision
    }

    /// 要写进请求的模板；`None` 表示原样放行（含 dry_run）。
    pub fn value(&self) -> Option<&str> {
        self.value.as_deref()
    }

    /// 桶里没有有效模板：账号此模型缺票。
    pub const fn needs_template(&self) -> bool {
        !self.had_template
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// 响应侧钩子：只接收本次真实上游响应；客户端传入的 state 不具备捕获资格。
    /// 可多次调用（握手头一次、WebSocket 元数据一次）；观测记一次，以最后一次有值的为准。
    pub fn observe(&mut self, upstream: Option<&str>) {
        self.observed = true;
        if let Some(value) = upstream {
            self.upstream_len = Some(value.len());
            if !self.rejected
                && self.candidate.is_none()
                && classify::storable(value, &self.settings)
            {
                self.candidate = Some(value.to_owned());
            }
        }
    }

    /// 上游这一轮声明的模型与发送的不一致：本轮签发的票不再入库，正在复用的那张模板
    /// 也不能继续用。失效是条件的，桶里已经换成别的值时不动。可重复调用，只生效一次。
    /// 模拟运行（`dry_run`）下不动桶里已有的模板，只是不收这一轮的新票。
    pub fn served_mismatch(&mut self, now: SystemTime) {
        if self.rejected {
            return;
        }
        self.rejected = true;
        self.candidate = None;
        if self.service.settings().dry_run {
            return;
        }
        let Some((scope, value)) = self.used.as_ref() else {
            return;
        };
        if self.service.0.store.invalidate(scope, value, now) {
            decision::log(
                &self.settings,
                Decision::Pass,
                &self.scope.account,
                &self.scope.model,
                Some(value.len()),
                "template dropped (served model mismatch)",
            );
        }
    }

    /// 请求完整成功后调用：把首个可入库候选立为该客户端的 pin。
    pub fn completed(&mut self, now: SystemTime) {
        if self.reused || self.rejected {
            return;
        }
        let Some(value) = self.candidate.take() else {
            return;
        };
        // 完成时已经过期的长请求不能重新赋予候选一小时寿命。
        let Ok(issued) = fernet::resolve_issued_at(&value, self.started_at, now, FUTURE_SKEW)
        else {
            return;
        };
        // 寿命按完成这一刻的设置算：长请求进行中设置被调短时，不沿用开始时的旧值。
        let current = self.service.settings();
        let expires_at = issued.at + current.ttl();
        if expires_at <= now {
            return;
        }
        let record = BucketRecord {
            account: self.scope.account.clone(),
            model: self.scope.model.clone(),
            len: value.len(),
            value,
            issued_at: issued.at,
            issued_at_source: issued.source,
            captured_at: self.started_at,
            expires_at,
            binding: self.scope.binding.clone(),
            egress: None,
            client: self.scope.client.clone(),
            source: Source::Passive,
            hits: 0,
            gateway: None,
        };
        let len = record.len;
        if self
            .service
            .0
            .store
            .insert_passive(record, &self.egress, now, caps_of(&current))
        {
            decision::log(
                &self.settings,
                Decision::Harvest,
                &self.scope.account,
                &self.scope.model,
                Some(len),
                "template stored (passive)",
            );
        }
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        if !self.observed {
            return;
        }
        let class = self
            .upstream_len
            .map(|len| classify::classify(len, &self.settings));
        self.service.0.observations.record(&ObservationInput {
            account: self.scope.account.clone(),
            model: self.scope.model.clone(),
            issued_len: self.upstream_len,
            class,
            decision: self.decision,
            injected: self.value.is_some(),
            at: SystemTime::now(),
        });
    }
}
