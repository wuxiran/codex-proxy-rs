//! Provider 管理能力与动态注册表。

use std::{collections::BTreeMap, sync::Arc, time::SystemTime};

use async_trait::async_trait;
use gateway_core::{
    account::ProviderAccountId,
    event::ProviderResponseHeader,
    operation::Operation,
    routing::{ProviderKind, UpstreamModelId},
};
use heck::ToUpperCamelCase;

use crate::model::observability::{
    CalculatedBillingBreakdown, DashboardWireProfile, ProviderBillingInput,
};
use crate::model::provider_credentials::{
    AuthorizationStarted, CompleteAuthorization, ConsumeProviderResetCredit,
    PendingAuthorizationMutation, PrepareCredentialImport, PrepareCredentialRefresh,
    PrepareCredentialRotation, PreparedAuthorizationCommit, PreparedCredentialImport,
    PreparedCredentialRotation, ProviderExport, ProviderExportCredentialInput, ProviderModels,
    ProviderProfileAvatar, ProviderProfileStatistics, ProviderQuota, ProviderQuotaRequest,
    ProviderResetCreditResult, ProviderResetCredits, ProviderSubscription, explicit_plan_type,
};
use crate::model::{
    provider_credentials::{ProviderDocument, ProviderQuotaWindow},
    quota_forecast_sampling::QuotaForecastObservation,
};

/// Provider 管理失败的稳定分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAdminErrorKind {
    Invalid,
    Unsupported,
    NotFound,
    Conflict,
    /// Provider 已发起不可逆操作，但无法确认最终执行结果。
    Ambiguous,
    Unavailable,
    CredentialRefreshRequired,
    BadGateway,
    Internal,
}

/// 不携带 OAuth 请求材料的管理错误。
///
/// `message` 是 Provider 局部诊断，可能包含原始上游正文；通用管理用例不得自动把它作为公开文案。
/// 只有明确拥有原始诊断合同的调用方才能读取，`Debug` 始终只记录是否存在。
/// `public_message` 则是明确标记为可公开的静态提示，不允许携带动态上游材料。
#[derive(Clone, PartialEq, Eq, thiserror::Error)]
#[error("provider admin operation failed: {kind:?}")]
pub struct ProviderAdminError {
    kind: ProviderAdminErrorKind,
    message: Option<String>,
    public_message: Option<&'static str>,
}

impl std::fmt::Debug for ProviderAdminError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderAdminError")
            .field("kind", &self.kind)
            .field("message", &self.message.as_ref().map(|_| "<redacted>"))
            .field("public_message", &self.public_message)
            .finish()
    }
}

impl ProviderAdminError {
    #[must_use]
    pub const fn new(kind: ProviderAdminErrorKind) -> Self {
        Self {
            kind,
            message: None,
            public_message: None,
        }
    }

    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// Provider 明确允许公开的静态提示；不得承载上游响应或凭据材料。
    #[must_use]
    pub const fn with_public_message(mut self, message: &'static str) -> Self {
        self.public_message = Some(message);
        self
    }

    #[must_use]
    pub const fn public_message(&self) -> Option<&'static str> {
        self.public_message
    }

    #[must_use]
    pub const fn kind(&self) -> ProviderAdminErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
}

/// 一次遍历代理找 state 冻结的 Provider 事实。
///
/// `binding` 由 Provider 从凭据派生，对控制面不透明；`Debug` 不展开它。
#[derive(Clone, PartialEq, Eq)]
pub struct TurnStateHuntTicket {
    account_id: ProviderAccountId,
    upstream_model: UpstreamModelId,
    expected_length: usize,
    binding: String,
}

impl TurnStateHuntTicket {
    #[must_use]
    pub const fn new(
        account_id: ProviderAccountId,
        upstream_model: UpstreamModelId,
        expected_length: usize,
        binding: String,
    ) -> Self {
        Self {
            account_id,
            upstream_model,
            expected_length,
            binding,
        }
    }

    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn upstream_model(&self) -> &UpstreamModelId {
        &self.upstream_model
    }

    #[must_use]
    pub const fn expected_length(&self) -> usize {
        self.expected_length
    }

    #[must_use]
    pub fn binding(&self) -> &str {
        &self.binding
    }
}

impl std::fmt::Debug for TurnStateHuntTicket {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TurnStateHuntTicket")
            .field("account_id", &self.account_id)
            .field("upstream_model", &self.upstream_model)
            .field("expected_length", &self.expected_length)
            .field("binding", &"<redacted>")
            .finish()
    }
}

/// 一个账号级 state 临近到期、需要重新遍历代理续期的账号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnStateRenewal {
    pub account_id: ProviderAccountId,
    pub upstream_model: UpstreamModelId,
    pub attempts: u8,
    pub include_direct: bool,
}

/// 单次探测观测到的 state 形状；不含 state 值。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TurnStateHuntObservation {
    pub length: Option<usize>,
    pub matched: bool,
}

/// 一个具体 Provider 对管理控制面提供的解析、验证、上游交互与运行时资源回收能力。
///
/// 数据变更由 Provider 返回 prepared facts；config revision、审计与 PostgreSQL 事务
/// 全部由 [`crate::ports::store::AccountStore`] 提交。运行时资源通知只在事务成功后发生。
#[async_trait]
pub trait ProviderAdmin: Send + Sync {
    fn provider_kind(&self) -> &ProviderKind;

    /// 将原始套餐值投影为展示名称；默认保留未知 Provider 的原始名称。
    fn plan_type_display(&self, plan_type: &str) -> String {
        plan_type.to_owned()
    }

    /// 账号已经由控制面提交为不可调度状态，释放 Provider 持有的账号级运行时资源。
    ///
    /// 无账号级运行时资源的 Provider 不需要执行额外操作。该通知发生在 Store 事务
    /// 成功之后，不参与事务成败，也不得恢复或改写已经提交的账号状态。
    async fn account_unavailable(&self, account_id: &ProviderAccountId);

    /// 账号资格事实已经由控制面提交，失效 Provider 持有的可重建派生状态。
    ///
    /// 该通知发生在 Store 事务成功之后、下一份 RuntimeSnapshot 编译之前；通知
    /// 不参与已提交事务成败。没有账号派生状态的 Provider 可使用默认空实现。
    async fn account_facts_changed(&self, _account_ids: &[ProviderAccountId]) {}

    /// 生成一次连接测试所需的 Provider-owned operation；Core 负责实际执行与落账。
    fn connection_test_operation(
        &self,
        upstream_model: &UpstreamModelId,
        input_text: &str,
    ) -> Result<Operation, ProviderAdminError>;

    /// 校验账号可以遍历代理找 state，并冻结本次遍历的长度规则与凭据绑定。
    ///
    /// 命中后再次调用并与首张票据比较，即可发现遍历期间凭据被刷新或重新捕获。
    async fn turn_state_hunt_prepare(
        &self,
        _account_id: &ProviderAccountId,
        _upstream_model: &UpstreamModelId,
    ) -> Result<TurnStateHuntTicket, ProviderAdminError> {
        Err(ProviderAdminError::new(ProviderAdminErrorKind::Unsupported))
    }

    /// 只报告上游返回的 state 字节数与是否符合规则；state 值不离开 Provider。
    fn turn_state_hunt_inspect(
        &self,
        _ticket: &TurnStateHuntTicket,
        _response_headers: &[ProviderResponseHeader],
    ) -> TurnStateHuntObservation {
        TurnStateHuntObservation::default()
    }

    /// 把刚观测到的 state 钉为账号级 state，返回其到期时间。
    async fn turn_state_hunt_pin(
        &self,
        _ticket: &TurnStateHuntTicket,
        _response_headers: &[ProviderResponseHeader],
        _captured_at: SystemTime,
    ) -> Result<SystemTime, ProviderAdminError> {
        Err(ProviderAdminError::new(ProviderAdminErrorKind::Unsupported))
    }

    /// 已开启自动续期、且本进程内账号级 state 缺失或将在 `margin` 内到期的账号。
    ///
    /// state 只存在于进程内存；服务重启后全部视为缺失，由续期任务重新找回。
    async fn turn_state_hunt_renewals(
        &self,
        _now: SystemTime,
        _margin: std::time::Duration,
    ) -> Vec<TurnStateRenewal> {
        Vec::new()
    }

    /// 返回该 Provider 实际持有的 Dashboard 上游身份画像。
    fn dashboard_wire_profile(&self) -> Option<DashboardWireProfile>;

    /// 使用 Provider-owned 价格规则恢复持久请求的逐项费用。
    fn calculated_billing(
        &self,
        input: &ProviderBillingInput,
    ) -> Result<Option<CalculatedBillingBreakdown>, ProviderAdminError>;

    async fn prepare_import(
        &self,
        command: PrepareCredentialImport,
    ) -> Result<PreparedCredentialImport, ProviderAdminError>;

    async fn start_authorization(
        &self,
        pending: PendingAuthorizationMutation,
    ) -> Result<AuthorizationStarted, ProviderAdminError>;

    async fn complete_authorization(
        &self,
        command: CompleteAuthorization,
    ) -> Result<PreparedAuthorizationCommit, ProviderAdminError>;

    async fn prepare_rotation(
        &self,
        command: PrepareCredentialRotation,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError>;

    async fn prepare_refresh(
        &self,
        command: PrepareCredentialRefresh,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError>;

    /// 可公开的账号连接设置；实现只能显式投影非敏感字段，不能返回凭据原文。
    async fn account_configuration(
        &self,
        _account_id: &ProviderAccountId,
    ) -> Result<Option<ProviderDocument>, ProviderAdminError> {
        Ok(None)
    }

    async fn quota(
        &self,
        request: ProviderQuotaRequest,
    ) -> Result<ProviderQuota, ProviderAdminError>;

    /// 历史观测的协议字段仅由具体 Provider 解释；不支持时保留累计估算。
    fn quota_forecast_observation(
        &self,
        _document: &ProviderDocument,
        _window: &ProviderQuotaWindow,
    ) -> Option<QuotaForecastObservation> {
        None
    }

    /// 查询 Provider 官方个人资料统计；不支持该能力的 Provider 使用默认拒绝。
    /// 只由个人信息显式查询，不在额度刷新或后台任务中预取。
    async fn subscription(
        &self,
        _account_id: &ProviderAccountId,
    ) -> Result<Option<ProviderSubscription>, ProviderAdminError> {
        Err(ProviderAdminError::new(ProviderAdminErrorKind::Unsupported))
    }

    async fn profile_statistics(
        &self,
        _account_id: &ProviderAccountId,
    ) -> Result<ProviderProfileStatistics, ProviderAdminError> {
        Err(ProviderAdminError::new(ProviderAdminErrorKind::Unsupported))
    }

    /// 打开 Provider 官方头像字节流；不支持该能力的 Provider 使用默认拒绝。
    async fn profile_avatar(
        &self,
        _account_id: &ProviderAccountId,
    ) -> Result<ProviderProfileAvatar, ProviderAdminError> {
        Err(ProviderAdminError::new(ProviderAdminErrorKind::Unsupported))
    }

    /// 查询 Provider 主动额度重置卡；不支持该能力的 Provider 使用默认拒绝。
    async fn reset_credits(
        &self,
        _account_id: &ProviderAccountId,
    ) -> Result<ProviderResetCredits, ProviderAdminError> {
        Err(ProviderAdminError::new(ProviderAdminErrorKind::Unsupported))
    }

    /// 消费 Provider 主动额度重置卡；不支持该能力的 Provider 使用默认拒绝。
    async fn consume_reset_credit(
        &self,
        _command: ConsumeProviderResetCredit,
    ) -> Result<ProviderResetCreditResult, ProviderAdminError> {
        Err(ProviderAdminError::new(ProviderAdminErrorKind::Unsupported))
    }

    async fn models(
        &self,
        account_id: &ProviderAccountId,
        refresh: bool,
    ) -> Result<ProviderModels, ProviderAdminError>;

    async fn export_credentials(
        &self,
        credentials: Vec<ProviderExportCredentialInput>,
    ) -> Result<ProviderExport, ProviderAdminError>;
}

/// 按 ProviderKind 动态发现管理能力；不含具体 Provider 分支。
#[derive(Clone)]
pub struct ProviderAdminRegistry {
    providers: Arc<BTreeMap<ProviderKind, Arc<dyn ProviderAdmin>>>,
}

impl ProviderAdminRegistry {
    /// 创建无重复 ProviderKind 的注册表。
    ///
    /// # Errors
    ///
    /// 重复注册同一 ProviderKind 时返回 Conflict。
    pub fn new(
        providers: impl IntoIterator<Item = Arc<dyn ProviderAdmin>>,
    ) -> Result<Self, ProviderAdminError> {
        let mut registered = BTreeMap::new();
        for provider in providers {
            let kind = provider.provider_kind().clone();
            if registered.insert(kind, provider).is_some() {
                return Err(ProviderAdminError::new(ProviderAdminErrorKind::Conflict));
            }
        }
        Ok(Self {
            providers: Arc::new(registered),
        })
    }

    pub fn require(
        &self,
        provider_kind: &ProviderKind,
    ) -> Result<Arc<dyn ProviderAdmin>, ProviderAdminError> {
        self.providers
            .get(provider_kind)
            .cloned()
            .ok_or_else(|| ProviderAdminError::new(ProviderAdminErrorKind::Unsupported))
    }

    /// 账号目录与关联列表共用套餐补全和展示规则，已知账号套餐优先于额度快照。
    pub(crate) fn resolve_account_plan(
        &self,
        provider_kind: &str,
        plan_type: &mut Option<String>,
        quota: Option<&ProviderQuota>,
    ) -> Option<String> {
        if let Some(quota) = quota {
            quota.fill_missing_plan_type(plan_type);
        }
        self.plan_type_display(provider_kind, plan_type.as_deref())
    }

    /// 账号页和 Dashboard 共用的大驼峰套餐展示名称，不修改原始套餐值。
    pub(crate) fn plan_type_display(
        &self,
        provider_kind: &str,
        plan_type: Option<&str>,
    ) -> Option<String> {
        let plan_type = explicit_plan_type(plan_type)?.trim();
        let provider = ProviderKind::new(provider_kind.to_owned())
            .ok()
            .and_then(|kind| self.providers.get(&kind));
        let display = provider.map_or_else(
            || plan_type.to_owned(),
            |provider| provider.plan_type_display(plan_type),
        );
        // 所有 Provider 及未知套餐统一排版，保留 Plus 后缀的含义。
        Some(display.replace('+', " Plus ").to_upper_camel_case())
    }

    /// 返回所有已注册 Provider 的 Dashboard 上游身份画像。
    /// 汇总各 Provider 需要续期账号级 state 的账号。
    pub(crate) async fn turn_state_hunt_renewals(
        &self,
        now: SystemTime,
        margin: std::time::Duration,
    ) -> Vec<TurnStateRenewal> {
        let mut due = Vec::new();
        for provider in self.providers.values() {
            due.extend(provider.turn_state_hunt_renewals(now, margin).await);
        }
        due
    }

    pub fn dashboard_wire_profiles(&self) -> Vec<DashboardWireProfile> {
        self.providers
            .values()
            .filter_map(|provider| provider.dashboard_wire_profile())
            .collect()
    }

    /// 动态分派 Provider-owned 费用规则，不含任何具体 Provider 分支。
    pub fn calculated_billing(
        &self,
        provider_kind: &ProviderKind,
        input: &ProviderBillingInput,
    ) -> Result<Option<CalculatedBillingBreakdown>, ProviderAdminError> {
        self.require(provider_kind)?.calculated_billing(input)
    }
}
