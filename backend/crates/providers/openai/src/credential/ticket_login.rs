//! 票据登录：把 `邮箱/密码/2FA 密钥` 交给同机内网的登录 sidecar，换回账号的新 OAuth 令牌。
//!
//! sidecar 只在 compose 内部网络可达，用共享令牌鉴权；登录走账号在 CPR 绑定的出口，
//! 避免登录 IP 与日常请求出口不一致触发风控。票据与令牌都不进日志。

use std::{fmt, time::Duration};

use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

/// 一次完整的密码 + 2FA 登录（含 Sentinel PoW）可能要一两分钟。
const LOGIN_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

pub(crate) struct TicketLoginClient {
    http: reqwest::Client,
    endpoint: String,
    token: SecretString,
}

pub(crate) struct TicketLoginRequest<'a> {
    pub email: &'a str,
    pub password: &'a str,
    pub totp_secret: &'a str,
    pub proxy_url: Option<&'a str>,
}

pub(crate) struct TicketLoginTokens {
    pub access_token: SecretString,
    pub refresh_token: Option<SecretString>,
    pub id_token: Option<SecretString>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TicketLoginError {
    #[error("ticket login service is unreachable")]
    Unreachable,
    #[error("ticket login was rejected: {0}")]
    Rejected(String),
    #[error("ticket login service returned an invalid response")]
    InvalidResponse,
}

impl TicketLoginError {
    /// sidecar 只返回固定错误码；这里映射成固定文案，上游原文不会透传到管理端。
    pub(crate) fn public_message(&self) -> &'static str {
        match self {
            Self::Unreachable => "票据登录服务暂不可用",
            Self::InvalidResponse => "票据登录服务返回了无法识别的结果",
            Self::Rejected(code) => match code.as_str() {
                "invalid_credentials" => "票据登录失败：邮箱或密码错误",
                "totp_failed" => "票据登录失败：2FA 验证码未通过，请检查 2FA 密钥",
                "challenge_required" => "票据登录失败：触发了人机验证，请稍后重试或手动重新授权",
                "proxy_error" => "票据登录失败：账号绑定的出口代理不可用",
                "timeout" => "票据登录超时，请稍后重试",
                "busy" => "票据登录服务正忙，请稍后重试",
                _ => "票据登录失败，请稍后重试或手动重新授权",
            },
        }
    }
}

impl fmt::Debug for TicketLoginClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TicketLoginClient")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct LoginResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    error: Option<String>,
}

impl TicketLoginClient {
    /// # Errors
    ///
    /// HTTP 客户端无法构建时返回错误。
    pub(crate) fn new(base_url: &str, token: SecretString) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .timeout(LOGIN_TIMEOUT)
            // sidecar 在内部网络，不经任何出口代理。
            .no_proxy()
            .build()?;
        Ok(Self {
            http,
            endpoint: format!("{}/v1/login", base_url.trim_end_matches('/')),
            token,
        })
    }

    pub(crate) async fn login(
        &self,
        request: TicketLoginRequest<'_>,
    ) -> Result<TicketLoginTokens, TicketLoginError> {
        let response = self
            .http
            .post(&self.endpoint)
            .bearer_auth(self.token.expose_secret())
            .json(&serde_json::json!({
                "email": request.email,
                "password": request.password,
                "totp_secret": request.totp_secret,
                "proxy": request.proxy_url,
            }))
            .send()
            .await
            .map_err(|_| TicketLoginError::Unreachable)?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|_| TicketLoginError::Unreachable)?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(TicketLoginError::InvalidResponse);
        }
        let parsed: LoginResponse =
            serde_json::from_slice(&bytes).map_err(|_| TicketLoginError::InvalidResponse)?;
        if !status.is_success() {
            // sidecar 只返回固定错误码，不含票据或上游原文。
            let code = parsed
                .error
                .filter(|code| {
                    code.len() <= 64
                        && code
                            .bytes()
                            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
                })
                .unwrap_or_else(|| "login_failed".to_owned());
            return Err(TicketLoginError::Rejected(code));
        }
        let access_token = parsed
            .access_token
            .filter(|token| !token.is_empty())
            .ok_or(TicketLoginError::InvalidResponse)?;
        let non_empty = |value: Option<String>| {
            value
                .filter(|token| !token.is_empty())
                .map(SecretString::from)
        };
        Ok(TicketLoginTokens {
            access_token: SecretString::from(access_token),
            refresh_token: non_empty(parsed.refresh_token),
            id_token: non_empty(parsed.id_token),
        })
    }
}
