//! 账号管理路由与 HTTP handler 编排。

use crate::auth::SessionState;

use super::*;

/// 构造统一账号管理路由。
pub fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .merge(super::import_tasks::router::<S>())
        .route("/api/admin/accounts", get(list_accounts::<S>))
        .route("/api/admin/accounts/detail", get(account_detail::<S>))
        .route("/api/admin/accounts/export", get(export_accounts::<S>))
        .route("/api/admin/accounts/import", post(import_accounts::<S>))
        .route("/api/admin/accounts/refresh", post(refresh_account::<S>))
        .route("/api/admin/accounts/recover", post(recover_account::<S>))
        .route("/api/admin/accounts/rotate", post(rotate_account::<S>))
        .route(
            "/api/admin/accounts/ticket",
            get(account_ticket::<S>).post(update_account_ticket::<S>),
        )
        .route(
            "/api/admin/accounts/ticket/restore",
            post(restore_account_from_ticket::<S>),
        )
        .route("/api/admin/accounts/update", post(update_account::<S>))
        .route("/api/admin/accounts/delete", post(delete_accounts::<S>))
        .route(
            "/api/admin/accounts/batch-update",
            post(batch_update_accounts::<S>),
        )
        .route("/api/admin/accounts/quota", get(account_quota::<S>))
        .route(
            "/api/admin/accounts/personal-info",
            get(account_personal_info::<S>),
        )
        .route(
            "/api/admin/accounts/quota-forecast",
            get(account_quota_forecast::<S>),
        )
        .route(
            "/api/admin/accounts/profile-avatar",
            get(account_profile_avatar::<S>),
        )
        .route(
            "/api/admin/accounts/reset-credits",
            get(account_reset_credits::<S>).post(consume_account_reset_credit::<S>),
        )
        .route(
            "/api/admin/accounts/quota/refresh",
            post(refresh_account_quota::<S>),
        )
        .route("/api/admin/accounts/models", get(account_models::<S>))
        .route(
            "/api/admin/accounts/models/refresh",
            post(refresh_account_models::<S>),
        )
        .route(
            "/api/admin/accounts/mint-turn-state",
            post(mint_account_turn_state::<S>),
        )
        .route(
            "/api/admin/accounts/connection-test",
            get(test_account_connection::<S>),
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
        .route(
            "/api/admin/accounts/oauth/start",
            post(start_account_authorization::<S>),
        )
        .route(
            "/api/admin/accounts/oauth/complete",
            post(complete_account_authorization::<S>),
        )
}

async fn batch_update_accounts<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<BatchUpdateAccountsRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let command = request.into_command().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .batch_update(&auth.context().mutation_context(), command)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(BatchUpdatedAccountsData::from(result)),
    ))
}

async fn list_accounts<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<ListQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let command = query.validate().map_err(map_wire_error)?;
    let page = command.page;
    let page_size = command.page_size.get();
    let result = state
        .admin_services()
        .accounts()
        .list(command)
        .await
        .map_err(map_service_error)?;
    let data = account_page_data(result, page, page_size, Utc::now());
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn account_detail<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let account_id = query.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .quota(&account_id, false)
        .await
        .map_err(map_service_error)?;
    let configuration = state
        .admin_services()
        .accounts()
        .account_configuration(&account_id)
        .await
        .map_err(map_service_error)?;
    let data = AccountDetailData {
        account: account_view(result, Utc::now()),
        credential_configuration: configuration.map(provider_document_value),
    };
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn export_accounts<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountExportQuery>,
) -> Result<Response, AdminError>
where
    S: SessionState + Send + Sync,
{
    let ids = query.into_ids().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .export(&auth.context().mutation_context(), ids)
        .await
        .map_err(map_service_error)?;
    let data = AccountExportData::from_result(result);
    let mut response = AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn import_accounts<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<AccountImportRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let (provider, command) = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let result = match provider {
        AccountProvider::OpenAi => {
            state
                .admin_services()
                .openai()
                .import_document(command)
                .await
        }
        AccountProvider::Xai => state.admin_services().xai().import_document(command).await,
    }
    .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::CREATED,
        AdminEnvelope::ok(AccountImportData::from_result(result)),
    ))
}

async fn start_account_authorization<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<StartAccountAuthorizationRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let (provider, command) = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let result = match provider {
        AccountProvider::OpenAi => {
            state
                .admin_services()
                .openai()
                .start_authorization(command)
                .await
        }
        AccountProvider::Xai => {
            state
                .admin_services()
                .xai()
                .start_authorization(command)
                .await
        }
    }
    .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::CREATED,
        AdminEnvelope::ok(AccountAuthorizationData::from(result)),
    ))
}

async fn complete_account_authorization<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<CompleteAccountAuthorizationRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let (provider, command) = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let result = match provider {
        AccountProvider::OpenAi => {
            state
                .admin_services()
                .openai()
                .complete_authorization(command)
                .await
        }
        AccountProvider::Xai => {
            state
                .admin_services()
                .xai()
                .complete_authorization(command)
                .await
        }
    }
    .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::CREATED,
        AdminEnvelope::ok(AccountMutationData::from(result)),
    ))
}

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
    let result = state
        .admin_services()
        .openai()
        .rotate(command)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountMutationData::from(result)),
    ))
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
    let result = services
        .openai()
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

async fn update_account<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<UpdateAccountRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let command = request.into_command().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .update(&auth.context().mutation_context(), command)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(UpdatedAccountData::from(result)),
    ))
}

async fn delete_accounts<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<AccountDeletionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let (provider, command) = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let result = match provider {
        AccountProvider::OpenAi => state.admin_services().openai().delete(command).await,
        AccountProvider::Xai => state.admin_services().xai().delete(command).await,
    }
    .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountDeletionData::from(result)),
    ))
}

async fn refresh_account<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<AccountRefreshRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let account_id = request.into_command().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .refresh(&auth.context().mutation_context(), account_id)
        .await
        .map_err(map_service_error)?;
    let data = account_refresh_data(result, Utc::now());
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn recover_account<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<AccountActionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let account_id = request.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .recover(&auth.context().mutation_context(), account_id)
        .await
        .map_err(map_service_error)?;
    let data = account_refresh_data(result, Utc::now());
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn account_quota<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let account_id = query.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .quota(&account_id, false)
        .await
        .map_err(map_service_error)?;
    let data = AccountQuotaData {
        account: account_view(result, Utc::now()),
    };
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn account_quota_forecast<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let account_id = query.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .quota_forecast(&account_id)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountQuotaForecastData::from(result)),
    ))
}

async fn account_personal_info<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let account_id = query.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .personal_info(&account_id)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountPersonalInfoData::from(result)),
    ))
}

async fn account_profile_avatar<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountProfileAvatarQuery>,
) -> Result<Response, AdminError>
where
    S: SessionState + Send + Sync,
{
    let account_id = query.into_id().map_err(map_wire_error)?;
    let avatar = state
        .admin_services()
        .accounts()
        .profile_avatar(&account_id)
        .await
        .map_err(map_service_error)?;
    Ok(profile_avatar_response(avatar))
}

/// 将 Provider 头像流投影为受保护的同源 HTTP 响应。
#[must_use]
pub fn profile_avatar_response(avatar: ProviderProfileAvatar) -> Response {
    let ProviderProfileAvatar {
        content_type,
        content_length,
        etag,
        body,
    } = avatar;
    let mut response = Response::new(Body::from_stream(body));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        content_type
            .and_then(|value| HeaderValue::from_str(&value).ok())
            .unwrap_or_else(|| HeaderValue::from_static("application/octet-stream")),
    );
    if let Some(value) =
        content_length.and_then(|length| HeaderValue::from_str(&length.to_string()).ok())
    {
        headers.insert(header::CONTENT_LENGTH, value);
    }
    if let Some(value) = etag.and_then(|value| HeaderValue::from_str(&value).ok()) {
        headers.insert(header::ETAG, value);
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=3600"),
    );
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    response
}

async fn refresh_account_quota<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<AccountActionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let account_id = request.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .quota(&account_id, true)
        .await
        .map_err(map_service_error)?;
    let data = AccountQuotaData {
        account: account_view(result, Utc::now()),
    };
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn account_reset_credits<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let account_id = query.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .reset_credits(&auth.context().mutation_context(), account_id)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountResetCreditsData::from(result)),
    ))
}

async fn consume_account_reset_credit<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<AccountResetCreditConsumeRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let command = request.into_command().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .consume_reset_credit(&auth.context().mutation_context(), command)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountResetCreditResultData::from(result)),
    ))
}

async fn account_models<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let account_id = query.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .models(&account_id, false)
        .await
        .map_err(map_service_error)?;
    let data = account_models_data(result);
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
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

async fn refresh_account_models<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<AccountActionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let account_id = request.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .models(&account_id, true)
        .await
        .map_err(map_service_error)?;
    let data = account_models_data(result);
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
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

async fn test_account_connection<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountTestQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AdminError>
where
    S: SessionState + Send + Sync,
{
    let (account_id, upstream_model) = query.into_command().map_err(map_wire_error)?;
    let stream = state
        .admin_services()
        .accounts()
        .test_connection(account_id, upstream_model)
        .await
        .map_err(map_service_error)?
        .map(|event| {
            let event = AccountConnectionTestEvent::from(event);
            let data = serde_json::to_string(&event.data).unwrap_or_else(|_| "{}".to_owned());
            Ok(Event::default().data(data))
        });
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
