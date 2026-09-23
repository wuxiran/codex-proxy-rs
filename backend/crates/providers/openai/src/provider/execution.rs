//! OpenAI attempt 的选择、发送与响应流执行。

use gateway_core::metering::{CalculatedCost, Usage};

use super::*;

impl CodexProvider {
    pub(super) async fn execute_image(
        &self,
        image: &ImageRequest,
        candidate: &ProviderCandidate,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        if image.payload().protocol() != PROVIDER_NAME || candidate.upstream_model().is_some() {
            return Err(provider_error(
                ProviderErrorKind::InvalidRequest,
                UpstreamSendState::NotSent,
            ));
        }
        let (endpoint_path, response_origin) = match image.kind() {
            ImageRequestKind::Generation => (
                CODEX_IMAGE_GENERATIONS_PATH,
                self.image_generations_url.clone(),
            ),
            ImageRequestKind::Edit => (CODEX_IMAGE_EDITS_PATH, self.image_edits_url.clone()),
        };
        let image_turn_id = image
            .payload()
            .context()
            .get("image_turn_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let session_affinity = derive_codex_endpoint_session_affinity(
            image.payload(),
            context.client_api_key_ref(),
            "session_id",
        );
        self.execute_raw_json_endpoint(
            context,
            RawJsonEndpointRequest {
                response_origin,
                endpoint_path,
                body: image.payload().body().clone(),
                image_turn_id,
                turn_metadata: None,
                session_affinity,
            },
        )
        .await
    }

    pub(super) async fn execute_search(
        &self,
        search: &StandaloneSearchRequest,
        candidate: &ProviderCandidate,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        if search.payload().protocol() != PROVIDER_NAME || candidate.upstream_model().is_some() {
            return Err(provider_error(
                ProviderErrorKind::InvalidRequest,
                UpstreamSendState::NotSent,
            ));
        }
        let turn_metadata = search
            .payload()
            .context()
            .get("turn_metadata")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let session_affinity = derive_codex_endpoint_session_affinity(
            search.payload(),
            context.client_api_key_ref(),
            "id",
        );
        self.execute_raw_json_endpoint(
            context,
            RawJsonEndpointRequest {
                response_origin: self.search_url.clone(),
                endpoint_path: CODEX_ALPHA_SEARCH_PATH,
                body: search.payload().body().clone(),
                image_turn_id: None,
                turn_metadata,
                session_affinity,
            },
        )
        .await
    }

    async fn execute_raw_json_endpoint(
        &self,
        context: AttemptContext,
        request: RawJsonEndpointRequest,
    ) -> Result<ProviderStream, ProviderError> {
        let selection_started_at = Instant::now();
        let lease = self
            .selector
            .select_for_provider_endpoint(&SelectCodexProviderEndpointCredential {
                request_url: &request.response_origin,
                attempt: &context,
                session_affinity: request.session_affinity.as_ref(),
            })
            .await
            .map_err(map_selection_error)?;
        let account_selection_wait_ms =
            u64::try_from(selection_started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        let lease = Arc::new(lease);
        let allows_account_state_mutation = lease.allows_account_state_mutation();
        let provider_kind = ProviderKind::new(PROVIDER_NAME)
            .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent))?;
        let metadata = ProviderCallMetadata::for_provider_endpoint(
            provider_kind,
            lease.account_id().clone(),
            UpstreamTransport::new(HTTP_JSON_TRANSPORT).map_err(|_| {
                provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
            })?,
        )
        .with_outbound_proxy(lease.account().outbound_proxy())
        .with_selection_observation(ProviderSelectionObservation::new(
            account_selection_wait_ms,
            lease.capacity_snapshot(),
        ));
        // Standalone Provider 端点没有可证明的账号 owner；Search metadata 必须按
        // 跨账号输入收敛到当前 lease，不能沿用下游声明的账号或 installation identity。
        let turn_metadata = request.turn_metadata.as_deref().and_then(|metadata| {
            crate::transport::request::scope_turn_metadata(metadata, lease.installation_id(), true)
        });
        let events = cold_json_response_stream(ColdJsonResponse {
            client: self
                .client_for_request(&context)?
                .for_account(lease.account())
                .map_err(|_| {
                    provider_error(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent)
                })?
                .with_authentication(lease.authentication()),
            response_origin: request.response_origin,
            endpoint_path: request.endpoint_path,
            body: request.body,
            image_turn_id: request.image_turn_id,
            turn_metadata,
            context,
            selector: Arc::clone(&self.selector),
            quota: Arc::clone(&self.quota),
            lease: Arc::clone(&lease),
            output_started_at: Instant::now(),
            session_affinity_key: request.session_affinity.map(CodexSessionAffinity::into_key),
        });
        let stream = ProviderStream::new(metadata, events, lease);
        Ok(if allows_account_state_mutation {
            stream.with_filtered_account_feedback(
                Arc::clone(&self.account_feedback),
                openai_failure_affects_account_score,
            )
        } else {
            stream
        })
    }
}

struct RawJsonEndpointRequest {
    response_origin: Url,
    endpoint_path: &'static str,
    body: Bytes,
    image_turn_id: Option<String>,
    turn_metadata: Option<String>,
    session_affinity: Option<CodexSessionAffinity>,
}

#[derive(Clone)]
pub(super) struct ColdResponse {
    pub(super) turn_state_pins: crate::turn_state_pin::TurnStatePins,
    pub(super) client: CodexBackendClient,
    pub(super) response_origin: Url,
    pub(super) request: CodexResponsesRequest,
    pub(super) upstream_model: UpstreamModelId,
    pub(super) transport_policy: CodexProviderTransport,
    pub(super) context: AttemptContext,
    pub(super) selector: Arc<CodexCredentialSelector>,
    pub(super) quota: Arc<CodexCredentialQuotaService>,
    pub(super) catalog: Arc<CodexCredentialCatalogService>,
    pub(super) lease: Arc<CodexCredentialLease>,
    pub(super) output_started_at: Instant,
    pub(super) session_affinity_key: Option<ProviderSessionAffinityKey>,
    pub(super) session_affinity_key_hash: Option<String>,
    pub(super) session_transport_recovery: CodexSessionTransportRecovery,
    pub(super) invalid_encrypted_content: InvalidEncryptedContentCache,
    pub(super) websocket_retry_count: u32,
    pub(super) stream_max_retries: u32,
    pub(super) session_capture: Option<OpenAiSessionCapture>,
}

pub(super) struct ColdJsonResponse {
    pub(super) client: CodexBackendClient,
    pub(super) response_origin: Url,
    pub(super) endpoint_path: &'static str,
    pub(super) body: Bytes,
    pub(super) image_turn_id: Option<String>,
    pub(super) turn_metadata: Option<String>,
    pub(super) context: AttemptContext,
    pub(super) selector: Arc<CodexCredentialSelector>,
    pub(super) quota: Arc<CodexCredentialQuotaService>,
    pub(super) lease: Arc<CodexCredentialLease>,
    pub(super) output_started_at: Instant,
    pub(super) session_affinity_key: Option<ProviderSessionAffinityKey>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct OpenAiSessionState {
    pub(super) account_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) credential_revision: Option<u64>,
    pub(super) conversation_id: Option<String>,
    #[serde(default)]
    pub(super) turn_state: Option<String>,
    #[serde(default)]
    pub(super) client_turn_id: Option<String>,
    pub(super) continuation_scope: OpenAiContinuationScope,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum OpenAiContinuationScope {
    Persisted,
    ConnectionLocal,
    ReplayRequired,
}

#[derive(Clone)]
pub(super) struct OpenAiSessionCapture {
    pub(super) account_id: String,
    pub(super) credential_revision: Option<u64>,
    pub(super) conversation_id: Option<String>,
    pub(super) turn_state: Option<String>,
    pub(super) client_turn_id: Option<String>,
    pub(super) response_store: bool,
    pub(super) continuation_scope: Option<OpenAiContinuationScope>,
}

pub(super) fn same_client_turn(previous: Option<&str>, current: Option<&str>) -> bool {
    previous
        .zip(current)
        .is_some_and(|(previous, current)| !previous.is_empty() && previous == current)
}

/// 组装请求日志的响应侧回填补丁：Set-Cookie(__cf_bm) 有无、上游票短指纹与票长、service_tier、
/// 上游实际服务模型（用于「实际模型≠请求模型」的猫腻分叉判断）。
/// 票优先取本回合捕获（含流内轮转）的 turn_state，退回 handshake 票；只存短指纹/长度/模型名，绝不存原文。
fn codex_response_log_patch(
    has_cfbm: bool,
    capture: Option<&OpenAiSessionCapture>,
    handshake_turn_state: Option<&str>,
    service_tier: Option<&str>,
    served_model: Option<&str>,
    resp_cookies: &[String],
) -> gateway_core::request_log::ResponsePatch {
    let ticket = capture
        .and_then(|c| c.turn_state.as_deref())
        .or(handshake_turn_state);
    gateway_core::request_log::ResponsePatch {
        set_cookie: Some(has_cfbm),
        ticket_out: ticket.map(gateway_core::request_log::fingerprint),
        ticket_len: ticket.map(str::len),
        service_tier: service_tier.map(str::to_owned),
        served_model: served_model.map(str::to_owned),
        resp_cookies: (!resp_cookies.is_empty()).then(|| resp_cookies.to_vec()),
    }
}

/// 把未过滤的 Set-Cookie 头列表折成安全摘要：每项 `name@domain#值指纹`，绝不含原文。
/// 只暴露 cookie 的名字与域（判断有没有隐藏的网关/节点 cookie）+ 值的短指纹（判断是否轮换）。
fn summarize_set_cookies(headers: &[String]) -> Vec<String> {
    headers
        .iter()
        .filter_map(|raw| {
            let raw = raw.trim();
            let mut parts = raw.split(';');
            let nv = parts.next()?.trim();
            let (name, value) = nv.split_once('=')?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            let domain = parts
                .filter_map(|attr| {
                    let attr = attr.trim();
                    attr.split_once('=').and_then(|(k, v)| {
                        k.trim()
                            .eq_ignore_ascii_case("domain")
                            .then(|| v.trim().to_owned())
                    })
                })
                .next()
                .unwrap_or_default();
            let fp = gateway_core::request_log::fingerprint(value.trim());
            Some(if domain.is_empty() {
                format!("{name}{fp}")
            } else {
                format!("{name}@{domain}{fp}")
            })
        })
        .collect()
}

pub(super) fn decode_openai_session_state(request: &GenerateRequest) -> Option<OpenAiSessionState> {
    request
        .provider_session_state(PROVIDER_NAME)
        .and_then(|state| serde_json::from_value(Value::Object(state.payload().clone())).ok())
}

pub(super) fn encode_openai_session_state(
    state: OpenAiSessionState,
) -> Result<ProviderSessionState, ProviderError> {
    let Value::Object(payload) = serde_json::to_value(state)
        .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent))?
    else {
        return Err(provider_error(
            ProviderErrorKind::Protocol,
            UpstreamSendState::Sent,
        ));
    };
    ProviderSessionState::new(PROVIDER_NAME, payload)
        .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent))
}

fn encode_openai_session_capture(
    capture: &OpenAiSessionCapture,
) -> Result<ProviderSessionState, ProviderError> {
    let Some(continuation_scope) = capture.continuation_scope else {
        return Err(provider_error(
            ProviderErrorKind::Protocol,
            UpstreamSendState::Sent,
        ));
    };
    encode_openai_session_state(OpenAiSessionState {
        account_id: capture.account_id.clone(),
        credential_revision: capture.credential_revision,
        conversation_id: capture.conversation_id.clone(),
        turn_state: capture.turn_state.clone(),
        client_turn_id: capture.client_turn_id.clone(),
        continuation_scope,
    })
}

pub(super) fn attach_openai_session_update(
    events: &mut [ProviderEvent],
    capture: &mut Option<OpenAiSessionCapture>,
) {
    let Some(terminal_index) = events
        .iter()
        .position(|event| terminal_response_output(event).is_some())
    else {
        return;
    };
    let Some(capture) = capture.take() else {
        return;
    };
    let Ok(update) = encode_openai_session_capture(&capture) else {
        return;
    };
    events[terminal_index].attach_session_update(update);
}

pub(super) fn terminal_response_output(event: &ProviderEvent) -> Option<&[Value]> {
    let wire = event.wire_event()?;
    if wire.protocol() != PROVIDER_NAME {
        return None;
    }
    let event_type = wire
        .event_type()
        .or_else(|| wire.data().get("type").and_then(Value::as_str));
    matches!(
        event_type,
        Some("response.completed" | "response.incomplete")
    )
    .then(|| {
        wire.data()
            .pointer("/response/output")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
    })
    .flatten()
}

pub(super) enum CodexHandshakeAttemptError {
    Client(CodexClientError),
    Cancelled,
    Timeout,
}

pub(super) async fn create_response_attempt(
    client: &CodexBackendClient,
    request: &CodexResponsesRequest,
    request_context: CodexRequestContext<'_>,
    account_id: &str,
    deadline: SystemTime,
    cancellation: &CancellationToken,
) -> Result<CodexBackendStreamingResponse, CodexHandshakeAttemptError> {
    let Some(handshake_deadline) = remaining(deadline) else {
        return Err(CodexHandshakeAttemptError::Timeout);
    };
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(CodexHandshakeAttemptError::Cancelled),
        _ = tokio::time::sleep(handshake_deadline) => Err(CodexHandshakeAttemptError::Timeout),
        response = client.create_response_stream_with_pool_account(
            request,
            request_context,
            Some(account_id),
        ) => response.map_err(CodexHandshakeAttemptError::Client),
    }
}

pub(super) fn map_handshake_attempt_error(
    error: CodexHandshakeAttemptError,
) -> MappedProviderFailure {
    match error {
        CodexHandshakeAttemptError::Client(error) => map_handshake_error(error),
        CodexHandshakeAttemptError::Cancelled => MappedProviderFailure::plain(provider_error(
            ProviderErrorKind::Cancelled,
            UpstreamSendState::Ambiguous,
        )),
        CodexHandshakeAttemptError::Timeout => MappedProviderFailure::plain(provider_error(
            ProviderErrorKind::Timeout,
            UpstreamSendState::Ambiguous,
        )),
    }
}

pub(super) async fn create_json_attempt(
    request: &ColdJsonResponse,
    account: &ProviderAccount,
    installation_id: &str,
    authorization: &SecretString,
    cookie_header: Option<&SecretString>,
    account_selection: CodexAccountSelectionTelemetry<'_>,
) -> Result<CodexBackendJsonResponse, CodexHandshakeAttemptError> {
    let Some(handshake_deadline) = remaining(request.context.deadline()) else {
        return Err(CodexHandshakeAttemptError::Timeout);
    };
    let request_id = request.context.request_id().as_str();
    let trace = request.context.trace();
    let mut request_context = CodexRequestContext::auxiliary(
        authorization.expose_secret(),
        account.upstream_account_id(),
        request_id,
        Some(installation_id),
    );
    request_context.trace = Some(&trace);
    request_context.cookie_header = cookie_header.map(ExposeSecret::expose_secret);
    request_context.turn_metadata = request.turn_metadata.as_deref();
    request_context.account_selection = account_selection;
    tokio::select! {
        biased;
        _ = request.context.cancellation().cancelled() => Err(CodexHandshakeAttemptError::Cancelled),
        _ = tokio::time::sleep(handshake_deadline) => Err(CodexHandshakeAttemptError::Timeout),
        response = request.client.post_raw_json(
            request.endpoint_path,
            request.body.clone(),
            request.image_turn_id.as_deref(),
            request_context,
        ) => response.map_err(CodexHandshakeAttemptError::Client),
    }
}

pub(super) fn cold_json_response_stream(request: ColdJsonResponse) -> EventStream {
    Box::pin(async_stream::try_stream! {
        let allows_account_state_mutation = request.lease.allows_account_state_mutation();
        let failure_context = OpenAiFailureContext {
            client: &request.client,
            selector: &request.selector,
            quota: &request.quota,
            response_origin: &request.response_origin,
            cyber_policy_scope: None,
            allows_account_state_mutation,
            allows_capacity_feedback: !request.context.is_diagnostic_required_account(),
        };
        let active_account = request.lease.account().clone();
        let cookie_header = build_cookie_header(request.lease.cookies())?;
        let authorization = request
            .lease
            .authentication()
            .authorization_header()
            .map_err(|_| {
                provider_error(
                    ProviderErrorKind::Unauthorized,
                    UpstreamSendState::NotSent,
                )
            })?;
        let account_selection = CodexAccountSelectionTelemetry::new(
            request.lease.affinity_hit(),
            request.lease.escape_reason(),
            request.lease.account_switch(),
        );
        let response = create_json_attempt(
            &request,
            &active_account,
            request.lease.installation_id(),
            &authorization,
            cookie_header.as_ref(),
            account_selection,
        )
        .await;
        if let Err(CodexHandshakeAttemptError::Client(error)) = &response {
            log_client_upstream_error(
                UpstreamErrorLogContext::new(&request.context, &active_account, None),
                error,
            );
        }
        let response = match response.map_err(map_handshake_attempt_error) {
            Ok(response) => response,
            Err(mut failure) => {
                if let Some(observation) = failure.observation.take() {
                    yield ProviderEvent::observation(observation);
                }
                apply_failure(&failure_context, &active_account, &failure).await;
                Err(failure.error)?;
                return;
            }
        };

        if allows_account_state_mutation && let Some(key) = request.session_affinity_key.as_ref() {
            // JSON 已完整接收；在首个 yield 前提交亲和迁移，避免下游取消漏掉更新。
            request.selector.update_session_affinity(
                key,
                request.lease.affinity_expected_account_id(),
                active_account.id(),
            ).await;
        }
        let mut metrics = response.transport_metrics.clone();
        metrics.first_event_ms = Some(
            i64::try_from(request.output_started_at.elapsed().as_millis()).unwrap_or(i64::MAX),
        );
        if let Some(observation) = codex_response_observation(
            CodexBackendTransport::HttpJson,
            &response.diagnostics,
            &response.response_metadata,
            &metrics,
            None,
            openai_response_timings(&metrics, &response.response_metadata),
        ) {
            yield ProviderEvent::observation(observation);
        }
        if allows_account_state_mutation {
            synchronize_passive_quota_headers(
                &request.quota,
                &active_account,
                &response.rate_limit_headers,
            )
            .await;
            if !response.set_cookie_headers.is_empty()
                && let Err(error) = request
                    .selector
                    .capture_response_cookies(
                        &active_account,
                        &request.response_origin,
                        &response.set_cookie_headers,
                    )
                    .await
            {
                tracing::warn!(
                    account_id = %active_account.id(),
                    error = %error,
                    "Failed to persist OpenAI provider endpoint response cookies"
                );
            }
        }

        let mut response_meta =
            ResponseMeta::for_provider_endpoint(request.context.request_id().as_str());
        yield ProviderEvent::canonical(GatewayEvent::Started(response_meta.clone()));
        if matches!(request.endpoint_path, CODEX_IMAGE_GENERATIONS_PATH | CODEX_IMAGE_EDITS_PATH)
            && let Some((usage, cost)) = image_response_metering(&request.body, &response.body, request.context.pricing())
        {
            yield ProviderEvent::canonical(GatewayEvent::Usage(usage));
            if let Some(cost) = cost {
                let billing_model = serde_json::from_slice::<serde_json::Value>(&request.body)
                    .ok().and_then(|body| body.get("model").and_then(serde_json::Value::as_str).map(str::to_owned));
                response_meta = response_meta.with_billing_model(billing_model);
                yield ProviderEvent::canonical(GatewayEvent::CalculatedCost(cost));
            }
        }
        let wire = ProtocolWireEvent::raw_json(PROVIDER_NAME, response.body).map_err(|_| {
            provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent)
        })?;
        yield ProviderEvent::wire(wire);
        yield ProviderEvent::canonical(GatewayEvent::Completed(
            response_meta.with_finish_reason(FinishReason::Stop),
        ));
    })
}

fn image_response_metering(
    request_body: &[u8],
    body: &[u8],
    prices: &gateway_core::metering::PricingOverrides,
) -> Option<(Usage, Option<CalculatedCost>)> {
    // 只保留 usage，跳过通常很大的 base64 图片；原始响应仍按字节透传。
    #[derive(Deserialize)]
    struct ImageUsageEnvelope {
        usage: Option<Value>,
    }

    let raw = serde_json::from_slice::<ImageUsageEnvelope>(body)
        .ok()?
        .usage?;
    let mut usage = Usage::new();
    usage.input_tokens = raw.get("input_tokens").and_then(Value::as_u64);
    usage.output_tokens = raw.get("output_tokens").and_then(Value::as_u64);
    usage.cached_tokens = raw
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64);
    usage.image_input_tokens = raw
        .pointer("/input_tokens_details/image_tokens")
        .and_then(Value::as_u64);
    usage.image_output_tokens = raw
        .pointer("/output_tokens_details/image_tokens")
        .and_then(Value::as_u64);
    // 总量是上游独立报告的事实；图片明细是总输入/输出的子集，不能再次相加。
    usage.total_tokens = raw.get("total_tokens").and_then(Value::as_u64);
    let cost = crate::transport::usage::image_calculated_cost(request_body, &raw, prices);
    (usage != Usage::default()).then_some((usage, cost))
}

pub(super) fn cold_response_stream(response: ColdResponse) -> EventStream {
    if !can_recover(&response.request) {
        return cold_response_stream_once(response);
    }
    Box::pin(async_stream::try_stream! {
        let mut response = response;
        let scope = recovery_scope(&response.request, response.lease.account(), response.context.client_api_key_ref());
        let known = response.invalid_encrypted_content.known(scope);
        let removed = strip_encrypted_reasoning(&mut response.request, Some(&known));
        if removed > 0 {
            tracing::info!(request_id = response.context.request_id().as_str(), removed_items = removed,
                recovery = "invalid_encrypted_content", recovery_action = "preclean", "OpenAI invalid reasoning history cleaned");
        }
        let mut retried = false;
        'recovery: loop {
            // 仅候选恢复请求保留一份快照；重试沿用同一个 lease/client，不刷新或换号。
            let mut events = cold_response_stream_once(response.clone());
            let mut delivered = false;
            while let Some(event) = events.next().await {
                match event {
                    Ok(event) => {
                        delivered |= event.has_client_event() || !event.canonical_facts().is_empty();
                        yield event;
                    }
                    Err(mut error) => {
                        if !delivered && !retried && is_encrypted_content_rejection(&error)
                            && (error.client_visible_upstream_response().is_some() || error.has_atomic_client_events())
                            && can_recover(&response.request) {
                            let pending = error.take_atomic_client_events();
                            if pending.iter().all(encrypted_recovery_prelude) {
                                response.invalid_encrypted_content.remember(scope, &response.request);
                                let removed = strip_encrypted_reasoning(&mut response.request, None);
                                if removed > 0 {
                                    retried = true;
                                    tracing::info!(request_id = response.context.request_id().as_str(), removed_items = removed,
                                        recovery = "invalid_encrypted_content", recovery_attempt = 1,
                                        "OpenAI rejected reasoning history recovered on the same credential");
                                    continue 'recovery;
                                }
                            }
                            error = error.with_atomic_client_events(pending);
                        }
                        Err(error)?;
                    }
                }
            }
            break;
        }
    })
}

fn encrypted_recovery_prelude(event: &ProviderEvent) -> bool {
    // 即使工具结构帧尚未交付，也不能在未知服务端副作用后重试。只放行空前导帧。
    event
        .canonical_facts()
        .iter()
        .all(|fact| matches!(fact, GatewayEvent::Started(_)))
        && event.wire_event().is_none_or(|wire| {
            let data = wire.data();
            matches!(
                wire.event_type()
                    .or_else(|| data.get("type").and_then(Value::as_str)),
                Some("response.created" | "response.in_progress" | "response.failed" | "error")
            ) && data
                .pointer("/response/output")
                .is_none_or(|output| output.as_array().is_some_and(Vec::is_empty))
                && data.pointer("/response/usage").is_none_or(Value::is_null)
        })
}

fn cold_response_stream_once(response: ColdResponse) -> EventStream {
    let ColdResponse {
        turn_state_pins,
        client,
        response_origin,
        mut request,
        upstream_model,
        transport_policy,
        context,
        selector,
        quota,
        catalog,
        lease,
        output_started_at,
        session_affinity_key,
        session_affinity_key_hash,
        session_transport_recovery,
        invalid_encrypted_content: _,
        websocket_retry_count,
        stream_max_retries,
        mut session_capture,
    } = response;
    Box::pin(async_stream::try_stream! {
        let cyber_policy_scope = lease.cyber_policy_scope().cloned();
        // 经临时出口的探测，其成败说明的是那个出口而不是账号：不回写冷却、反馈分、Cookie 或配额。
        let allows_account_state_mutation =
            lease.allows_account_state_mutation() && context.diagnostic_egress().is_none();
        let failure_context = OpenAiFailureContext {
            client: &client,
            selector: &selector,
            quota: &quota,
            response_origin: &response_origin,
            cyber_policy_scope: cyber_policy_scope.as_ref(),
            allows_account_state_mutation,
            allows_capacity_feedback: !context.is_diagnostic_required_account(),
        };
        let mut active_account = lease.account().clone();
        let cookie_header = build_cookie_header(lease.cookies())?;
        let authorization = lease
            .authentication()
            .authorization_header()
            .map_err(|_| {
                provider_error(
                    ProviderErrorKind::Unauthorized,
                    UpstreamSendState::NotSent,
                )
            })?;
        let mut pin_attempt = if request.generate() && !context.is_diagnostic_required_account()
            && allows_account_state_mutation
            && let Some(generation) = lease.turn_state_pin()
            && let Some(oauth) = lease.authentication().oauth()
            && let Some(expected_length) = crate::turn_state_pin::CaptureRule::for_plan(active_account.plan_type())
                .expected_length(upstream_model.as_str())
        {
            let binding = crate::turn_state_pin::credential_binding(generation, oauth.access_token.expose_secret());
            let egress = crate::turn_state_pin::egress_fingerprint(
                active_account.outbound_proxy().map(|proxy| proxy.expose_url()),
            );
            Some(turn_state_pins.attempt(
                active_account.id().as_str(), binding, upstream_model.as_str(),
                context.client_api_key_ref().as_str(), expected_length, &egress, SystemTime::now(),
            ))
        } else { None };
        if let Some(value) = pin_attempt.as_ref().and_then(crate::turn_state_pin::PinAttempt::value) {
            request.turn_state = Some(value.to_owned());
            request.passthrough_headers.remove("x-codex-turn-state");
        }
        let request_id = context.request_id().as_str().to_owned();
        let cancellation = context.cancellation().clone();
        let account_selection = CodexAccountSelectionTelemetry::new(
            lease.affinity_hit(),
            lease.escape_reason(),
            lease.account_switch(),
        );
        let request_transport_requirement = transport_requirement(&request);
        let trace = context.trace();
        let response = create_response_attempt(
            &client,
            &request,
            codex_request_context(
                &request,
                &request_id,
                &active_account,
                lease.installation_id(),
                &authorization,
                cookie_header.as_ref(),
                account_selection,
            ).with_trace(&trace),
            active_account.id().as_str(),
            context.deadline(),
            &cancellation,
        )
        .await;
        let websocket_failure_policy = match &response {
            Err(CodexHandshakeAttemptError::Client(error))
                if transport_policy == CodexProviderTransport::PreferWebSocket =>
            {
                websocket_client_failure_policy(error)
            }
            _ => None,
        };
        if let Err(CodexHandshakeAttemptError::Client(error)) = &response {
            log_client_upstream_error(
                UpstreamErrorLogContext::new(&context, &active_account, None),
                error,
            );
        }
        let response = response.map_err(map_handshake_attempt_error);
        let response = match response {
            Ok(response) => response,
            Err(mut failure) => {
                if let Some(policy) = websocket_failure_policy {
                    apply_websocket_recovery_policy(
                        &mut failure,
                        WebSocketRecoveryContext {
                            policy,
                            requirement: request_transport_requirement,
                            retry_count: websocket_retry_count,
                            max_retries: stream_max_retries,
                            request_id: context.request_id().as_str(),
                            attempt_index: context.attempt_index().get(),
                            account_id: active_account.id().as_str(),
                            session_affinity_key: session_affinity_key.as_ref(),
                            session_affinity_key_hash: session_affinity_key_hash.as_deref(),
                            session_transport_recovery: &session_transport_recovery,
                        },
                    );
                }
                if let Some(observation) = failure.observation.take() {
                    yield ProviderEvent::observation(observation);
                }
                apply_failure(&failure_context, &active_account, &failure)
                .await;
                Err(quota_continuation_replay_error(
                    failure.error,
                    &request,
                    ReplayBoundary::BeforeSemanticOutput,
                ))?;
                return;
            }
        };
        if !accepts_backend_transport(transport_policy, response.transport) {
            let failure = MappedProviderFailure::plain(provider_error(
                ProviderErrorKind::Protocol,
                UpstreamSendState::Sent,
            ));
            apply_failure(&failure_context, &active_account, &failure)
            .await;
            Err(failure.error)?;
            return;
        }
        if let Some(pin) = pin_attempt.as_mut() {
            pin.observe(response.turn_state.as_deref());
        }
        if let Some(capture) = session_capture.as_mut() {
            capture.continuation_scope = Some(if capture.response_store {
                OpenAiContinuationScope::Persisted
            } else if response.transport == CodexBackendTransport::WebSocket
                && response.connection_local_continuation
            {
                OpenAiContinuationScope::ConnectionLocal
            } else {
                OpenAiContinuationScope::ReplayRequired
            });
            capture.turn_state = response.turn_state.clone().or(capture.turn_state.clone());
        }
        // 响应侧观测信号：在 response.body 被移动前，读一次 Set-Cookie(__cf_bm) 与 handshake 票。
        // 真实 service_tier 要等解码后才有，故在成功出口处再一并回填（见 emit 处）。
        let resp_has_cfbm = response
            .set_cookie_headers
            .iter()
            .any(|h| h.trim_start().to_ascii_lowercase().starts_with("__cf_bm="));
        // 未过滤的全部 Set-Cookie 摘要（name@domain#值指纹，非原文），用于实测上游
        // 到底下发了哪些 cookie（含 cpr 平时按白名单丢掉的），排查节点信息是否藏在 cookie 里。
        let resp_cookie_summary = summarize_set_cookies(&response.set_cookie_headers);
        let resp_handshake_turn_state = response.turn_state.clone();
        let mut observation_state = OpenAiResponseObservationState::from_backend_response(
            &response,
            &request,
        );
        if let Some(observation) = observation_state.observation(None) {
            yield ProviderEvent::observation(observation);
        }
        if let Some(etag) = response.response_metadata.models_etag.as_deref()
            && let Err(error) = catalog.observe_response_etag(etag)
        {
            tracing::warn!(
                error = %error,
                "OpenAI model ETag observation was rejected"
            );
        }
        if allows_account_state_mutation
            && !response.set_cookie_headers.is_empty()
            && let Ok(outcome) = selector
                .capture_response_cookies(
                    &active_account,
                    &response_origin,
                    &response.set_cookie_headers,
                )
                .await
                && let Some(revision) = outcome.credential_revision
                && let Ok(current) = selector.current_account(active_account.id()).await
                && current.revision().get() == revision
        {
            active_account = current;
        }
        let response_transport = response.transport;
        let websocket_connection_id = response.websocket_connection_id;
        let mut body = response.body;
        let mut failure_diagnostics = response.diagnostics.clone();
        if response_transport == CodexBackendTransport::WebSocket {
            // opening ID 标识连接，不可作为缺失请求级错误头时的当前请求 ID。
            failure_diagnostics.request_id = None;
        }
        let failure_set_cookie_headers = response.set_cookie_headers.clone();
        let failure_rate_limit_headers = response.rate_limit_headers.clone();
        let mut passive_quota_observation =
            OpenAiPassiveQuotaObservation::new(response.rate_limit_headers);
        let rate_limit_updates = response.rate_limit_updates;
        let response_metadata_updates = response.response_metadata_updates;
        // OpenAI 线路为透明代理：HTTP SSE 与 WebSocket 两条上游均启用 raw 透传，
        // 下游按字节转发上游原文，避免 serde 往返改写数值/精度（大整数→f64、logprobs 等）。
        // WS 帧由 reducer 以 encode_sse_event(&event, raw) 逐字节内嵌上游原始 JSON
        // （transport/protocol/websocket.rs），push_frames 抽出的 data 即上游原文。
        let mut decoder = CodexCanonicalDecoder::new(upstream_model.as_str())
            .with_pricing(context.pricing().get("openai").and_then(|models| models.get(upstream_model.as_str())).cloned())
            .with_reported_model(response.response_metadata.effective_model.as_deref())
            .with_requested_service_tier(request.service_tier())
            .with_request_tool_pricing(upstream_model.as_str(), request.tools())
            .with_raw_sse_passthrough();
        let mut pre_commit_events = PreCommitClientEvents::new();
        loop {
            let Some(stream_deadline) = remaining(context.deadline()) else {
                if allows_account_state_mutation {
                    synchronize_passive_quota(
                        &quota,
                        &active_account,
                        passive_quota_observation.rate_limits(),
                    )
                    .await;
                }
                Err(provider_error(ProviderErrorKind::Timeout, UpstreamSendState::Sent))?;
                return;
            };
            let replay_grace_deadline = pre_commit_events.replay_grace_deadline();
            let next = tokio::select! {
                biased;
                _ = cancellation.cancelled() => Err(MappedProviderFailure::plain(provider_error(
                    ProviderErrorKind::Cancelled,
                    UpstreamSendState::Sent,
                ))),
                _ = tokio::time::sleep(stream_deadline) => Err(MappedProviderFailure::plain(provider_error(
                    ProviderErrorKind::Timeout,
                    UpstreamSendState::Sent,
                ))),
                _ = wait_for_replay_grace(replay_grace_deadline) => Ok(PreCommitPoll::GraceElapsed),
                chunk = body.next() => match chunk {
                    Some(Ok(chunk)) => Ok(PreCommitPoll::Upstream(Some(chunk))),
                    Some(Err(error)) => {
                        log_client_upstream_error(
                            UpstreamErrorLogContext::new(
                                &context,
                                &active_account,
                                websocket_connection_id,
                            ),
                            &error,
                        );
                        Err(map_stream_error(error))
                    }
                    None => Ok(PreCommitPoll::Upstream(None)),
                },
            };
            let next = match next {
                Ok(PreCommitPoll::Upstream(next)) => next,
                Ok(PreCommitPoll::GraceElapsed) => {
                    for event in pre_commit_events.commit_pending() {
                        yield event;
                    }
                    continue;
                }
                Err(mut failure) => {
                    let updates = take_rate_limit_updates(rate_limit_updates.as_ref()).await;
                    let rate_limits_changed = if updates.is_empty() {
                        false
                    } else {
                        passive_quota_observation.observe(&updates);
                        let update_headers = rate_limit_update_headers(&updates);
                        observation_state.merge_rate_limit_headers(&update_headers)
                    };
                    let metadata_merge = merge_response_metadata_updates(
                        response_metadata_updates.as_ref(),
                        &mut session_capture,
                        &mut observation_state,
                        pin_attempt.as_mut(),
                        &mut decoder,
                    )
                    .await;
                    let observation_event = if rate_limits_changed || metadata_merge.is_some() {
                        observation_state.observation(None).map(ProviderEvent::observation)
                    } else {
                        None
                    };
                    if failure.websocket_transport_retryable
                        && response_transport == CodexBackendTransport::WebSocket
                        && (matches!(
                            failure.error.send_state(),
                            UpstreamSendState::Sent | UpstreamSendState::Ambiguous
                        ) || !pre_commit_events.is_committed())
                    {
                        apply_websocket_recovery_policy(
                            &mut failure,
                            WebSocketRecoveryContext {
                                policy: WebSocketFailurePolicy::Budgeted,
                                requirement: request_transport_requirement,
                                retry_count: websocket_retry_count,
                                max_retries: stream_max_retries,
                                request_id: context.request_id().as_str(),
                                attempt_index: context.attempt_index().get(),
                                account_id: active_account.id().as_str(),
                                session_affinity_key: session_affinity_key.as_ref(),
                                session_affinity_key_hash: session_affinity_key_hash.as_deref(),
                                session_transport_recovery: &session_transport_recovery,
                            },
                        );
                    }
                    if let Some(event) = observation_event {
                        yield event;
                    }
                    if allows_account_state_mutation {
                        synchronize_passive_quota(
                            &quota,
                            &active_account,
                            passive_quota_observation.rate_limits(),
                        )
                        .await;
                    }
                    apply_failure(&failure_context, &active_account, &failure)
                    .await;
                    Err(quota_continuation_replay_error(
                        failure.error,
                        &request,
                        ReplayBoundary::from_semantic_output(pre_commit_events.is_committed()),
                    ))?;
                    return;
                }
            };
            let Some(chunk) = next else { break; };
            let updates = take_rate_limit_updates(rate_limit_updates.as_ref()).await;
            let rate_limits_changed = if updates.is_empty() {
                false
            } else {
                passive_quota_observation.observe(&updates);
                observation_state.merge_rate_limit_headers(&rate_limit_update_headers(&updates))
            };
            let first_event_changed =
                observation_state.observe_stream_chunk(&chunk, output_started_at);
            let chunk_len = chunk.len();
            let (mut events, canonical_failure) = match decoder.push(&chunk) {
                CodexCanonicalOutcome::Events(events) => (events, None),
                CodexCanonicalOutcome::Failed(failure) => {
                    let (events, error, semantic_output_seen) = failure.into_parts();
                    (events, Some((error, semantic_output_seen)))
                }
            };
            let metadata_merge = merge_response_metadata_updates(
                response_metadata_updates.as_ref(),
                &mut session_capture,
                &mut observation_state,
                pin_attempt.as_mut(),
                &mut decoder,
            )
            .await;
            let metadata_changed = metadata_merge.unwrap_or(false);
            pre_commit_events.observe_chunk(chunk_len);
            let response_model_changed = observation_state
                .observe_upstream_response_model(decoder.response_model());
            let service_tier_changed = observation_state
                .observe_upstream_service_tier(decoder.response_service_tier());
            let terminal_failure = canonical_failure.map(|(error, semantic_output_seen)| {
                log_canonical_upstream_error(
                    UpstreamErrorLogContext::new(
                        &context,
                        &active_account,
                        websocket_connection_id,
                    ),
                    response_transport,
                    &error,
                );
                let atomic_upstream_failure = matches!(&error, CodexCanonicalError::Upstream(_));
                (
                    map_canonical_error(
                        error,
                        &failure_diagnostics,
                        &failure_set_cookie_headers,
                        &failure_rate_limit_headers,
                        ReplayBoundary::from_semantic_output(
                            semantic_output_seen || pre_commit_events.is_committed(),
                        ),
                    ),
                    atomic_upstream_failure,
                )
            });
            let timing_signals = decoder.take_timing_signals();
            let timing_changed = first_event_changed
                || observation_state
                    .observe_timing_signals(timing_signals, output_started_at);
            let completed = events
                .iter()
                .flat_map(ProviderEvent::canonical_facts)
                .any(|event| matches!(event, GatewayEvent::Completed(_)));
            let terminal_changed = completed
                && observation_state.mark_completed(terminal_response_is_incomplete(&events));
            if response_transport == CodexBackendTransport::WebSocket
                && completed && terminal_failure.is_none()
                && let Some(key) = session_affinity_key.as_ref()
            {
                session_transport_recovery.websocket_succeeded(key);
            }
            if allows_account_state_mutation && (completed || terminal_failure.is_some()) {
                synchronize_passive_quota(
                    &quota,
                    &active_account,
                    passive_quota_observation.rate_limits(),
                )
                .await;
            }
            if let Some((failure, _)) = terminal_failure.as_ref() {
                apply_failure(&failure_context, &active_account, failure)
                .await;
            }
            if completed && terminal_failure.is_none() && !terminal_response_is_incomplete(&events)
                && let Some(pin) = pin_attempt.as_mut()
            { pin.completed(SystemTime::now()); }
            attach_openai_session_update(&mut events, &mut session_capture);
            if allows_account_state_mutation && completed && terminal_failure.is_none() {
                // 完成事件一旦交给下游，Core 可以立刻停止轮询 Provider stream；
                // 在此之前持久化亲和关系，保证成功请求不会因流被提前 drop 而丢失绑定。
                selector
                    .record_success(
                        &active_account,
                        session_affinity_key.as_ref(),
                        lease.affinity_expected_account_id(),
                    )
                    .await;
                selector
                    .observe_cyber_policy_success(cyber_policy_scope.as_ref())
                    .await;
            }
            if (rate_limits_changed
                || response_model_changed
                || service_tier_changed
                || timing_changed
                || metadata_changed
                || terminal_changed
                || (response_transport == CodexBackendTransport::WebSocket && terminal_failure.is_some()))
                && let Some(observation) = observation_state.observation(
                    terminal_failure.as_ref().map(|(failure, _)| &failure.error)
                )
            {
                yield ProviderEvent::observation(observation);
            }
            if let Some((mut failure, atomic_upstream_failure)) = terminal_failure {
                let failure_after_commit =
                    timing_signals.semantic_output || pre_commit_events.is_committed();
                if failure_after_commit {
                    for event in pre_commit_events.commit(events) {
                        yield event;
                    }
                } else if atomic_upstream_failure {
                    failure.error = failure
                        .error
                        .with_atomic_client_events(pre_commit_events.take_for_failure(events));
                }
                Err(quota_continuation_replay_error(
                    failure.error,
                    &request,
                    ReplayBoundary::from_semantic_output(failure_after_commit),
                ))?;
                return;
            }
            let events = pre_commit_events.stage(events, timing_signals, completed);
            for event in events {
                yield event;
            }
            if completed {
                // 成功出口（流内完成）：回填响应侧观测（Set-Cookie/票/真档位）。
                gateway_core::request_log::update_response(
                    &request_id,
                    codex_response_log_patch(
                        resp_has_cfbm,
                        session_capture.as_ref(),
                        resp_handshake_turn_state.as_deref(),
                        decoder.response_service_tier(),
                        decoder.response_model(),
                        &resp_cookie_summary,
                    ),
                );
                return;
            }
        }
        let (mut events, canonical_failure) = match decoder.finish() {
            CodexCanonicalOutcome::Events(events) => (events, None),
            CodexCanonicalOutcome::Failed(failure) => {
                let (events, error, semantic_output_seen) = failure.into_parts();
                (events, Some((error, semantic_output_seen)))
            }
        };
        let terminal_failure = canonical_failure.map(|(error, semantic_output_seen)| {
            log_canonical_upstream_error(
                UpstreamErrorLogContext::new(
                    &context,
                    &active_account,
                    websocket_connection_id,
                ),
                response_transport,
                &error,
            );
            let atomic_upstream_failure = matches!(&error, CodexCanonicalError::Upstream(_));
            (
                map_canonical_error(
                    error,
                    &failure_diagnostics,
                    &failure_set_cookie_headers,
                    &failure_rate_limit_headers,
                    ReplayBoundary::from_semantic_output(
                        semantic_output_seen || pre_commit_events.is_committed(),
                    ),
                ),
                atomic_upstream_failure,
            )
        });
        let timing_signals = decoder.take_timing_signals();
        let response_model_changed = observation_state
            .observe_upstream_response_model(decoder.response_model());
        let service_tier_changed = observation_state
            .observe_upstream_service_tier(decoder.response_service_tier());
        let timing_changed = observation_state
            .observe_timing_signals(timing_signals, output_started_at);
        let updates = take_rate_limit_updates(rate_limit_updates.as_ref()).await;
        let rate_limits_changed = if updates.is_empty() {
            false
        } else {
            passive_quota_observation.observe(&updates);
            observation_state.merge_rate_limit_headers(&rate_limit_update_headers(&updates))
        };
        if allows_account_state_mutation {
            synchronize_passive_quota(
                &quota,
                &active_account,
                passive_quota_observation.rate_limits(),
            )
            .await;
        }
        if let Some((failure, _)) = terminal_failure.as_ref() {
            apply_failure(&failure_context, &active_account, failure)
            .await;
        }
        let metadata_changed = merge_response_metadata_updates(
            response_metadata_updates.as_ref(),
            &mut session_capture,
            &mut observation_state,
            pin_attempt.as_mut(),
            &mut decoder,
        )
        .await
        .unwrap_or(false);
        attach_openai_session_update(&mut events, &mut session_capture);
        let completed = events
            .iter()
            .flat_map(ProviderEvent::canonical_facts)
            .any(|event| matches!(event, GatewayEvent::Completed(_)));
        if completed && terminal_failure.is_none() && !terminal_response_is_incomplete(&events)
            && let Some(pin) = pin_attempt.as_mut()
        { pin.completed(SystemTime::now()); }
        let terminal_changed = completed
            && observation_state.mark_completed(terminal_response_is_incomplete(&events));
        if response_transport == CodexBackendTransport::WebSocket
            && completed && terminal_failure.is_none()
            && let Some(key) = session_affinity_key.as_ref()
        {
            session_transport_recovery.websocket_succeeded(key);
        }
        if allows_account_state_mutation && completed && terminal_failure.is_none() {
            // 同上：尾部 finish() 也可能产出 completed，亲和记录必须先于任何下游 yield。
            selector
                .record_success(
                    &active_account,
                    session_affinity_key.as_ref(),
                    lease.affinity_expected_account_id(),
                )
                .await;
            selector
                .observe_cyber_policy_success(cyber_policy_scope.as_ref())
                .await;
        }
        if (response_model_changed
            || service_tier_changed
            || timing_changed
            || rate_limits_changed
            || metadata_changed
            || terminal_changed
            || (response_transport == CodexBackendTransport::WebSocket && terminal_failure.is_some()))
            && let Some(observation) = observation_state.observation(
                terminal_failure.as_ref().map(|(failure, _)| &failure.error)
            )
        {
            yield ProviderEvent::observation(observation);
        }
        if let Some((mut failure, atomic_upstream_failure)) = terminal_failure {
            let failure_after_commit =
                timing_signals.semantic_output || pre_commit_events.is_committed();
            if failure_after_commit {
                for event in pre_commit_events.commit(events) {
                    yield event;
                }
            } else if atomic_upstream_failure {
                failure.error = failure
                    .error
                    .with_atomic_client_events(pre_commit_events.take_for_failure(events));
            }
            Err(quota_continuation_replay_error(
                failure.error,
                &request,
                ReplayBoundary::from_semantic_output(failure_after_commit),
            ))?;
            return;
        }
        // 成功出口（流尾 finish）：回填响应侧观测（Set-Cookie/票/真档位）。
        gateway_core::request_log::update_response(
            &request_id,
            codex_response_log_patch(
                resp_has_cfbm,
                session_capture.as_ref(),
                resp_handshake_turn_state.as_deref(),
                decoder.response_service_tier(),
                decoder.response_model(),
                        &resp_cookie_summary,
            ),
        );
        let events = pre_commit_events.finish(events, timing_signals, completed);
        for event in events {
            yield event;
        }
    })
}

async fn merge_response_metadata_updates(
    updates: Option<&CodexResponseMetadataUpdates>,
    session_capture: &mut Option<OpenAiSessionCapture>,
    observation_state: &mut OpenAiResponseObservationState,
    pin: Option<&mut crate::turn_state_pin::PinAttempt>,
    decoder: &mut CodexCanonicalDecoder,
) -> Option<bool> {
    let updates = updates?;
    let mut pending = updates.lock().await;
    let turn_state = pending.turn_state.take();
    let reported_model = pending.reported_model.clone();
    drop(pending);
    if turn_state.is_none() && reported_model.is_none() {
        return None;
    }
    let mut changed = false;
    if let Some(turn_state) = turn_state {
        // turn-state pin 仍观测本回合的 state（仅在存在时），与上游 reported_model 处理并存。
        if let Some(pin) = pin {
            pin.observe(Some(&turn_state));
        }
        if let Some(capture) = session_capture.as_mut() {
            capture.turn_state = Some(turn_state.clone());
        }
        changed |= observation_state.merge_client_header("x-codex-turn-state", &turn_state);
    }
    if let Some(model) = reported_model {
        decoder.observe_reported_model(&model);
        changed |= observation_state.observe_upstream_response_model(decoder.response_model());
    }
    Some(changed)
}
