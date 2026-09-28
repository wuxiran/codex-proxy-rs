//! fork 管理接口的路由汇总；上游 `admin::router` 只合并这一处。

use axum::Router;

use crate::auth::SessionState;

pub(super) fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .merge(super::accounts::fork_router::<S>())
        .merge(super::ops_report::router::<S>())
        .merge(super::public_import::router::<S>())
        .merge(super::request_log::router::<S>())
        .merge(super::turn_state::router::<S>())
}
