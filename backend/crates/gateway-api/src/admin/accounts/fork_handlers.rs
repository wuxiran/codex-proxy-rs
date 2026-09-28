//! fork 的账号管理接口：票据、凭据轮换（观澜复活 / 钉 state / 自动续期）、
//! turn-state 打票与遍历、测智台。

use crate::auth::SessionState;

use super::*;

/// fork 账号管理路由，由 `admin::fork_routes` 汇总。
pub fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/accounts/rotate", post(rotate_account::<S>))
        .route(
            "/api/admin/accounts/ticket",
            get(account_ticket::<S>).post(update_account_ticket::<S>),
        )
        .route(
            "/api/admin/accounts/ticket/restore",
            post(restore_account_from_ticket::<S>),
        )
        .route(
            "/api/admin/accounts/mint-turn-state",
            post(mint_account_turn_state::<S>),
        )
        .route(
            "/api/admin/accounts/test-bench",
            post(run_account_test_bench::<S>),
        )
        .route(
            "/api/admin/accounts/turn-state-hunt",
            get(hunt_account_turn_state::<S>),
        )
        .route(
            "/api/admin/accounts/turn-state-auto-hunt",
            get(auto_hunt_account_turn_state::<S>),
        )
}

async fn account_ticket<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let account_id = query.into_id().map_err(map_wire_error)?;
    let ticket = state
        .admin_services()
        .accounts()
        .account_ticket(&account_id)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(account_ticket_view(ticket)),
    ))
}

async fn update_account_ticket<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<UpdateAccountTicketRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let command = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let ticket = state
        .admin_services()
        .accounts()
        .update_account_ticket(command)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(account_ticket_view(ticket)),
    ))
}

/// 解密票据 → Provider 经 sidecar 登录并核对身份 → 走凭据轮换写回原账号。
async fn restore_account_from_ticket<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<AccountActionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let account_id = request.into_id().map_err(map_wire_error)?;
    let services = state.admin_services();
    let provider_material = services
        .accounts()
        .ticket_restore_material(&account_id)
        .await
        .map_err(map_service_error)?;
    let provider = ProviderKind::new("openai").map_err(|_| AdminError::internal())?;
    let result = services
        .credentials()
        .for_provider(&provider)
        .map_err(map_service_error)?
        .rotate(RotateCredential {
            mutation: CredentialMutation {
                context: auth.context().mutation_context(),
                account_id,
            },
            provider_material,
            settings: None,
        })
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountMutationData::from(result)),
    ))
}

/// fork：手工换 OAuth token、观澜复活、钉 turn-state 与自动续期都经此入口（上游已移除 /rotate）。
async fn rotate_account<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<RotateAccountRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let command = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let provider = ProviderKind::new("openai").map_err(|_| AdminError::internal())?;
    let result = state
        .admin_services()
        .credentials()
        .for_provider(&provider)
        .map_err(map_service_error)?
        .rotate(command)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountMutationData::from(result)),
    ))
}

async fn mint_account_turn_state<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<MintTurnStateRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let (account_id, models) = request.into_parts().map_err(map_wire_error)?;
    let report = state
        .admin_services()
        .accounts()
        .mint_turn_state(&account_id, models)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({
            "gateway": report.gateway,
            "attempts": report.attempts,
            "observeOnly": report.observe_only,
            "pairWritten": report.pair_written,
            "tickets": report.tickets.iter().map(|ticket| serde_json::json!({
                "model": ticket.model,
                "length": ticket.length,
                "servedModel": ticket.served_model,
                "expiresAt": chrono::DateTime::<chrono::Utc>::from(ticket.expires_at),
            })).collect::<Vec<_>>(),
        })),
    ))
}

/// 遍历会改账号绑定，却只能是 GET（EventSource）。会话 Cookie 是 SameSite=Lax，
/// 跨站顶层导航会带上它，所以只接受显式声明 SSE 的请求；导航请求带不上这个 Accept。
async fn hunt_account_turn_state<S>(
    auth: AdminAuth,
    State(state): State<S>,
    headers: axum::http::HeaderMap,
    AdminQuery(query): AdminQuery<TurnStateHuntQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AdminError>
where
    S: SessionState + Send + Sync,
{
    let accepts_event_stream = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/event-stream"));
    if !accepts_event_stream {
        return Err(map_wire_error(WireValidationError::new("accept")));
    }
    // `Accept` 任何脚本都能设，算不上来源证明。浏览器会如实标注请求来自哪里：
    // 同站不同源（兄弟子域）的页面发来的请求仍会带上 Lax Cookie，这里挡掉。
    // 没有这个头的是非浏览器客户端（脚本、x-api-key 调用），由鉴权本身把关。
    let cross_origin = headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|site| !site.eq_ignore_ascii_case("same-origin"));
    if cross_origin {
        return Err(map_wire_error(WireValidationError::new("origin")));
    }
    let command = query
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let stream = state
        .admin_services()
        .accounts()
        .turn_state_hunt(command)
        .await
        .map_err(map_service_error)?
        .map(|event| Ok(Event::default().data(turn_state_hunt_event_data(event).to_string())));
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// 自动撞 state：从轮换代理模板即时生成多国临时出口反复撞，命中即切静态。与遍历同为
/// 会改账号绑定的 GET（EventSource），故沿用同一套 SSE / 同源守卫。
async fn auto_hunt_account_turn_state<S>(
    auth: AdminAuth,
    State(state): State<S>,
    headers: axum::http::HeaderMap,
    AdminQuery(query): AdminQuery<TurnStateAutoHuntQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AdminError>
where
    S: SessionState + Send + Sync,
{
    let accepts_event_stream = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/event-stream"));
    if !accepts_event_stream {
        return Err(map_wire_error(WireValidationError::new("accept")));
    }
    let cross_origin = headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|site| !site.eq_ignore_ascii_case("same-origin"));
    if cross_origin {
        return Err(map_wire_error(WireValidationError::new("origin")));
    }
    let request = query
        .into_request(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let stream = state
        .admin_services()
        .accounts()
        .auto_turn_state_hunt(request)
        .await
        .map_err(map_service_error)?
        .map(|event| Ok(Event::default().data(turn_state_hunt_event_data(event).to_string())));
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn run_account_test_bench<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<AccountTestBenchRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AdminError>
where
    S: SessionState + Send + Sync,
{
    let (account_id, upstream_model, prompt, effort) =
        request.into_command().map_err(map_wire_error)?;
    let stream = state
        .admin_services()
        .accounts()
        .run_test_bench(account_id, upstream_model, prompt, effort)
        .await
        .map_err(map_service_error)?
        .map(|event| {
            let event = AccountConnectionTestEvent::from(event);
            let data = serde_json::to_string(&event.data).unwrap_or_else(|_| "{}".to_owned());
            Ok(Event::default().data(data))
        });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
