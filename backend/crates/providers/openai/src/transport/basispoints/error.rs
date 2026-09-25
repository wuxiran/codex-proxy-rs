//! BPS 协议转换的错误类型。

use std::fmt;

/// 协议转换阶段的错误。provider 层据此决定客户端响应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PrepareError {
    /// 本地 400 `basispoints_request_invalid`，携带诊断消息（不回显客户端正文）。
    Invalid(String),
    /// 该请求 BPS 无法处理，provider 应回落原生 Codex（第一版：json_schema 结构化输出等）。
    RouteNative(&'static str),
}

impl PrepareError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

impl fmt::Display for PrepareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "{message}"),
            Self::RouteNative(reason) => write!(formatter, "route to native codex: {reason}"),
        }
    }
}
