//! 管理端账号连通性测试的真实执行链端口。

use std::fmt;

use bytes::Bytes;
use futures::future::BoxFuture;

use crate::engine::DiagnosticEgress;
use crate::error::{
    ClientVisibleUpstreamResponse, GatewayError, GatewayErrorKind, ProviderErrorKind,
};
use crate::event::ProviderResponseHeader;
use crate::identity::ProviderKind;
use crate::routing::UpstreamModelId;
use crate::{account::ProviderAccountId, operation::Operation, upstream::UpstreamSendState};

/// 固定账号测试的策略；连接检查和代理遍历保持原始诊断语义。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountProbeMode {
    #[default]
    Diagnostic,
    Business,
}

/// 只返回当前尝试的安全事实；缺少观测为 None，不把未知误报成未使用。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountProbeExecution {
    pub ticket_attached: Option<bool>,
    pub warm_pool_used: Option<bool>,
    pub warm_verified: Option<bool>,
    pub connection_reused: Option<bool>,
    pub connection_id: Option<String>,
    pub transport: Option<String>,
}

impl AccountProbeExecution {
    pub(crate) fn from_trace(trace: Option<serde_json::Value>) -> Self {
        let mut result = Self::default();
        let Some(trace) = trace else { return result };
        for event in trace
            .get("events")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let data = &event["data"];
            match event["stage"].as_str() {
                Some("attempt.started") => result = Self::default(),
                Some("upstream.turn_state") => result.ticket_attached = data["attached"].as_bool(),
                Some("upstream.exchange.started") => {
                    result.connection_id = None;
                    result.connection_reused = None;
                    result.warm_verified = None;
                    result.transport = data["transport"]
                        .as_str()
                        .filter(|v| matches!(*v, "websocket" | "http_sse" | "http"))
                        .map(str::to_owned);
                    result.warm_pool_used = result
                        .transport
                        .as_deref()
                        .filter(|v| *v != "websocket")
                        .map(|_| false);
                }
                Some("upstream.connection") => {
                    result.transport = Some("websocket".to_owned());
                    result.connection_reused = data["reused"].as_bool();
                    result.warm_verified = data["warmVerified"].as_bool();
                    result.warm_pool_used =
                        data.get("warmVerified").map(serde_json::Value::is_boolean);
                    result.connection_id = data["connectionId"]
                        .as_str()
                        .filter(|id| {
                            id.len() <= 128
                                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                        })
                        .map(str::to_owned);
                }
                _ => {}
            }
        }
        result
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountProbeRequest {
    pub mode: AccountProbeMode,
    pub account_id: ProviderAccountId,
    pub provider_kind: ProviderKind,
    pub upstream_model: UpstreamModelId,
    pub operation: Operation,
    /// 临时出口；`None` 沿用账号已绑定的代理（连接测试的既有行为）。
    pub egress: Option<DiagnosticEgress>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountProbeResult {
    pub execution: Option<AccountProbeExecution>,
    pub text: Vec<String>,
    /// 上游响应头，仅供 Provider 管理端在进程内解读；`Debug` 已脱敏，不得序列化。
    pub response_headers: Vec<ProviderResponseHeader>,
}

/// 仅供当前管理端连接测试展示的原始上游失败响应。
///
/// 正文不进入 `Debug`、日志或持久化；Core 只在探测终态从原始 Provider 错误显式复制。
#[derive(PartialEq, Eq)]
pub struct AccountProbeUpstreamResponse {
    status: u16,
    content_type: Option<Vec<u8>>,
    body: Bytes,
}

impl AccountProbeUpstreamResponse {
    pub(crate) fn from_client_response(response: &ClientVisibleUpstreamResponse) -> Self {
        Self {
            status: response.status(),
            content_type: response.content_type().map(<[u8]>::to_vec),
            body: response.body().clone(),
        }
    }

    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    #[must_use]
    pub fn content_type(&self) -> Option<&[u8]> {
        self.content_type.as_deref()
    }

    #[must_use]
    pub const fn body(&self) -> &Bytes {
        &self.body
    }
}

impl fmt::Debug for AccountProbeUpstreamResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccountProbeUpstreamResponse")
            .field("status", &self.status)
            .field(
                "content_type",
                &self.content_type.as_ref().map(|_| "<present>"),
            )
            .field("body", &"<redacted>")
            .finish()
    }
}

/// 连接测试失败发生的稳定责任边界。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountProbeErrorSource {
    Gateway,
    Provider,
    Upstream,
}

impl AccountProbeErrorSource {
    /// 返回管理端 wire 使用的稳定值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Provider => "provider",
            Self::Upstream => "upstream",
        }
    }
}

/// 管理端连接测试的终态错误。
///
/// `gateway` 保留稳定分类，`upstream_response` 只面向本次认证管理请求展示源响应。
#[derive(Debug)]
pub struct AccountProbeError {
    gateway: GatewayError,
    source: AccountProbeErrorSource,
    send_state: Option<UpstreamSendState>,
    upstream_response: Option<AccountProbeUpstreamResponse>,
    provider_kind: Option<ProviderErrorKind>,
    execution: Option<AccountProbeExecution>,
}

impl AccountProbeError {
    #[must_use]
    pub fn with_execution(mut self, execution: AccountProbeExecution) -> Self {
        self.execution = Some(execution);
        self
    }

    #[must_use]
    pub fn execution(&self) -> Option<&AccountProbeExecution> {
        self.execution.as_ref()
    }

    #[must_use]
    pub const fn new(
        gateway: GatewayError,
        source: AccountProbeErrorSource,
        send_state: Option<UpstreamSendState>,
        upstream_response: Option<AccountProbeUpstreamResponse>,
    ) -> Self {
        Self {
            gateway,
            source,
            send_state,
            upstream_response,
            provider_kind: None,
            execution: None,
        }
    }

    /// 附上 Provider 的原始失败分类。
    ///
    /// 面向客户端的 [`GatewayErrorKind`] 会把凭据失效、无权限与上游不可用折叠成同一类；
    /// 管理端要区分「账号的问题」和「出口的问题」，只能看这里。
    #[must_use]
    pub const fn with_provider_kind(mut self, kind: Option<ProviderErrorKind>) -> Self {
        self.provider_kind = kind;
        self
    }

    #[must_use]
    pub const fn provider_kind(&self) -> Option<ProviderErrorKind> {
        self.provider_kind
    }

    #[must_use]
    pub const fn kind(&self) -> GatewayErrorKind {
        self.gateway.kind()
    }

    #[must_use]
    pub const fn source(&self) -> AccountProbeErrorSource {
        self.source
    }

    #[must_use]
    pub const fn send_state(&self) -> Option<UpstreamSendState> {
        self.send_state
    }

    #[must_use]
    pub fn client_message(&self) -> &str {
        self.gateway.client_message()
    }

    #[must_use]
    pub fn client_error_type(&self) -> Option<&str> {
        self.gateway.client_error_type()
    }

    #[must_use]
    pub fn client_error_code(&self) -> Option<&str> {
        self.gateway.client_error_code()
    }

    #[must_use]
    pub const fn upstream_response(&self) -> Option<&AccountProbeUpstreamResponse> {
        self.upstream_response.as_ref()
    }
}

impl From<GatewayError> for AccountProbeError {
    fn from(gateway: GatewayError) -> Self {
        Self::new(gateway, AccountProbeErrorSource::Gateway, None, None)
    }
}

pub trait AccountProbe: Send + Sync {
    /// 插件管理操作传入其冻结快照；其他调用可使用当前发布视图。
    fn probe(
        &self,
        request: AccountProbeRequest,
        snapshot: Option<std::sync::Arc<crate::routing::RuntimeSnapshot>>,
    ) -> BoxFuture<'_, Result<AccountProbeResult, AccountProbeError>>;
}
