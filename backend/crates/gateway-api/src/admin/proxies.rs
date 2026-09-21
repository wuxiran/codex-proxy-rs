use crate::auth::SessionState;

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use gateway_admin::model::{
    PageSize, Revision,
    proxies::{
        MAX_PROXY_BATCH_ITEMS, NewProxy, ProxyAccountListQuery, ProxyBatchDeleteItem,
        ProxyBatchSkip, ProxyExitGeo, ProxyListQuery, ProxyMutation, ProxyQualityReport,
        ProxyQualitySnapshot, ProxyRecord, ProxyTestResult, UpdateProxy,
    },
};
use gateway_core::account::OutboundProxy;
use serde::{Deserialize, Serialize};

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminJson, AdminQuery, AdminResponse, PageMeta,
    accounts::{AccountGroupRefView, AccountProxyUpdate},
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListQuery {
    page: Option<u32>,
    page_size: Option<u16>,
    search: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AccountsQuery {
    proxy_id: String,
    page: Option<u32>,
    page_size: Option<u16>,
    search: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoveAccountRequest {
    proxy_id: String,
    account_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateRequest {
    location: Option<gateway_core::account::RequestLocation>,
    name: String,
    proxy_url: AccountProxyUpdate,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateRequest {
    #[serde(default, deserialize_with = "deserialize_location_update")]
    location: Option<Option<gateway_core::account::RequestLocation>>,
    id: String,
    revision: u64,
    name: String,
    proxy_url: Option<AccountProxyUpdate>,
}

fn deserialize_location_update<'de, D>(
    deserializer: D,
) -> Result<Option<Option<gateway_core::account::RequestLocation>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdRequest {
    id: String,
    revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProbeRequest {
    proxy_url: AccountProxyUpdate,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QualityReportQuery {
    id: String,
}

// 批量条目逐条解析：一条地址不合法只跳过该条，不让整批 422。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BatchCreateItem {
    name: Option<String>,
    proxy_url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BatchCreateRequest {
    items: Vec<BatchCreateItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BatchDeleteRequest {
    items: Vec<IdRequest>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExitGeoView {
    country: String,
    country_code: String,
    region: Option<String>,
    city: Option<String>,
}

impl From<ProxyExitGeo> for ExitGeoView {
    fn from(geo: ProxyExitGeo) -> Self {
        Self {
            country: geo.country,
            country_code: geo.country_code,
            region: geo.region,
            city: geo.city,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyTestView {
    success: bool,
    latency_ms: u64,
    exit_ip: Option<String>,
    exit_geo: Option<ExitGeoView>,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QualitySnapshotView {
    score: u8,
    grade: String,
    status: &'static str,
    summary: String,
    checked_at: String,
}

impl From<ProxyQualitySnapshot> for QualitySnapshotView {
    fn from(snapshot: ProxyQualitySnapshot) -> Self {
        Self {
            score: snapshot.score,
            grade: snapshot.grade.to_string(),
            status: snapshot.status.as_str(),
            summary: snapshot.summary,
            checked_at: snapshot.checked_at.to_rfc3339(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QualityItemView {
    target: String,
    status: &'static str,
    http_status: Option<u16>,
    latency_ms: Option<u64>,
    message: String,
    cf_ray: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QualityReportView {
    #[serde(flatten)]
    snapshot: QualitySnapshotView,
    exit_ip: Option<String>,
    exit_geo: Option<ExitGeoView>,
    base_latency_ms: Option<u64>,
    passed_count: u32,
    warn_count: u32,
    failed_count: u32,
    challenge_count: u32,
    items: Vec<QualityItemView>,
}

impl From<ProxyQualityReport> for QualityReportView {
    fn from(report: ProxyQualityReport) -> Self {
        Self {
            snapshot: report.snapshot.into(),
            exit_ip: report.exit_ip.map(|ip| ip.to_string()),
            exit_geo: report.exit_geo.map(Into::into),
            base_latency_ms: report.base_latency_ms,
            passed_count: report.passed_count,
            warn_count: report.warn_count,
            failed_count: report.failed_count,
            challenge_count: report.challenge_count,
            items: report
                .items
                .into_iter()
                .map(|item| QualityItemView {
                    target: item.target,
                    status: item.status.as_str(),
                    http_status: item.http_status,
                    latency_ms: item.latency_ms,
                    message: item.message,
                    cf_ray: item.cf_ray,
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchSkipView {
    reference: String,
    reason: String,
}

impl From<ProxyBatchSkip> for BatchSkipView {
    fn from(skip: ProxyBatchSkip) -> Self {
        Self {
            reference: skip.reference,
            reason: skip.reason,
        }
    }
}

impl From<ProxyTestResult> for ProxyTestView {
    fn from(result: ProxyTestResult) -> Self {
        Self {
            success: result.success,
            latency_ms: result.latency_ms,
            exit_ip: result.exit_ip.map(|ip| ip.to_string()),
            exit_geo: result.exit_geo.map(Into::into),
            message: result.message,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyAccountView {
    id: String,
    name: String,
    email: Option<String>,
    provider: String,
    authentication_kind: String,
    plan_type: Option<String>,
    plan_type_display: Option<String>,
    groups: Vec<AccountGroupRefView>,
    enabled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyView {
    location: Option<gateway_core::account::RequestLocation>,
    id: String,
    name: String,
    endpoint: String,
    has_authentication: bool,
    revision: u64,
    account_count: u64,
    last_test_at: Option<String>,
    last_test: Option<ProxyTestView>,
    quality: Option<QualitySnapshotView>,
    created_at: String,
    updated_at: String,
}

impl From<ProxyRecord> for ProxyView {
    fn from(record: ProxyRecord) -> Self {
        let endpoint = record.proxy.endpoint();
        Self {
            location: record.location,
            id: record.id,
            name: record.name,
            has_authentication: record.proxy.expose_url() != endpoint,
            endpoint,
            revision: record.revision.get(),
            account_count: record.account_count,
            last_test_at: record.last_test_at.map(|at| at.to_rfc3339()),
            last_test: record.last_test.map(Into::into),
            quality: record.quality.map(Into::into),
            created_at: record.created_at.to_rfc3339(),
            updated_at: record.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Serialize)]
struct ProxyPageView {
    items: Vec<ProxyView>,
    page: PageMeta,
}

#[derive(Serialize)]
struct ProxyAccountPageView {
    items: Vec<ProxyAccountView>,
    page: PageMeta,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MutationView {
    record: ProxyView,
    config_revision: u64,
}

impl From<ProxyMutation> for MutationView {
    fn from(value: ProxyMutation) -> Self {
        Self {
            record: value.record.into(),
            config_revision: value.config_revision.get(),
        }
    }
}

pub fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/proxies", get(list::<S>))
        .route("/api/admin/proxies/accounts", get(list_accounts::<S>))
        .route(
            "/api/admin/proxies/accounts/remove",
            post(remove_account::<S>),
        )
        .route("/api/admin/proxies/create", post(create::<S>))
        .route("/api/admin/proxies/update", post(update::<S>))
        .route("/api/admin/proxies/delete", post(delete::<S>))
        .route("/api/admin/proxies/test", post(test::<S>))
        .route("/api/admin/proxies/probe", post(probe::<S>))
        .route("/api/admin/proxies/quality-check", post(quality_check::<S>))
        .route(
            "/api/admin/proxies/quality-report",
            get(quality_report::<S>),
        )
        .route("/api/admin/proxies/batch-create", post(batch_create::<S>))
        .route("/api/admin/proxies/batch-delete", post(batch_delete::<S>))
}

fn revision(value: u64) -> Result<Revision, AdminError> {
    if value > i64::MAX as u64 {
        return Err(AdminError::bad_request("代理版本不合法"));
    }
    Revision::new(value).map_err(|_| AdminError::bad_request("代理版本不合法"))
}

fn map_error(error: gateway_admin::model::AdminError) -> AdminError {
    super::wire::map_admin_service_error(error)
}

async fn list<S>(
    _: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<ListQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .proxies()
        .list(ProxyListQuery {
            page: query.page.unwrap_or(1),
            page_size: PageSize::new(query.page_size.unwrap_or(20))
                .map_err(|_| AdminError::bad_request("分页大小不合法"))?,
            search: query.search.unwrap_or_default().trim().to_owned(),
        })
        .await
        .map_err(map_error)?;
    let total_pages = result.total.div_ceil(u64::from(result.page_size));
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ProxyPageView {
            items: result.items.into_iter().map(Into::into).collect(),
            page: PageMeta::new(
                result.page,
                u32::from(result.page_size),
                result.total,
                u32::try_from(total_pages).unwrap_or(u32::MAX),
            ),
        }),
    ))
}

async fn list_accounts<S>(
    _: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountsQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .proxies()
        .list_accounts(ProxyAccountListQuery {
            proxy_id: query.proxy_id,
            page: query.page.unwrap_or(1),
            page_size: PageSize::new(query.page_size.unwrap_or(20))
                .map_err(|_| AdminError::bad_request("分页大小不合法"))?,
            search: query.search.unwrap_or_default().trim().to_owned(),
        })
        .await
        .map_err(map_error)?;
    let total_pages = result.total.div_ceil(u64::from(result.page_size));
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ProxyAccountPageView {
            items: result
                .items
                .into_iter()
                .map(|account| ProxyAccountView {
                    id: account.id,
                    name: account.name,
                    email: account.email,
                    provider: account.provider_kind,
                    authentication_kind: account.authentication_kind,
                    plan_type: account.plan_type,
                    plan_type_display: account.plan_type_display,
                    groups: account
                        .groups
                        .into_iter()
                        .map(|group| AccountGroupRefView {
                            id: group.id.to_string(),
                            name: group.name,
                            color: group.color.as_str().to_owned(),
                            enabled: group.enabled,
                        })
                        .collect(),
                    enabled: account.enabled,
                })
                .collect(),
            page: PageMeta::new(
                result.page,
                u32::from(result.page_size),
                result.total,
                u32::try_from(total_pages).unwrap_or(u32::MAX),
            ),
        }),
    ))
}

async fn create<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<CreateRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let proxy = request
        .proxy_url
        .0
        .ok_or_else(|| AdminError::bad_request("代理 URL 不能为空"))?;
    let result = state
        .admin_services()
        .proxies()
        .create(
            NewProxy {
                location: request.location,
                name: request.name,
                proxy,
            },
            &auth.context().mutation_context(),
        )
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::CREATED,
        AdminEnvelope::ok(MutationView::from(result)),
    ))
}

async fn remove_account<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<RemoveAccountRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let revision = state
        .admin_services()
        .proxies()
        .remove_account(
            &request.proxy_id,
            &request.account_id,
            &auth.context().mutation_context(),
        )
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({"configRevision": revision.get()})),
    ))
}

async fn update<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<UpdateRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let proxy = request
        .proxy_url
        .map(|value| {
            value
                .0
                .ok_or_else(|| AdminError::bad_request("代理 URL 不能为空"))
        })
        .transpose()?;
    let result = state
        .admin_services()
        .proxies()
        .update(
            UpdateProxy {
                location: request.location,
                id: request.id,
                revision: revision(request.revision)?,
                name: request.name,
                proxy,
            },
            &auth.context().mutation_context(),
        )
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(MutationView::from(result)),
    ))
}

async fn delete<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<IdRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .proxies()
        .delete(
            &request.id,
            revision(request.revision)?,
            &auth.context().mutation_context(),
        )
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({"configRevision": result.get()})),
    ))
}

async fn probe<S>(
    _: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<ProbeRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let proxy = request
        .proxy_url
        .0
        .ok_or_else(|| AdminError::bad_request("代理 URL 不能为空"))?;
    let result = state
        .admin_services()
        .proxies()
        .probe(&proxy)
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ProxyTestView::from(result)),
    ))
}

async fn test<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<IdRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .proxies()
        .test(
            &request.id,
            revision(request.revision)?,
            &auth.context().mutation_context(),
        )
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ProxyView::from(result)),
    ))
}

async fn quality_check<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<IdRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .proxies()
        .quality_check(
            &request.id,
            revision(request.revision)?,
            &auth.context().mutation_context(),
        )
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({
            "record": ProxyView::from(result.record),
            "report": QualityReportView::from(result.report),
        })),
    ))
}

async fn quality_report<S>(
    _: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<QualityReportQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let report = state
        .admin_services()
        .proxies()
        .quality_report(&query.id)
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({
            "report": report.map(QualityReportView::from),
        })),
    ))
}

/// 名称缺省为脱敏端点的 `host:port`，与单条添加时用户通常的命名一致。
fn default_batch_name(proxy: &OutboundProxy) -> String {
    let endpoint = proxy.endpoint();
    endpoint
        .split_once("://")
        .map_or(endpoint.as_str(), |(_, rest)| rest)
        .trim_end_matches('/')
        .chars()
        .take(100)
        .collect()
}

async fn batch_create<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<BatchCreateRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    if request.items.is_empty() || request.items.len() > MAX_PROXY_BATCH_ITEMS {
        return Err(AdminError::bad_request("批量添加需要 1 至 200 条代理"));
    }
    let mut commands = Vec::new();
    let mut skipped = Vec::new();
    for (index, item) in request.items.into_iter().enumerate() {
        // 解析失败的原文可能带凭据，只回报行号。
        match OutboundProxy::parse(item.proxy_url.trim()) {
            Ok(proxy) => commands.push(NewProxy {
                location: None,
                name: item
                    .name
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| default_batch_name(&proxy)),
                proxy,
            }),
            Err(_) => skipped.push(BatchSkipView {
                reference: format!("第 {} 条", index + 1),
                reason: "代理地址不合法".to_owned(),
            }),
        }
    }
    let mut created = Vec::new();
    let mut config_revision = None;
    if !commands.is_empty() {
        let result = state
            .admin_services()
            .proxies()
            .create_batch(commands, &auth.context().mutation_context())
            .await
            .map_err(map_error)?;
        created = result.created.into_iter().map(ProxyView::from).collect();
        skipped.extend(result.skipped.into_iter().map(BatchSkipView::from));
        config_revision = result.config_revision.map(|revision| revision.get());
    }
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({
            "created": created,
            "skipped": skipped,
            "configRevision": config_revision,
        })),
    ))
}

async fn batch_delete<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<BatchDeleteRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let items = request
        .items
        .into_iter()
        .map(|item| {
            Ok(ProxyBatchDeleteItem {
                revision: revision(item.revision)?,
                id: item.id,
            })
        })
        .collect::<Result<Vec<_>, AdminError>>()?;
    let result = state
        .admin_services()
        .proxies()
        .delete_batch(items, &auth.context().mutation_context())
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({
            "deletedIds": result.deleted_ids,
            "skipped": result.skipped.into_iter().map(BatchSkipView::from).collect::<Vec<_>>(),
            "configRevision": result.config_revision.map(|revision| revision.get()),
        })),
    ))
}
