//! 多 Provider 账号目录与连接测试的公共事实。

use std::{collections::BTreeMap, pin::Pin};

use chrono::{DateTime, Utc};
use futures::Stream;

use gateway_core::{
    engine::probe::AccountProbeErrorSource, error::GatewayErrorKind, routing::ProviderKind,
    upstream::UpstreamSendState,
};

use super::{PageSize, Revision, account_groups::AccountGroupRef, observability::TimeRange};

pub use gateway_core::account::{
    AccountConcurrencyLimit, AccountErrorReason, AccountStatus, AccountStatusFacts,
    AccountStatusProjection, AccountWeight, CredentialState, QuotaAccessState, QuotaEvidence,
    QuotaState, resolve_account_status,
};

/// 导入时统一应用的账号备注、调度与分组设置；缺省时保留原有导入语义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountImportSettings {
    pub notes: Option<String>,
    pub enabled: bool,
    pub concurrency_limit: Option<AccountConcurrencyLimit>,
    pub weight: AccountWeight,
    pub model_access: Option<gateway_core::account::AccountModelAccess>,
    pub group_ids: Vec<gateway_core::routing::AccountGroupId>,
}

/// 账号列表排序字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountSortField {
    Email,
    Status,
    PlanType,
    Usage,
    LastUsedAt,
    ExpiresAt,
}

/// 账号列表排序方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

/// 一组完整的账号排序规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountSort {
    pub field: AccountSortField,
    pub direction: SortDirection,
}

/// 账号列表的存储查询条件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountListQuery {
    pub page: u32,
    pub page_size: PageSize,
    pub provider_kind: Option<ProviderKind>,
    pub group_filter: Option<AccountGroupFilter>,
    pub search: Option<String>,
    pub status: Option<AccountStatus>,
    pub sort: Option<AccountSort>,
}

/// Admin query service 从运行态存储取得的当前账号冷却快照。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountRuntimeSnapshot {
    pub cooldown: BTreeMap<String, gateway_core::account::AccountCooldown>,
    /// `None` 表示实时 lease 存储不可用；`Some` 中未出现的账号当前使用量为零。
    pub in_flight: Option<BTreeMap<String, u64>>,
}

/// 恢复任务读取的冻结快照；generation 将异步结果绑定到本次冻结。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountFreeze {
    pub credential_revision: Revision,
    pub until: DateTime<Utc>,
    pub generation: String,
    pub requires_probe: bool,
}

/// Optional account membership filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountGroupFilter {
    Group(gateway_core::routing::AccountGroupId),
    Ungrouped,
}

/// 账号公共存储投影；Provider 专属字段不进入此结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRecord {
    pub id: String,
    pub provider_kind: ProviderKind,
    pub groups: Vec<AccountGroupRef>,
    pub name: String,
    pub notes: Option<String>,
    pub email: Option<String>,
    pub upstream_user_id: Option<String>,
    pub upstream_account_id: Option<String>,
    pub plan_type: Option<String>,
    pub authentication_kind: String,
    pub credential_revision: Revision,
    pub has_refresh_token: bool,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub next_refresh_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub concurrency_limit: Option<AccountConcurrencyLimit>,
    pub weight: AccountWeight,
    pub model_access: gateway_core::account::AccountModelAccess,
    pub outbound_proxy: Option<gateway_core::account::OutboundProxy>,
    pub credential_state: CredentialState,
    pub credential_observed_at: DateTime<Utc>,
    pub quota: QuotaState,
    pub last_error_reason: Option<AccountErrorReason>,
    pub last_error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 单一货币的账号成本聚合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountCost {
    pub currency: String,
    pub amount: super::observability::DecimalAmount,
}

/// 模型价格与真实上游费用的独立 USD 聚合，计数用于标明部分覆盖。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountBillingAmounts {
    pub model_price_usd: Option<super::observability::DecimalAmount>,
    pub upstream_cost_usd: Option<super::observability::DecimalAmount>,
    pub model_price_count: u64,
    pub upstream_cost_count: u64,
}

/// 保留完整模型身份，不能按请求名或路由名合并不同的计价模型。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountModelIdentity {
    pub key: String,
    pub requested_model_id: Option<String>,
    pub upstream_model_id: Option<String>,
    pub response_model: Option<String>,
    pub billing_model: Option<String>,
}

/// 账号在一个模型组合上的历史用量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountModelUsage {
    pub identity: AccountModelIdentity,
    pub billing: AccountBillingAmounts,
    pub model: String,
    pub request_count: u64,
    pub success_count: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub image_input_tokens: Option<u64>,
    pub image_output_tokens: Option<u64>,
    pub image_request_count: u64,
    pub image_request_failed_count: u64,
    pub total_tokens: Option<u64>,
    pub cost_coverage: super::observability::CostCoverage,
    pub costs: Vec<AccountCost>,
    pub last_used_at: DateTime<Utc>,
}

/// 账号在一个小时窗口内的请求数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRequestBucket {
    pub bucket_start: DateTime<Utc>,
    pub request_count: u64,
}

/// 账号历史用量聚合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountUsage {
    pub billing: AccountBillingAmounts,
    pub account_id: String,
    pub request_count: u64,
    pub success_count: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub image_input_tokens: Option<u64>,
    pub image_output_tokens: Option<u64>,
    pub image_request_count: u64,
    pub image_request_failed_count: u64,
    pub total_tokens: Option<u64>,
    pub cost_coverage: super::observability::CostCoverage,
    pub costs: Vec<AccountCost>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub request_buckets: Vec<AccountRequestBucket>,
    pub models: Vec<AccountModelUsage>,
}

/// 某个账号在调用方指定时间窗口内的本地用量查询。
///
/// `key` 只用于把聚合结果关联回调用方的窗口，不承载 Provider 私有语义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountUsageWindowQuery {
    pub account_id: String,
    pub key: String,
    pub range: TimeRange,
}

/// 一个账号时间窗口用量查询的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountUsageWindowResult {
    pub account_id: String,
    pub key: String,
    pub usage: AccountUsage,
}

/// 账号列表页所需的完整存储事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountPage {
    pub config_revision: Revision,
    pub items: Vec<AccountPageItem>,
    pub total: u64,
    pub summary: AccountSummary,
}

/// 同一状态快照下的账号事实与唯一状态投影。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountPageItem {
    pub account: AccountRecord,
    pub projection: AccountStatusProjection,
}

/// 统一账号目录的全局状态计数，不受当前筛选和分页影响。
///
/// 计数与 [`AccountStatus`] 一一对应，由 store 按派生状态聚合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountSummary {
    pub total: u64,
    pub normal: u64,
    pub quota_exhausted: u64,
    pub rate_limited: u64,
    pub disabled: u64,
    pub error: u64,
}

/// 账号可编辑事实的一次性替换命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateAccount {
    pub account_id: String,
    /// 缺省保留备注；空字符串清空备注。
    pub notes: Option<String>,
    pub enabled: bool,
    pub concurrency_limit: Option<AccountConcurrencyLimit>,
    pub weight: AccountWeight,
    pub model_access: Option<gateway_core::account::AccountModelAccess>,
    pub group_ids: Vec<gateway_core::routing::AccountGroupId>,
    pub outbound_proxy: Option<super::proxies::AccountProxySelection>,
}

/// 账号更新结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountUpdateResult {
    pub config_revision: Revision,
    pub account_id: gateway_core::account::ProviderAccountId,
}

/// 仅修改显式字段的批量账号设置命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchUpdateAccounts {
    pub account_ids: Vec<String>,
    pub enabled: Option<bool>,
    pub concurrency_limit: Option<Option<AccountConcurrencyLimit>>,
    pub weight: Option<AccountWeight>,
    pub model_access: Option<gateway_core::account::AccountModelAccess>,
    pub group_ids: Option<Vec<gateway_core::routing::AccountGroupId>>,
    pub outbound_proxy: Option<super::proxies::AccountProxySelection>,
}

/// 批量账号更新结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountsUpdateResult {
    pub config_revision: Revision,
    pub account_ids: Vec<gateway_core::account::ProviderAccountId>,
}

/// 账号批量删除命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteAccounts {
    pub account_ids: Vec<String>,
}

/// 账号连接测试的语义事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountConnectionTestEvent {
    Started {
        model: String,
    },
    Request {
        model: String,
        input_text: String,
        stream: bool,
        store: bool,
    },
    Content {
        text: String,
    },
    Completed,
    Failed {
        source: AccountProbeErrorSource,
        gateway_error_code: GatewayErrorKind,
        send_state: Option<UpstreamSendState>,
        message: String,
        provider_error_code: Option<String>,
        provider_error_type: Option<String>,
        upstream_status: Option<u16>,
        upstream_content_type: Option<String>,
        upstream_body: Option<String>,
    },
}

/// 每次连接测试独占的有限事件流。
pub type AccountConnectionTestEventStream =
    Pin<Box<dyn Stream<Item = AccountConnectionTestEvent> + Send + 'static>>;

/// 遍历代理找 state 的命令；`attempts` 是每个出口最多发出的真实上游请求数。
#[derive(Debug, Clone)]
pub struct TurnStateHuntCommand {
    pub account_id: gateway_core::account::ProviderAccountId,
    pub upstream_model: gateway_core::routing::UpstreamModelId,
    pub attempts: u8,
    pub include_direct: bool,
    /// 只在这一个代理上遍历。轮换出口（每次请求换一个 IP）值得单独打上百次，
    /// 而固定出口同一个 IP 反复打没有意义，所以高次数只应落在指定的那一个上。
    pub only_proxy_id: Option<String>,
    /// 后台续期为 true：账号被停用或凭据失效后，开始前与每个出口前都会停手。
    pub require_schedulable: bool,
    /// 自动撞：不遍历已存代理，而是从一条轮换代理模板即时生成 `count` 个「一 IP 一条唯一
    /// session」的临时出口（多国随机）。`Some` 时忽略 `only_proxy_id` 与已存代理列表。
    /// 每个临时出口只打 1 次：同一地址第二次会复用连接并被上游 strip 掉 turn-state。
    pub ephemeral: Option<EphemeralHunt>,
    /// 命中后改绑到的静态出口 id；`None` 时沿用命中的那个出口（普通遍历的既有行为）。
    /// 自动撞必须给它：命中在轮换 IP 上，但要落到稳定的静态家宽发请求（state 已确认可移植）。
    pub bind_to: Option<String>,
    pub context: super::MutationContext,
}

/// 自动撞的高层请求：凭据（模板代理地址）与静态出口的选取由 use case 层解析后
/// 再落成一条带 `ephemeral`/`bind_to` 的 [`TurnStateHuntCommand`]。
#[derive(Debug, Clone)]
pub struct TurnStateAutoHuntRequest {
    pub account_id: gateway_core::account::ProviderAccountId,
    pub upstream_model: gateway_core::routing::UpstreamModelId,
    /// 轮换代理模板的已存代理 id。
    pub template_proxy_id: String,
    pub countries: Vec<HuntCountry>,
    /// 命中后可改绑的静态出口候选 id；use case 取其中测试通过、账号数最少的一个。
    pub static_proxy_ids: Vec<String>,
    pub max_ips: u16,
    pub context: super::MutationContext,
}

/// 自动撞的临时出口生成参数。
#[derive(Debug, Clone)]
pub struct EphemeralHunt {
    /// 轮换代理模板地址（含凭据）。其用户名里的 `_area-XX`/`_session-...` 段会被替换为
    /// 目标国家与一次性 session；没有 `_area-` 段时直接追加。仅支持 smartproxy 式下划线用户名。
    pub template_url: String,
    /// 每次随机从中选一个国家出口。非空，取值见 [`HuntCountry`]。
    pub countries: Vec<HuntCountry>,
    /// 生成多少个临时 IP（= 最多试多少个出口）。命中即止。
    pub count: u16,
}

/// 自动撞支持的出口国家。值是 smartproxy 的 `_area-` 代码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HuntCountry {
    Us,
    Jp,
    De,
    Ph,
}

impl HuntCountry {
    #[must_use]
    pub const fn area_code(self) -> &'static str {
        match self {
            Self::Us => "US",
            Self::Jp => "JP",
            Self::De => "DE",
            Self::Ph => "PH",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "US" => Some(Self::Us),
            "JP" => Some(Self::Jp),
            "DE" => Some(Self::De),
            "PH" => Some(Self::Ph),
            _ => None,
        }
    }
}

impl TurnStateHuntCommand {
    pub const MAX_ATTEMPTS: u8 = 200;
    /// 自动撞最多生成的临时 IP 数上限（额度闸）。
    pub const MAX_EPHEMERAL_IPS: u16 = 2000;
}

/// 遍历中的一个出口；`proxy_id == None` 表示直连。`endpoint` 不含代理凭据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnStateHuntEgress {
    pub proxy_id: Option<String>,
    pub name: String,
    pub endpoint: Option<String>,
}

/// 单次探测失败的可公开事实；不含上游正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnStateHuntAttemptError {
    pub code: GatewayErrorKind,
    pub source: AccountProbeErrorSource,
    pub upstream_status: Option<u16>,
    pub message: String,
}

/// 遍历代理找 state 的语义事件。任何事件都只携带 state 的字节数，绝不携带其值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnStateHuntEvent {
    Started {
        model: String,
        expected_length: usize,
        attempts: u8,
        egresses: Vec<TurnStateHuntEgress>,
    },
    EgressStarted {
        egress: TurnStateHuntEgress,
        index: usize,
        total: usize,
    },
    Attempt {
        proxy_id: Option<String>,
        index: u8,
        length: Option<usize>,
        matched: bool,
        error: Option<TurnStateHuntAttemptError>,
    },
    EgressFinished {
        proxy_id: Option<String>,
        attempts: u8,
        matched: bool,
        skipped: Option<&'static str>,
    },
    Hit {
        proxy_id: Option<String>,
        attempt_index: u8,
        length: usize,
    },
    Bound {
        proxy_id: Option<String>,
        changed: bool,
    },
    Pinned {
        model: String,
        length: usize,
        expires_at: DateTime<Utc>,
    },
    Completed {
        success: bool,
        requests: u32,
    },
    Failed {
        code: &'static str,
        message: String,
    },
}

/// 每次遍历独占的有限事件流。
pub type TurnStateHuntEventStream =
    Pin<Box<dyn Stream<Item = TurnStateHuntEvent> + Send + 'static>>;
