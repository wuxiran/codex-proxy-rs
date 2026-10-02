//! 请求日志（统一 cookie / turn-state 票 / 满血观测）只读端点。
//!
//! 数据来自进程内有界环形缓冲 `gateway_core::request_log`；只含短标识/短指纹，
//! 不含 cookie 或票据原文。管理端「请求日志」菜单读取。

use crate::auth::SessionState;

use axum::{Router, http::StatusCode, response::IntoResponse, routing::get};

use super::{AdminAuth, AdminEnvelope, AdminError, AdminResponse};

pub fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new().route("/api/admin/logs/recent", get(recent_logs))
}

pub(crate) async fn recent_logs(_auth: AdminAuth) -> Result<impl IntoResponse, AdminError> {
    let data = gateway_core::request_log::recent(300);
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}
