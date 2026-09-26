//! 免登录导入入口的管理配置：每个号商一条（名字、开关、目标分组、state 绑定、有效期、令牌轮换）。

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
        .route("/api/admin/public-import", get(list::<S>))
        .route("/api/admin/public-import/create", post(create::<S>))
        .route("/api/admin/public-import/update", post(update::<S>))
        .route("/api/admin/public-import/delete", post(delete::<S>))
        .route(
            "/api/admin/public-import/rotate-token",
            post(rotate_token::<S>),
        )
}

/// 令牌明文返回给管理员用于拼接密链，与部署级 Admin API Key 的展示合同一致。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicImportConfigView {
    pub id: String,
    pub name: String,
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
            id: config.id,
            name: config.name,
            enabled: config.enabled,
            token: config.token,
            group_ids: config.group_ids.iter().map(ToString::to_string).collect(),
            pin_turn_state: config.pin_turn_state,
            expires_at: config.expires_at,
            updated_at: config.updated_at,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicImportListView {
    pub configs: Vec<PublicImportConfigView>,
}

/// 新建/修改一个号商配置的公共字段。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpsertPublicImportRequest {
    pub name: String,
    pub enabled: bool,
    pub group_ids: Vec<String>,
    pub pin_turn_state: bool,
    /// RFC 3339 时间；`null` 表示长期有效。字段必填，避免漏传时静默清掉有效期。
    #[serde(deserialize_with = "required_nullable")]
    pub expires_at: Option<DateTime<Utc>>,
}

/// 修改需带目标 id。不用 `flatten`（serde 的 flatten 与 deny_unknown_fields 不兼容），字段直接展开。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdatePublicImportRequest {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub group_ids: Vec<String>,
    pub pin_turn_state: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub expires_at: Option<DateTime<Utc>>,
}

impl UpdatePublicImportRequest {
    fn split(self) -> Result<(String, UpdatePublicImportConfig), WireValidationError> {
        let command = UpsertPublicImportRequest {
            name: self.name,
            enabled: self.enabled,
            group_ids: self.group_ids,
            pin_turn_state: self.pin_turn_state,
            expires_at: self.expires_at,
        }
        .into_command()?;
        Ok((self.id, command))
    }
}

/// 删除 / 轮换令牌只需 id。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicImportIdRequest {
    pub id: String,
}

/// 带 `deserialize_with` 的 Option 字段缺失时报错，而显式 `null` 仍解析为 `None`。
fn required_nullable<'de, D>(deserializer: D) -> Result<Option<DateTime<Utc>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<DateTime<Utc>>::deserialize(deserializer)
}

impl UpsertPublicImportRequest {
    fn into_command(self) -> Result<UpdatePublicImportConfig, WireValidationError> {
        if self.name.trim().is_empty() || self.name.chars().count() > 60 {
            return Err(WireValidationError::new("name"));
        }
        if self.group_ids.len() > MAX_GROUPS
            || self.group_ids.iter().collect::<BTreeSet<_>>().len() != self.group_ids.len()
        {
            return Err(WireValidationError::new("groupIds"));
        }
        Ok(UpdatePublicImportConfig {
            name: self.name,
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

async fn list<S>(
    _auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let configs = state
        .admin_services()
        .public_import()
        .list()
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(PublicImportListView {
            configs: configs.into_iter().map(PublicImportConfigView::from).collect(),
        }),
    ))
}

async fn create<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<UpsertPublicImportRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let command = request.into_command().map_err(map_wire_error)?;
    let config = state
        .admin_services()
        .public_import()
        .create(&auth.context().mutation_context(), command)
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
    let (id, command) = request.split().map_err(map_wire_error)?;
    let config = state
        .admin_services()
        .public_import()
        .update(&auth.context().mutation_context(), &id, command)
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(PublicImportConfigView::from(config)),
    ))
}

async fn delete<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<PublicImportIdRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    state
        .admin_services()
        .public_import()
        .delete(&auth.context().mutation_context(), &request.id)
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(PublicImportIdRequest { id: request.id }),
    ))
}

async fn rotate_token<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<PublicImportIdRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let config = state
        .admin_services()
        .public_import()
        .rotate_token(&auth.context().mutation_context(), &request.id)
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(PublicImportConfigView::from(config)),
    ))
}
