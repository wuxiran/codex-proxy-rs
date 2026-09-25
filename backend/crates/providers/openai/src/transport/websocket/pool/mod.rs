//! Codex WebSocket 连接池。

mod lease;
mod state;
mod supervisor;

use std::{
    future::Future,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::sync::Semaphore;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use uuid::Uuid;

use self::state::{
    WebSocketPoolConnecting, WebSocketPoolSlot, WebSocketPoolState, close_pooled_connection,
    close_pooled_connections,
};
use super::pump::PumpKeepalive;
use super::pump::WebSocketConnectionObservation;

pub use self::state::CodexWebSocketPoolKey;
pub(crate) use self::{
    lease::{
        WebSocketPoolAcquire, WebSocketPoolConnectLease, WebSocketPoolConnectOutcome,
        WebSocketPoolConnectWaiter, WebSocketPoolLease,
    },
    state::{
        CodexWebSocketConnectionMetadata, PooledWebSocketConnection, WARM_CONVERSATION_PREFIX,
        WebSocketContinuationState,
    },
};

const DEFAULT_MAX_CONNECTING: usize = 16;
const DEFAULT_MAX_AGE: Duration = Duration::from_mins(55);
const DEFAULT_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(25);
const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(25);
// 心跳也覆盖正在生成的连接，给短时链路停顿留出恢复余量。
const DEFAULT_PING_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const DEFAULT_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// WebSocket 连接池。
#[derive(Clone)]
pub struct CodexWebSocketPool {
    inner: Arc<Mutex<WebSocketPoolState>>,
    config: CodexWebSocketPoolConfig,
    tasks: TaskTracker,
    shutdown: CancellationToken,
    connect_semaphore: Arc<Semaphore>,
    maintenance_started: Arc<AtomicBool>,
    /// 业务请求是否可领养 warmer 挂住的满血连接（WS 保活的「业务复用」开关）。
    /// warmer 每轮按设置刷新；关掉后保活连接仍建/仍验，但业务不领养。
    warm_reuse: Arc<AtomicBool>,
}

impl Default for CodexWebSocketPool {
    fn default() -> Self {
        Self::with_config(CodexWebSocketPoolConfig::default())
    }
}

/// WebSocket 连接池配置。
#[derive(Debug, Clone, Copy)]
pub struct CodexWebSocketPoolConfig {
    /// 是否启用连接池。
    pub enabled: bool,
    /// 单个 socket 的最大生命周期。
    pub max_age: Duration,
    /// 所有账号合计允许并发执行的 opening 数。
    pub max_connecting: usize,
    /// 后台维护间隔；`None` 表示不启动后台任务。
    pub maintenance_interval: Option<Duration>,
    /// 池化连接的探活 ping 间隔（包括正在生成的连接）；`None` 表示不主动 ping。
    pub ping_interval: Option<Duration>,
    /// 发送 ping 后等待任意入站帧的超时时间；零值表示不校验 Pong deadline。
    pub ping_timeout: Duration,
    /// idle socket 无活动多久后视为失活。
    pub liveness_timeout: Option<Duration>,
    /// 等待下一条上游消息的空闲超时；`None` 或零值使用默认 300 秒。
    pub stream_idle_timeout: Option<Duration>,
}

impl Default for CodexWebSocketPoolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_age: DEFAULT_MAX_AGE,
            max_connecting: DEFAULT_MAX_CONNECTING,
            maintenance_interval: Some(DEFAULT_MAINTENANCE_INTERVAL),
            ping_interval: Some(DEFAULT_PING_INTERVAL),
            ping_timeout: DEFAULT_PING_TIMEOUT,
            // idle 连接不设失活截断：靠 ping/pong 保活，只在 max_age（55 分钟）
            // 或 ping 失败时关闭，维持跨轮可复用连接。
            liveness_timeout: None,
            stream_idle_timeout: Some(DEFAULT_STREAM_IDLE_TIMEOUT),
        }
    }
}

impl CodexWebSocketPoolConfig {
    /// pump 后台任务的保活策略：从连接池配置派生出 ping/pong 与 liveness 策略。
    pub(crate) fn keepalive(&self) -> PumpKeepalive {
        PumpKeepalive {
            ping_interval: self.ping_interval,
            ping_timeout: (!self.ping_timeout.is_zero()).then_some(self.ping_timeout),
            liveness_timeout: self.liveness_timeout,
        }
    }
}

impl CodexWebSocketPool {
    /// 构造不限制累计 slot 数的连接池策略和状态。
    pub fn new(max_age: Duration) -> Self {
        Self::with_config(CodexWebSocketPoolConfig {
            max_age,
            maintenance_interval: None,
            ping_interval: None,
            liveness_timeout: None,
            ..CodexWebSocketPoolConfig::default()
        })
    }

    /// 使用完整配置构造连接池。
    pub fn with_config(config: CodexWebSocketPoolConfig) -> Self {
        let pool = Self {
            inner: Arc::new(Mutex::new(WebSocketPoolState::default())),
            config,
            tasks: TaskTracker::new(),
            shutdown: CancellationToken::new(),
            connect_semaphore: Arc::new(Semaphore::new(config.max_connecting)),
            maintenance_started: Arc::new(AtomicBool::new(false)),
            warm_reuse: Arc::new(AtomicBool::new(true)),
        };
        pool.spawn_maintenance_task();
        pool
    }

    /// 业务复用开关：warmer 每轮按 `warm_pool.business_reuse` 刷新。
    pub fn set_warm_reuse(&self, enabled: bool) {
        self.warm_reuse.store(enabled, Ordering::Release);
    }

    /// 一条建连许可（不阻塞前台）；warmer 建保活连接前先拿，拿不到就本轮少建。
    pub(crate) fn try_connect_permit(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.connect_semaphore.clone().try_acquire_owned().ok()
    }

    /// 某账号当前挂着的保活连接数（空闲的保活 key slot）。观测用。
    pub(crate) fn warm_len_for_account(&self, account_id: &str) -> usize {
        let state = self.lock_state();
        state
            .slots
            .iter()
            .filter(|(key, slot)| {
                key.is_warm()
                    && key.account_id() == account_id
                    && matches!(slot, WebSocketPoolSlot::Idle { .. })
            })
            .count()
    }

    /// 某账号当前已占用的保活 slot 序号集合（空闲的保活连接）。warmer 据此挑**空闲序号**补齐、
    /// 挑**已占用序号**复探，避免用「计数」当序号导致反复复探同一条、永不补满。
    pub(crate) fn warm_slots_for_account(&self, account_id: &str) -> std::collections::BTreeSet<usize> {
        let state = self.lock_state();
        state
            .slots
            .iter()
            .filter_map(|(key, slot)| {
                (key.is_warm()
                    && key.account_id() == account_id
                    && matches!(slot, WebSocketPoolSlot::Idle { .. }))
                .then(|| key.warm_slot())
                .flatten()
            })
            .collect()
    }

    /// 进程内所有账号的保活连接总数（空闲的保活 slot）；`max_total_connections` 用。
    pub(crate) fn warm_len_total(&self) -> usize {
        let state = self.lock_state();
        state
            .slots
            .iter()
            .filter(|(key, slot)| key.is_warm() && matches!(slot, WebSocketPoolSlot::Idle { .. }))
            .count()
    }

    /// 关掉某账号所有空闲的保活连接（探针判降智时用，避免业务领养到降智连接）。
    /// 返回关闭的条数。正在被 canary 借用（Busy）的不动，它会在归还后由下一轮处理。
    pub(crate) async fn evict_warm_for_account(&self, account_id: &str) -> usize {
        let mut to_close = Vec::new();
        {
            let mut state = self.lock_state();
            let keys: Vec<_> = state
                .slots
                .iter()
                .filter(|(key, slot)| {
                    key.is_warm()
                        && key.account_id() == account_id
                        && matches!(slot, WebSocketPoolSlot::Idle { .. })
                })
                .map(|(key, _)| key.clone())
                .collect();
            for key in keys {
                if let Some(WebSocketPoolSlot::Idle { connection }) = state.slots.remove(&key) {
                    to_close.push(*connection);
                }
            }
        }
        let count = to_close.len();
        close_pooled_connections(to_close).await;
        count
    }

    /// pump 后台任务的保活策略（供建连时传入）。
    pub(crate) fn keepalive(&self) -> PumpKeepalive {
        self.config.keepalive()
    }

    /// 等待下一条上游消息的空闲超时；`None` 或零值使用默认 300 秒。
    pub(crate) fn stream_idle_timeout(&self) -> Option<Duration> {
        self.config.stream_idle_timeout
    }

    /// 注册由连接池生命周期托管的 opening 任务。
    pub(crate) fn spawn_connect_task(&self, future: impl Future<Output = ()> + Send + 'static) {
        drop(self.tasks.spawn(future));
    }

    pub(crate) async fn acquire(
        &self,
        key: &CodexWebSocketPoolKey,
        required_response_id: Option<&str>,
    ) -> WebSocketPoolAcquire {
        self.spawn_maintenance_task();
        let mut connections_to_close = Vec::new();
        let acquire = {
            let mut state = self.lock_state();
            let mut continuation_loss = None;
            if !self.config.enabled || state.shutting_down {
                return WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Disabled);
            }
            let key = if let Some(response_id) = required_response_id {
                let Some(key) = state.slots.iter().find_map(|(candidate, slot)| {
                    (candidate.same_logical_connection(key)
                        && slot.latest_response_id() == Some(response_id))
                    .then(|| candidate.clone())
                }) else {
                    return state
                        .continuation_loss(key, response_id, tokio::time::Instant::now())
                        .map_or(
                            WebSocketPoolAcquire::Bypass(
                                WebSocketPoolBypassReason::ContinuationNotFound,
                            ),
                            WebSocketPoolAcquire::ContinuationLost,
                        );
                };
                key
            } else {
                key.clone()
            };
            match state.slots.get(&key) {
                Some(WebSocketPoolSlot::Busy(_)) => {
                    return WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Busy);
                }
                Some(WebSocketPoolSlot::Connecting(connecting)) => {
                    return WebSocketPoolAcquire::Wait(WebSocketPoolConnectWaiter {
                        receiver: connecting.outcome.subscribe(),
                        started_at: connecting.started_at,
                    });
                }
                Some(WebSocketPoolSlot::Idle { .. }) => {
                    let Some(WebSocketPoolSlot::Idle { connection, .. }) = state.slots.remove(&key)
                    else {
                        return WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Busy);
                    };
                    // 零成本探活：后台 pump 已实时感知连接死亡（RST/Close/EOF/失活），
                    // 复用前只需读取 is_closed 标志，避免复用到静默死连接后卡到超时。
                    let expired = connection.created_at.elapsed() >= self.config.max_age;
                    let closed = connection.websocket.is_closed();
                    if !expired && !closed {
                        let lease = WebSocketPoolLease::reserve(
                            self.clone(),
                            key.clone(),
                            connection.continuation.latest_response_id(),
                        );
                        state.slots.insert(
                            key.clone(),
                            WebSocketPoolSlot::Busy(lease.reservation.clone()),
                        );
                        return WebSocketPoolAcquire::Reused { connection, lease };
                    }
                    let observation = if expired && !closed {
                        connection
                            .websocket
                            .observation()
                            .with_exit_reason("max_age_expired")
                    } else {
                        connection.websocket.observation()
                    };
                    state.remember_continuation_loss(
                        &key,
                        connection.continuation.latest_response_id(),
                        observation.clone(),
                        tokio::time::Instant::now(),
                    );
                    connections_to_close.push(*connection);
                    if required_response_id.is_some() {
                        continuation_loss = Some(observation);
                    }
                }
                None => {}
            }

            let acquire = if let Some(observation) = continuation_loss {
                WebSocketPoolAcquire::ContinuationLost(observation)
            } else if required_response_id.is_some() {
                WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::ContinuationNotFound)
            } else if self.warm_reuse.load(Ordering::Acquire)
                && let Some(reused) =
                    self.adopt_warm_locked(&mut state, &key, &mut connections_to_close)
            {
                // 没有该对话的连接、又是可新拨的请求：优先领养一条 warmer 挂住的满血连接，
                // 让业务跑在"健康期"内建立的连接上。没有可领养的就落到下面照常新拨（行为不变）。
                reused
            } else {
                let connect_permit = self.connect_semaphore.clone().try_acquire_owned().ok();
                match connect_permit {
                    Some(connect_permit) => {
                        let lease = WebSocketPoolConnectLease::reserve(
                            self.clone(),
                            key.clone(),
                            connect_permit,
                        );
                        state.slots.insert(
                            key,
                            WebSocketPoolSlot::Connecting(WebSocketPoolConnecting {
                                id: lease.id,
                                started_at: lease.started_at,
                                outcome: lease.outcome.clone(),
                                cancellation: lease.cancellation.clone(),
                            }),
                        );
                        WebSocketPoolAcquire::Connect(lease)
                    }
                    None => WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Cap),
                }
            };
            drop(state);
            acquire
        };

        close_pooled_connections(connections_to_close).await;

        acquire
    }

    /// 在已加锁的池状态里，为可新拨的业务 key 领养一条 warmer 挂住的满血连接。
    /// 命中则从保活 key 摘出、清空探针留下的续接状态、以业务 key 登记为 Busy 并返回 `Reused`；
    /// 过期/已关的保活连接顺手加入关闭队列并跳过；没有可用的返回 `None`（调用方照常新拨）。
    fn adopt_warm_locked(
        &self,
        state: &mut WebSocketPoolState,
        key: &CodexWebSocketPoolKey,
        connections_to_close: &mut Vec<PooledWebSocketConnection>,
    ) -> Option<WebSocketPoolAcquire> {
        loop {
            let warm_key = state.slots.iter().find_map(|(candidate, slot)| {
                (candidate.serves_warm_target(key)
                    && matches!(slot, WebSocketPoolSlot::Idle { .. }))
                .then(|| candidate.clone())
            })?;
            let Some(WebSocketPoolSlot::Idle { connection }) = state.slots.remove(&warm_key) else {
                return None;
            };
            let mut connection = connection;
            if connection.created_at.elapsed() >= self.config.max_age
                || connection.websocket.is_closed()
            {
                connections_to_close.push(*connection);
                continue;
            }
            // 探针 response id 不是业务续接，绑定给对话前清空。
            connection.continuation = WebSocketContinuationState::default();
            let lease = WebSocketPoolLease::reserve(self.clone(), key.clone(), None);
            state.slots.insert(
                key.clone(),
                WebSocketPoolSlot::Busy(lease.reservation.clone()),
            );
            return Some(WebSocketPoolAcquire::Reused { connection, lease });
        }
    }

    async fn put_reserved(
        &self,
        key: &CodexWebSocketPoolKey,
        reservation_id: Uuid,
        connection: PooledWebSocketConnection,
    ) {
        let mut connection = Some(connection);
        {
            let mut state = self.lock_state();
            let expired = connection
                .as_ref()
                .is_some_and(|connection| connection.created_at.elapsed() >= self.config.max_age);
            let owns_reservation = matches!(
                state.slots.get(key),
                Some(WebSocketPoolSlot::Busy(reservation))
                    if reservation.id == reservation_id
            );
            if owns_reservation && (expired || state.shutting_down || !self.config.enabled) {
                if expired && let Some(connection) = connection.as_ref() {
                    state.remember_continuation_loss(
                        key,
                        connection.continuation.latest_response_id(),
                        connection
                            .websocket
                            .observation()
                            .with_exit_reason("max_age_expired"),
                        tokio::time::Instant::now(),
                    );
                }
                state.slots.remove(key);
            } else if owns_reservation && let Some(connection) = connection.take() {
                state.slots.insert(
                    key.clone(),
                    WebSocketPoolSlot::Idle {
                        connection: Box::new(connection),
                    },
                );
            }
        }
        if let Some(connection) = connection {
            close_pooled_connection(connection).await;
        }
    }

    fn discard_reserved_now(&self, key: &CodexWebSocketPoolKey, reservation_id: Uuid) {
        let mut state = self.lock_state();
        if matches!(
            state.slots.get(key),
            Some(WebSocketPoolSlot::Busy(reservation)) if reservation.id == reservation_id
        ) {
            state.slots.remove(key);
        }
    }

    fn discard_reserved_with_observation(
        &self,
        key: &CodexWebSocketPoolKey,
        reservation_id: Uuid,
        observation: WebSocketConnectionObservation,
    ) {
        let mut state = self.lock_state();
        let latest_response_id = match state.slots.get(key) {
            Some(WebSocketPoolSlot::Busy(reservation)) if reservation.id == reservation_id => {
                reservation.latest_response_id.clone()
            }
            _ => return,
        };
        state.remember_continuation_loss(
            key,
            latest_response_id.as_deref(),
            observation,
            tokio::time::Instant::now(),
        );
        state.slots.remove(key);
    }

    async fn finish_connect_reserved(
        &self,
        key: &CodexWebSocketPoolKey,
        connect_id: Uuid,
        connection: PooledWebSocketConnection,
    ) -> Result<(Box<PooledWebSocketConnection>, WebSocketPoolLease), Box<PooledWebSocketConnection>>
    {
        let mut state = self.lock_state();
        let owns_connect = matches!(
            state.slots.get(key),
            Some(WebSocketPoolSlot::Connecting(connecting)) if connecting.id == connect_id
        );
        if owns_connect && !state.shutting_down && self.config.enabled {
            let lease = WebSocketPoolLease::reserve(self.clone(), key.clone(), None);
            state.slots.insert(
                key.clone(),
                WebSocketPoolSlot::Busy(lease.reservation.clone()),
            );
            Ok((Box::new(connection), lease))
        } else {
            if owns_connect {
                state.slots.remove(key);
            }
            Err(Box::new(connection))
        }
    }

    async fn fail_connect(&self, key: &CodexWebSocketPoolKey, connect_id: Uuid) {
        self.fail_connect_now(key, connect_id);
    }

    fn fail_connect_now(&self, key: &CodexWebSocketPoolKey, connect_id: Uuid) {
        let mut state = self.lock_state();
        if matches!(
            state.slots.get(key),
            Some(WebSocketPoolSlot::Connecting(connecting)) if connecting.id == connect_id
        ) {
            state.slots.remove(key);
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, WebSocketPoolState> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSocketPoolBypassReason {
    Disabled,
    Busy,
    Cap,
    ContinuationNotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebSocketPoolDecision {
    kind: WebSocketPoolDecisionKind,
}

impl WebSocketPoolDecision {
    pub fn new() -> Self {
        Self {
            kind: WebSocketPoolDecisionKind::New,
        }
    }

    pub fn reuse() -> Self {
        Self {
            kind: WebSocketPoolDecisionKind::Reuse,
        }
    }

    pub fn kind(self) -> &'static str {
        self.kind.as_str()
    }

    pub const fn is_reuse(self) -> bool {
        matches!(self.kind, WebSocketPoolDecisionKind::Reuse)
    }
}

impl Default for WebSocketPoolDecision {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebSocketPoolDecisionKind {
    New,
    Reuse,
}

impl WebSocketPoolDecisionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Reuse => "reuse",
        }
    }
}
