//! 账号购买成本与每日核算的管理端点。金额仅用于展示，不参与计费或调度。

use crate::auth::SessionState;

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, NaiveDate, Utc};
use gateway_admin::model::cost_accounting::{
    AccountCostRow, AccountPurchase, CostDay, CostReport, CostTotals, SetAccountPurchase,
};
use serde::{Deserialize, Serialize};

use super::{AdminAuth, AdminEnvelope, AdminError, AdminJson, AdminQuery, AdminResponse};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DailyQuery {
    from: NaiveDate,
    to: NaiveDate,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AccountsQuery {
    from: NaiveDate,
    to: NaiveDate,
    include_retired: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PurchaseRequest {
    account_id: String,
    /// 省略保留原价，`null` 清除价格。
    #[serde(default, deserialize_with = "double_option")]
    price: Option<Option<f64>>,
    purchased_at: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "double_option")]
    note: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RetireRequest {
    account_ids: Vec<String>,
    retired: bool,
}

fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer).map(Some)
}

/// 价格以两位小数的金额提交；换算成分时四舍五入，拒绝负数、非有限值和超出列宽的金额。
fn price_cents(value: f64) -> Result<i64, AdminError> {
    let cents = (value * 100.0).round();
    if !value.is_finite() || value < 0.0 || cents > 999_999_999_999.0 {
        return Err(AdminError::bad_request("购买价格不合法"));
    }
    Ok(cents as i64)
}

fn usd(micros: i64) -> f64 {
    micros as f64 / 1_000_000.0
}

fn amount(cents: i64) -> f64 {
    cents as f64 / 100.0
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PurchaseView {
    account_id: String,
    name: String,
    email: Option<String>,
    price: Option<f64>,
    purchased_at: String,
    retired_at: Option<String>,
    note: Option<String>,
}

impl From<AccountPurchase> for PurchaseView {
    fn from(purchase: AccountPurchase) -> Self {
        Self {
            account_id: purchase.account_ref,
            name: purchase.name,
            email: purchase.email,
            price: purchase.price_cents.map(amount),
            purchased_at: purchase.purchased_at.to_rfc3339(),
            retired_at: purchase.retired_at.map(|at| at.to_rfc3339()),
            note: purchase.note,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountCostView {
    #[serde(flatten)]
    purchase: PurchaseView,
    /// 账号已删除时为 false，其余现状字段为空。
    account_exists: bool,
    enabled: Option<bool>,
    credential_ready: Option<bool>,
    quota_exhausted: Option<bool>,
    plan_type: Option<String>,
    usage_usd: f64,
    request_count: u64,
    total_tokens: u64,
    cost_per_usd: Option<f64>,
}

impl From<AccountCostRow> for AccountCostView {
    fn from(row: AccountCostRow) -> Self {
        let live = row.live;
        Self {
            purchase: row.purchase.into(),
            account_exists: live.is_some(),
            enabled: live.as_ref().map(|live| live.enabled),
            credential_ready: live.as_ref().map(|live| live.credential_ready),
            quota_exhausted: live.as_ref().map(|live| live.quota_exhausted),
            plan_type: live.and_then(|live| live.plan_type),
            usage_usd: usd(row.usage_micros),
            request_count: row.request_count,
            total_tokens: row.total_tokens,
            cost_per_usd: row.cost_per_usd,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CostDayView {
    day: String,
    purchased_count: u32,
    spend: f64,
    usage_usd: f64,
    request_count: u64,
    total_tokens: u64,
    cost_per_usd: Option<f64>,
    cumulative_cost_per_usd: Option<f64>,
}

impl From<CostDay> for CostDayView {
    fn from(day: CostDay) -> Self {
        Self {
            day: day.day.to_string(),
            purchased_count: day.purchased_count,
            spend: amount(day.spend_cents),
            usage_usd: usd(day.usage_micros),
            request_count: day.request_count,
            total_tokens: day.total_tokens,
            cost_per_usd: day.cost_per_usd,
            cumulative_cost_per_usd: day.cumulative_cost_per_usd,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CostTotalsView {
    purchased_count: u32,
    spend: f64,
    usage_usd: f64,
    request_count: u64,
    total_tokens: u64,
    cost_per_usd: Option<f64>,
}

impl From<CostTotals> for CostTotalsView {
    fn from(totals: CostTotals) -> Self {
        Self {
            purchased_count: totals.purchased_count,
            spend: amount(totals.spend_cents),
            usage_usd: usd(totals.usage_micros),
            request_count: totals.request_count,
            total_tokens: totals.total_tokens,
            cost_per_usd: totals.cost_per_usd,
        }
    }
}

#[derive(Serialize)]
struct CostReportView {
    days: Vec<CostDayView>,
    totals: CostTotalsView,
}

impl From<CostReport> for CostReportView {
    fn from(report: CostReport) -> Self {
        Self {
            days: report.days.into_iter().map(Into::into).collect(),
            totals: report.totals.into(),
        }
    }
}

pub fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/cost-accounting/daily", get(daily::<S>))
        .route("/api/admin/cost-accounting/accounts", get(accounts::<S>))
        .route("/api/admin/cost-accounting/purchase", post(purchase::<S>))
        .route("/api/admin/cost-accounting/retire", post(retire::<S>))
}

fn map_error(error: gateway_admin::model::AdminError) -> AdminError {
    super::wire::map_admin_service_error(error)
}

async fn daily<S>(
    _: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<DailyQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let report = state
        .admin_services()
        .cost_accounting()
        .daily(query.from, query.to)
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(CostReportView::from(report)),
    ))
}

async fn accounts<S>(
    _: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountsQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let rows = state
        .admin_services()
        .cost_accounting()
        .accounts(query.from, query.to, query.include_retired.unwrap_or(false))
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({
            "items": rows.into_iter().map(AccountCostView::from).collect::<Vec<_>>(),
        })),
    ))
}

async fn purchase<S>(
    _: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<PurchaseRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let price_cents = match request.price {
        None => None,
        Some(None) => Some(None),
        Some(Some(value)) => Some(Some(price_cents(value)?)),
    };
    let saved = state
        .admin_services()
        .cost_accounting()
        .set_purchase(SetAccountPurchase {
            account_id: request.account_id,
            price_cents,
            purchased_at: request.purchased_at,
            note: request.note,
        })
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(PurchaseView::from(saved)),
    ))
}

async fn retire<S>(
    _: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<RetireRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let changed = state
        .admin_services()
        .cost_accounting()
        .set_retired(request.account_ids, request.retired)
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({ "accountIds": changed })),
    ))
}
