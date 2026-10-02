//! 经营日报只读端点：返回后台任务落盘的每日快照（新日期在前）。

use crate::auth::SessionState;

use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use serde::Deserialize;

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminQuery, AdminResponse, wire::map_admin_service_error,
};

const DEFAULT_DAYS: usize = 60;
const MAX_DAYS: usize = 400;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DailyQuery {
    days: Option<usize>,
}

pub fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new().route("/api/admin/ops-report/daily", get(daily::<S>))
}

async fn daily<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<DailyQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let days = query.days.unwrap_or(DEFAULT_DAYS).clamp(1, MAX_DAYS);
    let report = state
        .admin_services()
        .ops_report()
        .report(days)
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(report),
    ))
}
