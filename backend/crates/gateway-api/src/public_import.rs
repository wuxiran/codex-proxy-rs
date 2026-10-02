//! 免登录账号导入 API：仅凭密链令牌访问，不读取会话，也不回显账号 ID 或凭据。

use axum::{
    Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode, header, request::Parts},
    middleware,
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use gateway_admin::model::public_import::{
    PublicImportEntry, PublicImportItem, PublicImportItemStatus, PublicImportResult,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    admin::{
        AdminEnvelope, AdminError, AdminJson, AdminResponse, auth::admin_request_id,
        wire::map_admin_service_error,
    },
    auth::SessionState,
};

const TOKEN_HEADER: &str = "x-import-token";
const MAX_TOKEN_BYTES: usize = 128;
/// 200 个账号的 sub2api 导出远小于此值；公开入口不沿用管理端的 64 MiB 上限。
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/public-import/entry", get(entry::<S>))
        .route("/api/public-import/accounts", post(import_accounts::<S>))
        .route("/api/public-import/tickets", post(import_tickets::<S>))
        .route("/api/public-import", any(not_found))
        .route("/api/public-import/{*path}", any(not_found))
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::map_response(no_store))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EntryData {
    group_names: Vec<String>,
    pin_turn_state: bool,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    max_accounts: usize,
}

impl From<PublicImportEntry> for EntryData {
    fn from(entry: PublicImportEntry) -> Self {
        Self {
            group_names: entry.group_names,
            pin_turn_state: entry.pin_turn_state,
            expires_at: entry.expires_at,
            max_accounts: entry.max_accounts,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportRequest {
    data: Value,
}

/// 票据导入：每行 `邮箱----密码----2FA密钥`；买入价、币种与预计到期时间必填。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TicketImportRequest {
    tickets: Vec<String>,
    purchase_amount: String,
    purchase_currency: String,
    expires_at: String,
}

impl std::fmt::Debug for TicketImportRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TicketImportRequest")
            .field("tickets", &self.tickets.len())
            .field("purchase_amount", &self.purchase_amount)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportItemData {
    index: usize,
    name: Option<String>,
    status: &'static str,
    imported_accounts: usize,
    proxy_name: Option<String>,
    state_pinned: bool,
    message: Option<String>,
}

impl From<PublicImportItem> for ImportItemData {
    fn from(item: PublicImportItem) -> Self {
        Self {
            index: item.index,
            name: item.name,
            status: match item.status {
                PublicImportItemStatus::Imported => "imported",
                PublicImportItemStatus::Failed => "failed",
            },
            imported_accounts: item.imported_accounts,
            proxy_name: item.proxy_name,
            state_pinned: item.state_pinned,
            message: item.message,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportData {
    total: usize,
    imported: usize,
    failed: usize,
    items: Vec<ImportItemData>,
}

impl From<PublicImportResult> for ImportData {
    fn from(result: PublicImportResult) -> Self {
        let imported = result
            .items
            .iter()
            .filter(|item| item.status == PublicImportItemStatus::Imported)
            .count();
        Self {
            total: result.items.len(),
            imported,
            failed: result.items.len() - imported,
            items: result.items.into_iter().map(Into::into).collect(),
        }
    }
}

async fn entry<S>(
    State(state): State<S>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let entry = state
        .admin_services()
        .public_import()
        .entry(&token(&headers)?)
        .await
        .map_err(map_admin_service_error)?
        .ok_or_else(invalid_link)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(EntryData::from(entry)),
    ))
}

async fn import_accounts<S>(
    State(state): State<S>,
    parts: Parts,
    AdminJson(request): AdminJson<ImportRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let token = token(&parts.headers)?;
    let request_id = admin_request_id(&parts).ok_or_else(AdminError::internal)?;
    let Value::Object(document) = request.data else {
        return Err(AdminError::bad_request("data 必须是 JSON 对象"));
    };
    let result = state
        .admin_services()
        .public_import()
        .import(&token, &request_id, document)
        .await
        .map_err(map_admin_service_error)?
        .ok_or_else(invalid_link)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ImportData::from(result)),
    ))
}

async fn import_tickets<S>(
    State(state): State<S>,
    parts: Parts,
    AdminJson(request): AdminJson<TicketImportRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let token = token(&parts.headers)?;
    let request_id = admin_request_id(&parts).ok_or_else(AdminError::internal)?;
    let expires_at = chrono::DateTime::parse_from_rfc3339(request.expires_at.trim())
        .map(|at| at.with_timezone(&chrono::Utc))
        .map_err(|_| AdminError::bad_request("预计到期时间格式不正确"))?;
    let command = gateway_admin::model::public_import::PublicTicketImport {
        tickets: request
            .tickets
            .into_iter()
            .filter(|line| !line.trim().is_empty())
            .map(secrecy::SecretString::from)
            .collect(),
        purchase_amount: request.purchase_amount,
        purchase_currency: request.purchase_currency,
        expires_at,
    };
    let result = state
        .admin_services()
        .public_import()
        .import_tickets(&token, &request_id, command)
        .await
        .map_err(map_admin_service_error)?
        .ok_or_else(invalid_link)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ImportData::from(result)),
    ))
}

fn token(headers: &HeaderMap) -> Result<String, AdminError> {
    headers
        .get(TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= MAX_TOKEN_BYTES)
        .map(ToOwned::to_owned)
        .ok_or_else(invalid_link)
}

/// 令牌缺失、错误与入口关闭返回同一个结果，不向外暴露入口是否存在。
fn invalid_link() -> AdminError {
    AdminError::not_found("导入链接无效或已关闭")
}

async fn not_found() -> AdminError {
    AdminError::not_found("导入接口不存在")
}

async fn method_not_allowed() -> AdminError {
    AdminError::method_not_allowed()
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
