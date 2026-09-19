//! revive-api HTTP 客户端。令牌只放请求头，错误文本脱敏。

use std::time::Duration;

use reqwest::{Client, StatusCode, header};
use serde_json::Value;
use tokio::time::sleep;

use crate::config::CodexReviveSettings;

const GZIP_THRESHOLD: usize = 256 * 1024;
const TOKEN_HEADER: &str = "X-Revive-Task-Token";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReviveClientError {
    #[error("revive-api request failed")]
    Transport,
    #[error("revive-api rejected the signed export")]
    Rejected,
    #[error("revive-api rate limited")]
    RateLimited,
    #[error("revive verification snapshot expired")]
    Expired,
    #[error("revive-api returned an invalid response")]
    InvalidResponse,
    #[error("revive job timed out")]
    Timeout,
}

pub struct ReviveApiClient {
    http: Client,
    settings: CodexReviveSettings,
    poll_interval: Duration,
    max_wait: Duration,
}

impl std::fmt::Debug for ReviveApiClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReviveApiClient")
            .field("base_url", &self.settings.base_url)
            .field("poll_interval", &self.poll_interval)
            .finish_non_exhaustive()
    }
}

impl ReviveApiClient {
    pub fn new(settings: CodexReviveSettings) -> Result<Self, ReviveClientError> {
        let http = Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10))
            .user_agent("cpr-revive/1.0")
            .build()
            .map_err(|_| ReviveClientError::Transport)?;
        Ok(Self {
            http,
            settings,
            poll_interval: Duration::from_secs(2),
            max_wait: Duration::from_secs(1800),
        })
    }

    pub fn with_timing(mut self, poll_interval: Duration, max_wait: Duration) -> Self {
        self.poll_interval = poll_interval;
        self.max_wait = max_wait;
        self
    }

    pub async fn recover_signed_export(&self, document: &[u8]) -> Result<Value, ReviveClientError> {
        let job = self.start_verify(document).await?;
        tracing::info!(
            job_id = %job.job_id,
            document_bytes = document.len(),
            "Guanlan revive verify started"
        );
        let token = job.task_token.clone();
        let job = self
            .poll_job(&job.job_id, &token, "/verify/", &["completed", "failed"])
            .await?;
        if job.status == "failed" {
            // last_error 是观澜的拒绝原因（如签名/记录数不符），不含凭据；截断后落日志。
            tracing::warn!(
                job_id = %job.job_id,
                reason = %log_text(job.last_error.as_deref()),
                "Guanlan revive verify rejected the signed export"
            );
            return Err(ReviveClientError::Rejected);
        }
        tracing::info!(
            job_id = %job.job_id,
            total = job.total_count.unwrap_or(0),
            normal = job.normal_count.unwrap_or(0),
            unauthorized = job.unauthorized_count.unwrap_or(0),
            counts_present = job.unauthorized_count.is_some(),
            "Guanlan revive verify completed"
        );
        if job.unauthorized_count.unwrap_or(0) == 0 {
            return Ok(serde_json::json!({ "accounts": [] }));
        }
        // result 视图可能不带统计计数，因此保留 summary 的最终 unauthorized_count。
        let detail_url = format!(
            "{}/verify/{}?result=1",
            self.settings.base_url.trim_end_matches('/'),
            job.job_id
        );
        let detail = self.get_json(&detail_url, &token).await?;
        let preflight_id = detail
            .preflight_id
            .clone()
            .ok_or(ReviveClientError::InvalidResponse)?;
        let task = self.start_task(document, &preflight_id).await?;
        tracing::info!(task_id = %task.job_id, "Guanlan revive task started");
        let token = if task.task_token.is_empty() {
            token
        } else {
            task.task_token
        };
        let task = self
            .poll_job(
                &task.job_id,
                &token,
                "/tasks/",
                &["normal", "recovered", "partial", "failed", "stopped"],
            )
            .await?;
        let download_ready =
            task.download_ready.unwrap_or(false) || task.all_download_ready.unwrap_or(false);
        tracing::info!(
            task_id = %task.job_id,
            status = %task.status,
            success = task.success_count.unwrap_or(0),
            failure = task.failure_count.unwrap_or(0),
            download_ready,
            reason = %log_text(task.last_error.as_deref()),
            "Guanlan revive task finished"
        );
        if !download_ready {
            return Err(ReviveClientError::InvalidResponse);
        }
        self.download_task(&task.job_id, &token).await
    }

    async fn start_verify(&self, document: &[u8]) -> Result<JobView, ReviveClientError> {
        let url = format!(
            "{}/verify/start?workers={}",
            self.settings.base_url.trim_end_matches('/'),
            self.settings.verify_workers
        );
        self.post_signed(&url, document).await
    }

    async fn start_task(
        &self,
        document: &[u8],
        preflight_id: &str,
    ) -> Result<JobView, ReviveClientError> {
        let url = format!(
            "{}/tasks?preflight_id={preflight_id}&workers={}&auto_start=1",
            self.settings.base_url.trim_end_matches('/'),
            self.settings.task_workers
        );
        self.post_signed(&url, document).await
    }

    async fn poll_job(
        &self,
        id: &str,
        token: &str,
        path_prefix: &str,
        done: &[&str],
    ) -> Result<JobView, ReviveClientError> {
        let url = if path_prefix == "/verify/" {
            format!(
                "{}{path_prefix}{id}?summary=1",
                self.settings.base_url.trim_end_matches('/')
            )
        } else {
            format!(
                "{}{path_prefix}{id}",
                self.settings.base_url.trim_end_matches('/')
            )
        };
        let deadline = tokio::time::Instant::now() + self.max_wait;
        loop {
            let view = self.get_json(&url, token).await?;
            if done.iter().any(|status| *status == view.status) {
                return Ok(view);
            }
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(job_id = %id, status = %view.status, "Guanlan revive poll timed out");
                return Err(ReviveClientError::Timeout);
            }
            sleep(self.poll_interval).await;
        }
    }

    async fn download_task(&self, task_id: &str, token: &str) -> Result<Value, ReviveClientError> {
        let url = format!(
            "{}/tasks/{task_id}/download?scope=recovered&format=json",
            self.settings.base_url.trim_end_matches('/')
        );
        let response = self
            .http
            .get(url)
            .header(TOKEN_HEADER, token)
            .send()
            .await
            .map_err(|_| ReviveClientError::Transport)?;
        check_status("download", response.status())?;
        response
            .json()
            .await
            .map_err(|_| ReviveClientError::InvalidResponse)
    }

    async fn post_signed(&self, url: &str, document: &[u8]) -> Result<JobView, ReviveClientError> {
        let mut request = self
            .http
            .post(url)
            .header(header::CONTENT_TYPE, "application/json");
        if document.len() >= GZIP_THRESHOLD {
            let compressed = gzip(document)?;
            request = request
                .header(header::CONTENT_ENCODING, "gzip")
                .body(compressed);
        } else {
            request = request.body(document.to_vec());
        }
        let response = request
            .send()
            .await
            .map_err(|_| ReviveClientError::Transport)?;
        check_status("submit", response.status())?;
        let envelope: JobEnvelope = response
            .json()
            .await
            .map_err(|_| ReviveClientError::InvalidResponse)?;
        envelope.into_view()
    }

    async fn get_json(&self, url: &str, token: &str) -> Result<JobView, ReviveClientError> {
        let response = self
            .http
            .get(url)
            .header(TOKEN_HEADER, token)
            .send()
            .await
            .map_err(|_| ReviveClientError::Transport)?;
        check_status("poll", response.status())?;
        let envelope: JobEnvelope = response
            .json()
            .await
            .map_err(|_| ReviveClientError::InvalidResponse)?;
        envelope.into_view()
    }
}

fn gzip(bytes: &[u8]) -> Result<Vec<u8>, ReviveClientError> {
    use std::io::Write as _;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(bytes)
        .map_err(|_| ReviveClientError::Transport)?;
    encoder.finish().map_err(|_| ReviveClientError::Transport)
}

fn map_status(status: StatusCode) -> Result<(), ReviveClientError> {
    match status.as_u16() {
        200 | 202 => Ok(()),
        429 => Err(ReviveClientError::RateLimited),
        410 => Err(ReviveClientError::Expired),
        400 | 404 | 409 => Err(ReviveClientError::Rejected),
        _ => Err(ReviveClientError::Transport),
    }
}

/// 失败时只记录阶段与状态码；响应正文可能回显凭据，不落日志。
fn check_status(stage: &'static str, status: StatusCode) -> Result<(), ReviveClientError> {
    map_status(status).inspect_err(|error| {
        tracing::warn!(
            stage,
            http_status = status.as_u16(),
            error = %error,
            "Guanlan revive-api returned an error status"
        );
    })
}

const LOG_TEXT_LIMIT: usize = 200;

/// 观澜的失败原因是定位问题的关键线索（签名不符、记录数不符等），保留；
/// 但它是第三方自由文本，出现 JWT 或长凭据样字符串时整条不落日志。
fn log_text(text: Option<&str>) -> String {
    let text = text.unwrap_or_default();
    let credential_like = text.contains("eyJ")
        || text
            .split(|c: char| {
                !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '/' | '='))
            })
            .any(|run| run.len() >= 40);
    if credential_like {
        return "<redacted>".to_owned();
    }
    text.chars()
        .filter(|c| !c.is_control())
        .take(LOG_TEXT_LIMIT)
        .collect()
}

#[derive(Debug, serde::Deserialize)]
struct JobEnvelope {
    #[serde(default)]
    job: Option<JobBody>,
    #[serde(default)]
    task: Option<JobBody>,
}

#[derive(Debug, serde::Deserialize)]
struct JobBody {
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    task_token: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    preflight_id: Option<String>,
    #[serde(default)]
    unauthorized_count: Option<u64>,
    #[serde(default)]
    total_count: Option<u64>,
    #[serde(default)]
    normal_count: Option<u64>,
    #[serde(default)]
    success_count: Option<u64>,
    #[serde(default)]
    failure_count: Option<u64>,
    #[serde(default)]
    last_error: Option<String>,
    #[serde(default)]
    download_ready: Option<bool>,
    #[serde(default)]
    all_download_ready: Option<bool>,
}

struct JobView {
    job_id: String,
    task_token: String,
    status: String,
    preflight_id: Option<String>,
    unauthorized_count: Option<u64>,
    total_count: Option<u64>,
    normal_count: Option<u64>,
    success_count: Option<u64>,
    failure_count: Option<u64>,
    last_error: Option<String>,
    download_ready: Option<bool>,
    all_download_ready: Option<bool>,
}

impl JobEnvelope {
    fn into_view(self) -> Result<JobView, ReviveClientError> {
        let body = self
            .job
            .or(self.task)
            .ok_or(ReviveClientError::InvalidResponse)?;
        let job_id = body
            .job_id
            .or(body.task_id)
            .ok_or(ReviveClientError::InvalidResponse)?;
        Ok(JobView {
            job_id,
            task_token: body.task_token.unwrap_or_default(),
            status: body.status.unwrap_or_default(),
            preflight_id: body.preflight_id,
            unauthorized_count: body.unauthorized_count,
            total_count: body.total_count,
            normal_count: body.normal_count,
            success_count: body.success_count,
            failure_count: body.failure_count,
            last_error: body.last_error,
            download_ready: body.download_ready,
            all_download_ready: body.all_download_ready,
        })
    }
}

impl std::fmt::Debug for JobView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JobView")
            .field("job_id", &self.job_id)
            .field("status", &self.status)
            .field("task_token", &"<redacted>")
            .finish_non_exhaustive()
    }
}
