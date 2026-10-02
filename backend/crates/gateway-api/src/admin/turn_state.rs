//! turn-state 模板、运行设置与观测的管理接口。
//!
//! 服务由组合根注入（`SessionState::turn_state`），不经过 ProviderAdmin：这些接口只读写
//! `turn_state` crate 自己的文件存储，与账号领域无关。管理路由只用 GET/POST 静态路径。

use std::time::SystemTime;

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use turn_state::{
    BucketSummary, CloudMintSettings, InjectMode, ObservationSnapshot, ServedMismatchAction,
    Settings, TurnStateError, TurnStateService, WarmPoolSettings,
};

use super::{AdminAuth, AdminEnvelope, AdminError, AdminJson, AdminQuery, AdminResponse};
use crate::auth::SessionState;

/// 整体替换运行设置；字段全部必填，避免部分提交把未提及的项静默重置成默认值。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateTurnStateSettingsRequest {
    ttl_seconds: u64,
    inject_mode: InjectMode,
    dry_run: bool,
    log_decisions: bool,
    template_lengths: Vec<usize>,
    degraded_lengths: Vec<usize>,
    /// 省略时保留现有的换模型处置。
    #[serde(default)]
    served_mismatch_action: Option<ServedMismatchAction>,
    /// 省略时保留现有云端打票设置；`relayKey` 为 `<set>` 占位时沿用磁盘上的密钥。
    #[serde(default)]
    cloud_mint: Option<CloudMintSettingsPatch>,
    /// 省略时保留现有 WS 保活设置。
    #[serde(default)]
    warm_pool: Option<WarmPoolSettingsPatch>,
}

/// 旧客户端不认识严格模式时，编辑其他预热参数不能把已开启的保护清掉。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct WarmPoolSettingsPatch {
    #[serde(default)]
    require_verified: Option<bool>,
    #[serde(flatten)]
    settings: WarmPoolSettings,
}

/// 旧客户端省略专用代理时保留已保存值，显式空字符串才表示清除。
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct CloudMintSettingsPatch {
    #[serde(default)]
    upstream_proxy_url: Option<String>,
    #[serde(flatten)]
    settings: CloudMintSettings,
}

impl std::fmt::Debug for CloudMintSettingsPatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CloudMintSettingsPatch")
            .field(
                "upstream_proxy_provided",
                &self.upstream_proxy_url.is_some(),
            )
            .field("settings", &self.settings)
            .finish()
    }
}

impl UpdateTurnStateSettingsRequest {
    #[must_use]
    pub fn into_settings(self, current: &Settings) -> Settings {
        Settings {
            ttl_seconds: self.ttl_seconds,
            inject_mode: self.inject_mode,
            dry_run: self.dry_run,
            log_decisions: self.log_decisions,
            template_lengths: self.template_lengths,
            degraded_lengths: self.degraded_lengths,
            served_mismatch_action: self
                .served_mismatch_action
                .unwrap_or(current.served_mismatch_action),
            cloud_mint: self
                .cloud_mint
                .map(|patch| CloudMintSettings {
                    upstream_proxy_url: patch
                        .upstream_proxy_url
                        .unwrap_or_else(|| current.cloud_mint.upstream_proxy_url.clone()),
                    ..patch.settings
                })
                .unwrap_or_else(|| current.cloud_mint.clone()),
            warm_pool: self
                .warm_pool
                .map(|patch| WarmPoolSettings {
                    require_verified: patch
                        .require_verified
                        .unwrap_or(current.warm_pool.require_verified),
                    ..patch.settings
                })
                .unwrap_or_else(|| current.warm_pool.clone()),
        }
        .merge_secret_placeholders(current)
    }
}

/// 桶列表过滤条件。
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BucketsQuery {
    account: Option<String>,
    model: Option<String>,
}

/// 清除某账号（可限定模型）的全部模板。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClearBucketsRequest {
    account: String,
    model: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BucketsView {
    now: u64,
    buckets: Vec<BucketSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClearedView {
    cleared: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ObservationsView {
    now: u64,
    #[serde(flatten)]
    snapshot: ObservationSnapshot,
}

/// 构造固定 GET/POST turn-state 管理路由。
pub fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/turn-state/settings", get(settings::<S>))
        .route(
            "/api/admin/turn-state/settings/update",
            post(update_settings::<S>),
        )
        .route("/api/admin/turn-state/observations", get(observations::<S>))
        .route("/api/admin/turn-state/buckets", get(buckets::<S>))
        .route(
            "/api/admin/turn-state/buckets/clear",
            post(clear_buckets::<S>),
        )
}

fn service<S: SessionState>(state: &S) -> Result<&TurnStateService, AdminError> {
    state
        .turn_state()
        .ok_or_else(AdminError::service_unavailable)
}

fn map_error(error: TurnStateError) -> AdminError {
    match error {
        TurnStateError::Settings(error) => AdminError::bad_request(error.to_string()),
        TurnStateError::Io => AdminError::service_unavailable(),
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

async fn settings<S>(
    _auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let settings = service(&state)?.settings().redacted();
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(settings),
    ))
}

async fn update_settings<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminJson(payload): AdminJson<UpdateTurnStateSettingsRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let service = service(&state)?;
    let current = service.settings();
    let requested = payload.into_settings(&current);
    for proxy in [
        &requested.cloud_mint.upstream_proxy_url,
        &requested.cloud_mint.proxy_url,
    ] {
        if !proxy.trim().is_empty()
            && gateway_core::account::OutboundProxy::parse(proxy.trim()).is_err()
        {
            return Err(AdminError::bad_request(
                "代理地址格式不正确，请使用 HTTP、HTTPS 或 SOCKS5 地址",
            ));
        }
    }
    let saved = service
        .update_settings(requested)
        .map_err(map_error)?
        .redacted();
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(saved)))
}

async fn observations<S>(
    _auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let snapshot = service(&state)?.observations(SystemTime::now());
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ObservationsView {
            now: unix_now(),
            snapshot,
        }),
    ))
}

async fn buckets<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<BucketsQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let buckets = service(&state)?
        .buckets(SystemTime::now())
        .into_iter()
        .filter(|bucket| {
            query
                .account
                .as_deref()
                .is_none_or(|account| bucket.account == account)
                && query
                    .model
                    .as_deref()
                    .is_none_or(|model| bucket.model == model)
        })
        .collect();
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(BucketsView {
            now: unix_now(),
            buckets,
        }),
    ))
}

async fn clear_buckets<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminJson(payload): AdminJson<ClearBucketsRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    if payload.account.trim().is_empty() {
        return Err(AdminError::bad_request("account 不能为空"));
    }
    let cleared = service(&state)?.clear_bucket(&payload.account, payload.model.as_deref());
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ClearedView { cleared }),
    ))
}
