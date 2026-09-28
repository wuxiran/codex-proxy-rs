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
const IDEMPOTENCY_HEADER: &str = "Idempotency-Key";
/// 观澜下载格式：`sub2api` = 已签名 JSON（导入与 401 复活都依赖它）；`cpa` 是 ZIP，不用。
const DOWNLOAD_FORMAT: &str = "sub2api";
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

    /// 兑换（或找回）CDK，按空间逐个下载已签名的账号文件。
    ///
    /// 观澜 20260922 起一批 CDK 可能分属多个空间，每个空间一个独立签名的文件；
    /// 这里按服务端顺序全部返回，调用方逐份归档、导入，不能合并成一份（合并会破坏签名）。
    pub async fn redeem_export(&self, cdks: &[String]) -> Result<Vec<Value>, CodexCdkError> {
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
        let mut documents = Vec::with_capacity(ticket.downloads.len());
        for download in &ticket.downloads {
            documents.push(self.download(download).await?);
            // 回执只是告诉观澜「文件已送达」，失败不影响本次导入（观澜会在下次对账时补齐）。
            self.acknowledge(&ticket.idempotency_key, download).await;
        }
        Ok(documents)
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
        let idempotency_key = Uuid::now_v7().to_string();
        let response = self
            .http
            .post(url)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json")
            .header(CLIENT_HEADER, &self.client_id)
            .header(CLIENT_VERSION_HEADER, &self.settings.client_version)
            .header(IDEMPOTENCY_HEADER, &idempotency_key)
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
        log_skipped_codes(&payload.details);
        payload.into_ticket(idempotency_key)
    }

    async fn download(&self, download: &RedeemDownloadTicket) -> Result<Value, CodexCdkError> {
        let url = format!(
            "{}/api/cdk/redemptions/{}/download?format={DOWNLOAD_FORMAT}",
            self.settings.base_url.trim_end_matches('/'),
            download.redemption_id
        );
        let response = self
            .http
            .get(url)
            .header(header::ACCEPT, "application/json")
            .header(CLIENT_HEADER, &self.client_id)
            .header(CLIENT_VERSION_HEADER, &self.settings.client_version)
            .header(DOWNLOAD_TOKEN_HEADER, &download.download_token)
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
        // 与观澜兑换页一致：文件必须带非空账号数组，且不能是 `ok: false` 的错误体。
        let has_accounts = document
            .get("accounts")
            .and_then(Value::as_array)
            .is_some_and(|accounts| !accounts.is_empty());
        if !has_accounts || document.get("ok").and_then(Value::as_bool) == Some(false) {
            return Err(CodexCdkError::InvalidResponse);
        }
        Ok(document)
    }

    /// 接收回执（观澜 `durable_delivery_receipts`）：与兑换请求同一 Idempotency-Key。
    async fn acknowledge(&self, idempotency_key: &str, download: &RedeemDownloadTicket) {
        let url = format!(
            "{}/api/cdk/redemptions/{}/received",
            self.settings.base_url.trim_end_matches('/'),
            download.redemption_id
        );
        let result = self
            .http
            .post(url)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json")
            .header(CLIENT_HEADER, &self.client_id)
            .header(CLIENT_VERSION_HEADER, &self.settings.client_version)
            .header(IDEMPOTENCY_HEADER, idempotency_key)
            .body("{}")
            .send()
            .await;
        match result {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => tracing::warn!(
                status = response.status().as_u16(),
                redemption_id = %download.redemption_id,
                "cdk delivery receipt was not accepted"
            ),
            Err(_) => tracing::warn!(
                redemption_id = %download.redemption_id,
                "cdk delivery receipt could not be sent"
            ),
        }
    }
}

/// 部分 CDK 未生成（已撤销、不存在等）时只记状态分布，不落兑换码本身。
fn log_skipped_codes(details: &[RedeemDetail]) {
    let skipped: Vec<&str> = details
        .iter()
        .map(|detail| detail.status.as_str())
        .filter(|status| !matches!(*status, "redeemed" | "recovered"))
        .collect();
    if !skipped.is_empty() {
        tracing::warn!(
            skipped = skipped.len(),
            statuses = ?skipped,
            "some cdks were not redeemed"
        );
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
    // 观澜兑换协议升级后，旧版本客户端会被 409 拒绝且「本次未执行兑换」。
    // 协议只改版本号时可在配置里跟进；协议有变化时需要升级 cpr。
    if payload.error_code.as_deref() == Some("client_update_required") {
        let required = payload
            .cdk_client_version
            .as_deref()
            .filter(|version| is_client_version(version))
            .unwrap_or("未知");
        return CodexCdkError::Rejected(format!(
            "观澜 CDK 兑换接口已升级（要求客户端版本 {required}），本次未执行兑换；\
             请升级 cpr，或确认协议兼容后把 openai.auth.cdk.client_version 改为该版本"
        ));
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

/// 观澜回传的版本号只在符合其兑换页同款格式时才展示给管理员。
fn is_client_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// 兑换记录 ID 会拼进 URL 路径，只接受观澜兑换页同款的安全字符。
fn is_redemption_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[derive(Debug, Deserialize)]
struct RedeemResponse {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    redemption_id: Option<String>,
    #[serde(default)]
    download_token: Option<String>,
    #[serde(default)]
    downloads: Vec<RedeemDownload>,
    #[serde(default)]
    details: Vec<RedeemDetail>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    cdk_client_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RedeemDownload {
    #[serde(default)]
    redemption_id: Option<String>,
    #[serde(default)]
    download_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RedeemDetail {
    #[serde(default)]
    status: String,
}

struct RedeemDownloadTicket {
    redemption_id: String,
    download_token: String,
}

struct RedeemTicket {
    idempotency_key: String,
    downloads: Vec<RedeemDownloadTicket>,
}

impl RedeemResponse {
    /// 与观澜兑换页一致：成功与否只看 `ok`，文件清单优先取 `downloads`（多空间），
    /// 没有时退回顶层单文件字段；两者都没有完整下载信息即视为无效响应。
    fn into_ticket(self, idempotency_key: String) -> Result<RedeemTicket, CodexCdkError> {
        let mut downloads: Vec<RedeemDownloadTicket> = self
            .downloads
            .into_iter()
            .filter_map(|item| ticket_from(item.redemption_id, item.download_token))
            .collect();
        if downloads.is_empty()
            && let Some(single) = ticket_from(self.redemption_id, self.download_token)
        {
            downloads.push(single);
        }
        if downloads.is_empty() {
            return Err(CodexCdkError::InvalidResponse);
        }
        Ok(RedeemTicket {
            idempotency_key,
            downloads,
        })
    }
}

fn ticket_from(
    redemption_id: Option<String>,
    download_token: Option<String>,
) -> Option<RedeemDownloadTicket> {
    let redemption_id = redemption_id?.trim().to_owned();
    let download_token = download_token?.trim().to_owned();
    if !is_redemption_id(&redemption_id) || download_token.is_empty() {
        return None;
    }
    Some(RedeemDownloadTicket {
        redemption_id,
        download_token,
    })
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredClient {
    client_id: String,
}
