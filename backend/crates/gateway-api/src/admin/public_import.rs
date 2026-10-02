//! 免登录导入入口的管理配置：开关、目标分组、state 绑定与令牌轮换。

use std::collections::BTreeSet;

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use gateway_admin::model::public_import::{PublicImportConfig, UpdatePublicImportConfig};
use gateway_core::routing::AccountGroupId;
use serde::{Deserialize, Serialize};

use crate::auth::SessionState;

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminJson, AdminResponse, WireValidationError,
    wire::map_admin_service_error,
};

const MAX_GROUPS: usize = 50;

pub fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/public-import", get(config::<S>))
        .route("/api/admin/public-import/update", post(update::<S>))
        .route(
            "/api/admin/public-import/rotate-token",
            post(rotate_token::<S>),
        )
}

/// 令牌明文返回给管理员用于拼接密链，与部署级 Admin API Key 的展示合同一致。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicImportConfigView {
    pub enabled: bool,
    pub token: String,
    pub group_ids: Vec<String>,
    pub pin_turn_state: bool,
    pub expires_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

impl From<PublicImportConfig> for PublicImportConfigView {
    fn from(config: PublicImportConfig) -> Self {
        Self {
            enabled: config.enabled,
            token: config.token,
            group_ids: config.group_ids.iter().map(ToString::to_string).collect(),
            pin_turn_state: config.pin_turn_state,
            expires_at: config.expires_at,
            updated_at: config.updated_at,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdatePublicImportRequest {
    pub enabled: bool,
    pub group_ids: Vec<String>,
    pub pin_turn_state: bool,
    /// RFC 3339 时间；`null` 表示长期有效。字段必填，避免漏传时静默清掉有效期。
    #[serde(deserialize_with = "required_nullable")]
    pub expires_at: Option<DateTime<Utc>>,
}

/// 带 `deserialize_with` 的 Option 字段缺失时报错，而显式 `null` 仍解析为 `None`。
fn required_nullable<'de, D>(deserializer: D) -> Result<Option<DateTime<Utc>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<DateTime<Utc>>::deserialize(deserializer)
}

impl UpdatePublicImportRequest {
    fn into_command(self) -> Result<UpdatePublicImportConfig, WireValidationError> {
        if self.group_ids.len() > MAX_GROUPS
            || self.group_ids.iter().collect::<BTreeSet<_>>().len() != self.group_ids.len()
        {
            return Err(WireValidationError::new("groupIds"));
        }
        Ok(UpdatePublicImportConfig {
            enabled: self.enabled,
            group_ids: self
                .group_ids
                .into_iter()
                .map(|id| AccountGroupId::new(id).map_err(|_| WireValidationError::new("groupIds")))
                .collect::<Result<_, _>>()?,
            pin_turn_state: self.pin_turn_state,
            expires_at: self.expires_at,
        })
    }
}

fn map_wire_error(error: WireValidationError) -> AdminError {
    AdminError::bad_request(format!("{} 字段不合法", error.field()))
}

async fn config<S>(
    _auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let config = state
        .admin_services()
        .public_import()
        .config()
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(PublicImportConfigView::from(config)),
    ))
}

async fn update<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<UpdatePublicImportRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let command = request.into_command().map_err(map_wire_error)?;
    let config = state
        .admin_services()
        .public_import()
        .update(&auth.context().mutation_context(), command)
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(PublicImportConfigView::from(config)),
    ))
}

async fn rotate_token<S>(
    auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let config = state
        .admin_services()
        .public_import()
        .rotate_token(&auth.context().mutation_context())
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(PublicImportConfigView::from(config)),
    ))
}
