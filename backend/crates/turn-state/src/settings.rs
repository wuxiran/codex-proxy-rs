//! 运行设置：落在 `<dir>/settings.json`，多实例共享，按 mtime 热刷新。

use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, SystemTime},
};

use serde::{Deserialize, Serialize};

use crate::fs_util;

pub const DEFAULT_TTL: Duration = Duration::from_secs(3600);
const MIN_TTL_SECONDS: u64 = 600;
const MAX_TTL_SECONDS: u64 = 86_400;
const SETTINGS_FILE: &str = "settings.json";
/// 两次 stat 之间的最短间隔：请求路径上不能每次都碰磁盘。
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// 请求头改写策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InjectMode {
    /// 桶内有有效模板就注入：请求没带 state 则补上，带了不同的就替换（现网既有行为）。
    #[default]
    Always,
    /// 只替换长度属于受限档的 state；没带 state 或长度不属于受限档的请求原样放行。
    ReplaceOnly,
}

impl InjectMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::ReplaceOnly => "replace-only",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct Settings {
    /// 模板自签发起的可用时长（秒）。
    pub ttl_seconds: u64,
    pub inject_mode: InjectMode,
    /// 只算决策、打日志，不真改请求。
    pub dry_run: bool,
    /// 每个请求打一行决策日志（不含票值）。
    pub log_decisions: bool,
    /// 可入库的模板长度；空表示退回 ≥200 字节下限规则。
    pub template_lengths: Vec<usize>,
    /// 受限/降级档长度；永不入库。
    pub degraded_lengths: Vec<usize>,
    /// 云端打票：账号缺票时向 relay 铸票并钉住路由 cookie 对。
    pub cloud_mint: CloudMintSettings,
    /// WS 保活：在乐观窗口内为账号预建并挂住上游 WebSocket，用 canary 验满血。
    pub warm_pool: WarmPoolSettings,
}

/// WS 保活设置。图里说法：风控号首次碰到新节点有 ~200s 乐观窗口，窗口内建立的
/// WebSocket 挂住后 1h 满血；预建一批满血 WS 供业务复用。默认关，开了也只在
/// 后台开连接跑 canary，不改业务链路（业务复用是 Phase 2，另有开关）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct WarmPoolSettings {
    pub enabled: bool,
    /// 每个账号预热并挂住多少条 WS。
    pub connections_per_account: u32,
    /// 预热的模型列表；空 = 用账号支持的模型 / cloud_mint.models。
    pub models: Vec<String>,
    /// 一条 WS 最多挂多久（秒），到点主动弃用；应 < 上游 55min。
    pub max_age_seconds: u64,
    /// canary 复探间隔（秒）：开连接后每隔这么久在同一条 WS 上再问一次探针题。
    pub reprobe_seconds: u64,
    /// 是否跑 canary 探针（发探针题、按答案判满血）；关掉则只开连接挂住不验。
    pub probe: bool,
    /// 探针题正文；空 = 用内置糖果题。
    pub probe_prompt: String,
    /// 满血判据：答案去空白后以此开头即判满血（如 "21"）。
    pub probe_expect: String,
    /// 探针用的模型；空 = 用 models 里的第一个。
    pub probe_model: String,
    /// 探针 effort：low/medium/high/xhigh。
    pub probe_effort: String,
    /// 业务请求是否可领养保活连接；关掉则只建/只验不复用（观测态）。
    pub business_reuse: bool,
    /// 探针判降智/失败后，该账号多久内不再开保活连接（秒）。
    pub cooldown_seconds: u64,
    /// 进程内保活连接总数上限。
    pub max_total_connections: u32,
    /// 单次探针的整次调用上限（秒）；高 effort 慢，留足。
    pub probe_timeout_seconds: u64,
    /// 探到降智时，最多再换几个节点重试找满血（同出口重开=落新节点实例）；
    /// 只有连续这么多次都降智才冷却。0 = 不重试（一次降智就冷却，适合死号多的场景）。
    pub probe_retries: u32,
}

impl Default for WarmPoolSettings {
    fn default() -> Self {
        Self {
            // 老板决策：默认开。安全性来自领养的「无满血连接即照常新拨」降级语义，
            // 加上 warmer 对反复探针失败的账号（如被标记号）做冷却，避免烧配额空转。
            enabled: true,
            connections_per_account: 2,
            models: Vec::new(),
            // 50 分钟：卡在上游 55min 硬上限之内。
            max_age_seconds: 3000,
            // 低频复探（老板选定）：新建连接必验，之后每 5 分钟一次。
            reprobe_seconds: 300,
            probe: true,
            probe_prompt: String::new(),
            probe_expect: "21".to_owned(),
            probe_model: String::new(),
            probe_effort: "high".to_owned(),
            business_reuse: true,
            cooldown_seconds: 600,
            max_total_connections: 64,
            probe_timeout_seconds: 180,
            // 降智时最多再换 4 个节点找满血；干净号一般几次内撞到满血。
            probe_retries: 4,
        }
    }
}

impl WarmPoolSettings {
    pub fn max_age(&self) -> Duration {
        Duration::from_secs(self.max_age_seconds)
    }

    pub fn reprobe(&self) -> Duration {
        Duration::from_secs(self.reprobe_seconds)
    }

    pub fn cooldown(&self) -> Duration {
        Duration::from_secs(self.cooldown_seconds)
    }

    pub fn probe_timeout(&self) -> Duration {
        Duration::from_secs(self.probe_timeout_seconds)
    }

    fn validate(&self) -> Result<(), SettingsError> {
        if !self.enabled {
            return Ok(());
        }
        if !(1..=16).contains(&self.connections_per_account) {
            return Err(SettingsError::WarmConnections);
        }
        if !(60..=3300).contains(&self.max_age_seconds) {
            return Err(SettingsError::WarmMaxAge);
        }
        if !(15..=1800).contains(&self.reprobe_seconds) {
            return Err(SettingsError::WarmReprobe);
        }
        if !matches!(
            self.probe_effort.as_str(),
            "low" | "medium" | "high" | "xhigh"
        ) {
            return Err(SettingsError::WarmEffort);
        }
        if !(30..=86_400).contains(&self.cooldown_seconds) {
            return Err(SettingsError::WarmCooldown);
        }
        if !(1..=1024).contains(&self.max_total_connections) {
            return Err(SettingsError::WarmTotal);
        }
        if !(30..=600).contains(&self.probe_timeout_seconds) {
            return Err(SettingsError::WarmProbeTimeout);
        }
        if self.probe_retries > 16 {
            return Err(SettingsError::WarmRetries);
        }
        Ok(())
    }
}

/// 云端打票设置。relay 即 `deploy/cloud-mint/index.js`（阿里 FC 或 89 上的容器）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct CloudMintSettings {
    pub enabled: bool,
    /// `native`：cpr 自己经账号代理向上游铸票；`relay`：交给 deploy/cloud-mint 的 relay。
    pub mode: MintMode,
    /// 只观测不注入：照常打票、记录，但不把 pair/票写进业务请求。
    pub observe_only: bool,
    /// relay 地址，如 `http://127.0.0.1:9000`。
    pub relay_url: String,
    /// relay 的 `X-Relay-Key`；只落在 0600 的 settings.json，不回显给前端。
    pub relay_key: String,
    /// 插件 → relay 之间的前置代理；空 = 直连。
    pub proxy_url: String,
    /// 目标网关（`unified-95`）；空 = 任意。
    pub gateway: String,
    /// 预期票长；0 = 不查。
    pub ticket_len: usize,
    /// 票有效期（秒）；从票内嵌签发时间起算。
    pub ticket_ttl_seconds: u64,
    /// 打票的模型列表；空 = 用业务请求的模型。
    pub models: Vec<String>,
    /// `sse` 或 `websocket`。
    pub transport: String,
    /// 同一账号两次打票之间的最短间隔（秒），失败后的冷却。
    pub cooldown_seconds: u64,
    /// 原生打票每个模型最多发几次（每发都是一次真实上游请求，烧配额）。
    pub max_attempts: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MintMode {
    #[default]
    Native,
    Relay,
}

impl Default for CloudMintSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: MintMode::Native,
            observe_only: false,
            relay_url: String::new(),
            relay_key: String::new(),
            proxy_url: String::new(),
            // 空 = 任意网关：哪个节点不降智没有证据，先观测再定，不默认钉某个节点。
            gateway: String::new(),
            ticket_len: 780,
            ticket_ttl_seconds: 240,
            models: Vec::new(),
            transport: "sse".to_owned(),
            cooldown_seconds: 30,
            max_attempts: 8,
        }
    }
}

impl CloudMintSettings {
    pub fn ticket_ttl(&self) -> Duration {
        Duration::from_secs(self.ticket_ttl_seconds)
    }

    pub fn cooldown(&self) -> Duration {
        Duration::from_secs(self.cooldown_seconds)
    }

    /// 前端展示用：抹掉密钥，只说有没有。
    pub fn redacted(&self) -> Self {
        Self {
            relay_key: if self.relay_key.is_empty() {
                String::new()
            } else {
                "<set>".to_owned()
            },
            ..self.clone()
        }
    }

    fn validate(&self) -> Result<(), SettingsError> {
        if self.enabled && self.mode == MintMode::Relay {
            let url = self.relay_url.trim();
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(SettingsError::RelayUrl);
            }
            if self.relay_key.is_empty() {
                return Err(SettingsError::RelayKey);
            }
        }
        if !self.proxy_url.is_empty()
            && !(self.proxy_url.starts_with("http://")
                || self.proxy_url.starts_with("https://")
                || self.proxy_url.starts_with("socks5://")
                || self.proxy_url.starts_with("socks5h://"))
        {
            return Err(SettingsError::ProxyUrl);
        }
        if !(30..=86_400).contains(&self.ticket_ttl_seconds) {
            return Err(SettingsError::Ttl);
        }
        if self.transport != "sse" && self.transport != "websocket" {
            return Err(SettingsError::Transport);
        }
        if !(1..=64).contains(&self.max_attempts) {
            return Err(SettingsError::Attempts);
        }
        if self.ticket_len != 0 && self.ticket_len < crate::classify::MIN_TURN_STATE_LEN {
            return Err(SettingsError::LengthTooShort);
        }
        Ok(())
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ttl_seconds: DEFAULT_TTL.as_secs(),
            inject_mode: InjectMode::Always,
            dry_run: false,
            log_decisions: true,
            template_lengths: Vec::new(),
            degraded_lengths: Vec::new(),
            cloud_mint: CloudMintSettings::default(),
            warm_pool: WarmPoolSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("cloud mint relay url must be an http(s) origin")]
    RelayUrl,
    #[error("cloud mint relay key is required when enabled")]
    RelayKey,
    #[error("proxy url must be http(s) or socks5(h)")]
    ProxyUrl,
    #[error("cloud mint transport must be sse or websocket")]
    Transport,
    #[error("cloud mint max attempts must be between 1 and 64")]
    Attempts,
    #[error("ttl must be between {MIN_TTL_SECONDS} and {MAX_TTL_SECONDS} seconds")]
    Ttl,
    #[error("a length must be at least 200 bytes")]
    LengthTooShort,
    #[error("template and degraded lengths must not overlap")]
    Overlap,
    #[error("warm pool connections per account must be between 1 and 16")]
    WarmConnections,
    #[error("warm pool max age must be between 60 and 3300 seconds")]
    WarmMaxAge,
    #[error("warm pool reprobe interval must be between 15 and 1800 seconds")]
    WarmReprobe,
    #[error("warm pool probe effort must be low, medium, high or xhigh")]
    WarmEffort,
    #[error("warm pool cooldown must be between 30 and 86400 seconds")]
    WarmCooldown,
    #[error("warm pool max total connections must be between 1 and 1024")]
    WarmTotal,
    #[error("warm pool probe timeout must be between 30 and 600 seconds")]
    WarmProbeTimeout,
    #[error("warm pool probe retries must be at most 16")]
    WarmRetries,
}

impl Settings {
    pub fn ttl(&self) -> Duration {
        Duration::from_secs(self.ttl_seconds)
    }

    /// 排序去重后校验；写盘前必须经过这里。
    pub fn normalized(mut self) -> Result<Self, SettingsError> {
        self.template_lengths.sort_unstable();
        self.template_lengths.dedup();
        self.degraded_lengths.sort_unstable();
        self.degraded_lengths.dedup();
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), SettingsError> {
        if !(MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(&self.ttl_seconds) {
            return Err(SettingsError::Ttl);
        }
        if self
            .template_lengths
            .iter()
            .chain(&self.degraded_lengths)
            .any(|len| *len < crate::classify::MIN_TURN_STATE_LEN)
        {
            return Err(SettingsError::LengthTooShort);
        }
        if self
            .template_lengths
            .iter()
            .any(|len| self.degraded_lengths.contains(len))
        {
            return Err(SettingsError::Overlap);
        }
        self.cloud_mint.validate()?;
        self.warm_pool.validate()
    }

    /// 前端展示用副本：relay 密钥不回显。
    pub fn redacted(&self) -> Self {
        Self {
            cloud_mint: self.cloud_mint.redacted(),
            ..self.clone()
        }
    }

    /// 前端整体提交时密钥字段可能是 `<set>` 占位：沿用磁盘上的旧值。
    pub fn merge_secret_placeholders(mut self, current: &Self) -> Self {
        if self.cloud_mint.relay_key == "<set>" {
            self.cloud_mint.relay_key = current.cloud_mint.relay_key.clone();
        }
        self
    }
}

struct Cache {
    settings: Settings,
    mtime: Option<SystemTime>,
    checked_at: Option<Instant>,
}

/// 设置存储；`dir` 为空时只在内存里保存。
pub struct SettingsStore {
    dir: Option<PathBuf>,
    cache: Mutex<Cache>,
}

impl SettingsStore {
    pub fn in_memory() -> Self {
        Self {
            dir: None,
            cache: Mutex::new(Cache {
                settings: Settings::default(),
                mtime: None,
                checked_at: None,
            }),
        }
    }

    pub fn open(dir: &Path) -> Self {
        let store = Self {
            dir: Some(dir.to_path_buf()),
            cache: Mutex::new(Cache {
                settings: Settings::default(),
                mtime: None,
                checked_at: None,
            }),
        };
        store.refresh(true);
        store
    }

    pub fn get(&self) -> Settings {
        self.refresh(false);
        self.cache
            .lock()
            .map_or_else(|_| Settings::default(), |cache| cache.settings.clone())
    }

    /// 校验后原子写盘，并立即更新缓存。
    pub fn set(&self, settings: Settings) -> Result<Settings, SetSettingsError> {
        let settings = settings.normalized().map_err(SetSettingsError::Invalid)?;
        if let Some(dir) = &self.dir {
            let _guard = fs_util::lock(dir).map_err(|_| SetSettingsError::Io)?;
            let bytes = serde_json::to_vec_pretty(&settings).map_err(|_| SetSettingsError::Io)?;
            fs_util::atomic_write(dir, SETTINGS_FILE, &bytes).map_err(|_| SetSettingsError::Io)?;
        }
        if let Ok(mut cache) = self.cache.lock() {
            cache.settings = settings.clone();
            cache.mtime = self
                .dir
                .as_ref()
                .and_then(|dir| fs::metadata(dir.join(SETTINGS_FILE)).ok())
                .and_then(|meta| meta.modified().ok());
            cache.checked_at = Some(Instant::now());
        }
        Ok(settings)
    }

    fn refresh(&self, force: bool) {
        let Some(dir) = &self.dir else {
            return;
        };
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        if !force
            && cache
                .checked_at
                .is_some_and(|checked| checked.elapsed() < REFRESH_INTERVAL)
        {
            return;
        }
        cache.checked_at = Some(Instant::now());
        let path = dir.join(SETTINGS_FILE);
        let mtime = match fs::metadata(&path) {
            Ok(meta) => meta.modified().ok(),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                // 文件被删 = 回到默认值。
                if cache.mtime.is_some() {
                    cache.settings = Settings::default();
                    cache.mtime = None;
                }
                return;
            }
            Err(_) => return,
        };
        if mtime.is_some() && mtime == cache.mtime {
            return;
        }
        match fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
            .and_then(|settings| settings.normalized().ok())
        {
            Some(settings) => {
                cache.settings = settings;
                cache.mtime = mtime;
            }
            None => {
                // 损坏的设置文件不能把业务拖回默认值：保留上次能读出的好值。
                tracing::warn!(
                    target: "turn_state",
                    "[turn-state] settings file is unreadable; keeping the last good settings"
                );
                cache.mtime = mtime;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SetSettingsError {
    #[error(transparent)]
    Invalid(SettingsError),
    #[error("turn state settings could not be written")]
    Io,
}
