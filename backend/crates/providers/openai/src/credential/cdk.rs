//! 向 zzledu CDK 兑换接口取回已签名号池，再走现有 OpenAI 导入。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::{Client, StatusCode, header};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::CodexCdkSettings;

const CLIENT_FILE: &str = "client.json";
const USER_AGENT: &str = "cpr-cdk/1.0";
const CLIENT_HEADER: &str = "X-Cdk-Client";
const CLIENT_VERSION_HEADER: &str = "X-Cdk-Client-Version";
const DOWNLOAD_TOKEN_HEADER: &str = "X-Cdk-Download-Token";
const MAX_CDK_BATCH: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodexCdkError {
    #[error("cdk client identity is unavailable")]
    Identity,
    #[error("cdk redeem request failed")]
    Transport,
    #[error("{0}")]
    Rejected(String),
    #[error("cdk redeem was rate limited")]
    RateLimited,
    #[error("cdk redeem returned an invalid response")]
    InvalidResponse,
}

impl CodexCdkError {
    #[must_use]
    pub fn public_message(&self) -> &str {
        match self {
            Self::Identity => "无法创建 CDK 兑换身份，请检查运行数据目录权限",
            Self::Transport => "暂时无法连接 CDK 兑换服务，请稍后重试",
            Self::Rejected(message) => message,
            Self::RateLimited => "CDK 兑换被限流，请稍后重试",
            Self::InvalidResponse => "CDK 兑换服务返回了无法识别的结果",
        }
    }
}

pub struct CodexCdkClient {
    http: Client,
    settings: CodexCdkSettings,
    client_id: String,
}

impl std::fmt::Debug for CodexCdkClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexCdkClient")
            .field("base_url", &self.settings.base_url)
            .field("client_version", &self.settings.client_version)
            .finish_non_exhaustive()
    }
}

impl CodexCdkClient {
    pub fn new(data_dir: PathBuf, settings: CodexCdkSettings) -> Result<Self, CodexCdkError> {
        fs::create_dir_all(&data_dir).map_err(|_| CodexCdkError::Identity)?;
        let http = Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(USER_AGENT)
            .build()
            .map_err(|_| CodexCdkError::Transport)?;
        let client_id = load_or_create_client_id(&data_dir, &settings.client_id)?;
        Ok(Self {
            http,
            settings,
            client_id,
        })
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.settings.enabled
    }

    pub async fn redeem_export(&self, cdks: &[String]) -> Result<Value, CodexCdkError> {
        if cdks.is_empty() || cdks.len() > MAX_CDK_BATCH {
            return Err(CodexCdkError::Rejected("请提供 1 到 200 个 CDK".to_owned()));
        }
        if !self.settings.enabled {
            return Err(CodexCdkError::Rejected(
                "未启用 CDK 兑换，请在 openai.auth.cdk.enabled 打开".to_owned(),
            ));
        }
        let ticket = match self.redeem(cdks, false).await {
            Ok(ticket) => ticket,
            Err(CodexCdkError::Rejected(message)) if message.contains("已兑换") => {
                self.redeem(cdks, true).await?
            }
            Err(error) => return Err(error),
        };
        self.download(&ticket).await
    }

    async fn redeem(&self, cdks: &[String], recover: bool) -> Result<RedeemTicket, CodexCdkError> {
        let url = format!(
            "{}/api/cdk/redeem",
            self.settings.base_url.trim_end_matches('/')
        );
        let mut body = serde_json::json!({ "cdks": cdks });
        if recover {
            body["download"] = Value::Bool(true);
        }
        let response = self
            .http
            .post(url)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json")
            .header(CLIENT_HEADER, &self.client_id)
            .header(CLIENT_VERSION_HEADER, &self.settings.client_version)
            .header("Idempotency-Key", Uuid::now_v7().to_string())
            .json(&body)
            .send()
            .await
            .map_err(|_| CodexCdkError::Transport)?;
        let status = response.status();
        let payload = response
            .json::<RedeemResponse>()
            .await
            .map_err(|_| CodexCdkError::InvalidResponse)?;
        if matches!(status.as_u16(), 429) {
            return Err(CodexCdkError::RateLimited);
        }
        if !payload.ok || !status.is_success() {
            return Err(map_redeem_failure(status, &payload));
        }
        payload.into_ticket()
    }

    async fn download(&self, ticket: &RedeemTicket) -> Result<Value, CodexCdkError> {
        let url = format!(
            "{}/api/cdk/redemptions/{}/download",
            self.settings.base_url.trim_end_matches('/'),
            ticket.redemption_id
        );
        let response = self
            .http
            .get(url)
            .header(header::ACCEPT, "application/json")
            .header(CLIENT_HEADER, &self.client_id)
            .header(CLIENT_VERSION_HEADER, &self.settings.client_version)
            .header(DOWNLOAD_TOKEN_HEADER, &ticket.download_token)
            .send()
            .await
            .map_err(|_| CodexCdkError::Transport)?;
        if !response.status().is_success() {
            return Err(CodexCdkError::Rejected(
                "CDK 账号文件下载失败或已过期".to_owned(),
            ));
        }
        let document = response
            .json::<Value>()
            .await
            .map_err(|_| CodexCdkError::InvalidResponse)?;
        if document.get("accounts").and_then(Value::as_array).is_none() {
            return Err(CodexCdkError::InvalidResponse);
        }
        Ok(document)
    }
}

/// 顶层 `cdks` 且没有账号数组时，视为 CDK 兑换请求。
pub fn extract_cdk_codes(value: &Value) -> Result<Option<Vec<String>>, CodexCdkError> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if object
        .get("accounts")
        .and_then(Value::as_array)
        .is_some_and(|accounts| !accounts.is_empty())
    {
        return Ok(None);
    }
    let Some(raw) = object.get("cdks") else {
        return Ok(None);
    };
    let Some(items) = raw.as_array() else {
        return Err(CodexCdkError::Rejected("cdks 必须是字符串数组".to_owned()));
    };
    if items.is_empty() || items.len() > MAX_CDK_BATCH {
        return Err(CodexCdkError::Rejected("请提供 1 到 200 个 CDK".to_owned()));
    }
    let mut codes = Vec::with_capacity(items.len());
    for item in items {
        let Some(code) = item
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Err(CodexCdkError::Rejected("CDK 格式不正确".to_owned()));
        };
        if !is_cdk_code(code) {
            return Err(CodexCdkError::Rejected("CDK 格式不正确".to_owned()));
        }
        codes.push(code.to_ascii_uppercase());
    }
    Ok(Some(codes))
}

pub fn is_cdk_code(value: &str) -> bool {
    let value = value.trim();
    let Some(rest) = value
        .strip_prefix("CDK-")
        .or_else(|| value.strip_prefix("cdk-"))
    else {
        return false;
    };
    let groups: Vec<&str> = rest.split('-').collect();
    groups.len() == 8
        && groups
            .iter()
            .all(|group| group.len() == 4 && group.bytes().all(|byte| byte.is_ascii_alphanumeric()))
}

fn load_or_create_client_id(data_dir: &Path, configured: &str) -> Result<String, CodexCdkError> {
    let configured = configured.trim();
    if !configured.is_empty() {
        return Ok(configured.to_owned());
    }
    let path = data_dir.join(CLIENT_FILE);
    if let Ok(raw) = fs::read_to_string(&path)
        && let Ok(stored) = serde_json::from_str::<StoredClient>(&raw)
        && stored.client_id.starts_with("r1-")
        && stored.client_id.len() == 67
    {
        return Ok(stored.client_id);
    }
    let client_id = generate_client_id()?;
    let payload = serde_json::to_vec(&StoredClient {
        client_id: client_id.clone(),
    })
    .map_err(|_| CodexCdkError::Identity)?;
    fs::write(path, payload).map_err(|_| CodexCdkError::Identity)?;
    Ok(client_id)
}

fn generate_client_id() -> Result<String, CodexCdkError> {
    let mut seed = [0_u8; 32];
    getrandom::fill(&mut seed).map_err(|_| CodexCdkError::Identity)?;
    Ok(format!("r1-{}", hex::encode(Sha256::digest(seed))))
}

fn map_redeem_failure(status: StatusCode, payload: &RedeemResponse) -> CodexCdkError {
    if status.as_u16() == 429 {
        return CodexCdkError::RateLimited;
    }
    let message = match payload.error_code.as_deref() {
        Some("invalid_format") => "CDK 格式不正确",
        Some("not_found") => "CDK 不存在或无效",
        Some("already_redeemed") => "CDK 已兑换且无法再次取回，请改用已下载的 JSON 导入",
        _ => payload
            .error
            .as_deref()
            .filter(|value| {
                let lowered = value.to_ascii_lowercase();
                !lowered.contains("cdk-") && !lowered.contains("token")
            })
            .unwrap_or("供应商拒绝了 CDK 兑换"),
    };
    CodexCdkError::Rejected(message.to_owned())
}

#[derive(Debug, Deserialize)]
struct RedeemResponse {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    status: String,
    #[serde(default)]
    redemption_id: Option<String>,
    #[serde(default)]
    download_token: Option<String>,
    #[serde(default)]
    downloads: Vec<RedeemDownload>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RedeemDownload {
    #[serde(default)]
    redemption_id: Option<String>,
    #[serde(default)]
    download_token: Option<String>,
}

struct RedeemTicket {
    redemption_id: String,
    download_token: String,
}

impl RedeemResponse {
    fn into_ticket(self) -> Result<RedeemTicket, CodexCdkError> {
        if !matches!(self.status.as_str(), "redeemed" | "recovered") {
            return Err(CodexCdkError::InvalidResponse);
        }
        let first = self.downloads.first();
        let redemption_id = self
            .redemption_id
            .or_else(|| first.and_then(|item| item.redemption_id.clone()))
            .filter(|value| !value.is_empty())
            .ok_or(CodexCdkError::InvalidResponse)?;
        let download_token = self
            .download_token
            .or_else(|| first.and_then(|item| item.download_token.clone()))
            .filter(|value| !value.is_empty())
            .ok_or(CodexCdkError::InvalidResponse)?;
        Ok(RedeemTicket {
            redemption_id,
            download_token,
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredClient {
    client_id: String,
}
