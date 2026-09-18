//! 使用真实 loopback HTTP/SSE/WS 验证恢复请求、交付边界与摘要缓存。

use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures::{SinkExt, StreamExt};
use gateway_core::account::{ProviderAccountId, ProviderAccountStore as _};
use gateway_core::engine::{
    AccountAttemptContext, AttemptContext, ModelRequestId, RequestAttemptContext,
};
use gateway_core::error::{ContinuationRecoveryDisposition, ProviderErrorKind};
use gateway_core::event::GatewayEvent;
use gateway_core::lifecycle::CancellationToken;
use gateway_core::operation::{GenerateRequest, Operation, ProtocolPayload};
use gateway_core::policy::ClientApiKeyId;
use serde_json::{Map, Value, json};
use tokio::{net::TcpListener, time::timeout};
use tokio_tungstenite::tungstenite::Message;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::contract::{
    CAPTURE_COMPLETED_SSE, captured_request_body, context, context_with_state_owner,
    contract_account_scope, create_account, planned_request, planned_request_for_model,
    provider_with_base_url,
};
use crate::support::{MemoryAccountStore, account_policy};
use crate::transport::accept_codex_test_websocket;
use gateway_core::engine::provider::Provider as _;

fn encrypted_recovery_operation(input: Value, session: Option<&str>, websocket: bool) -> Operation {
    let mut body = json!({"model":"gpt-5.4", "input":input, "store":false});
    if let Some(session) = session {
        body["session_id"] = json!(session);
    }
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
            .unwrap()
            .with_context(Map::from_iter([(
                "use_websocket".to_owned(),
                json!(websocket),
            )])),
    ))
}

fn encrypted_recovery_history() -> Value {
    json!([
        {"type":"message", "role":"user", "content":"keep this question"},
        {"type":"reasoning", "id":"rs_invalid", "summary":[], "encrypted_content":"mock-invalid-ciphertext"},
        {"type":"function_call", "call_id":"call_mock", "name":"lookup", "arguments":"{}"},
        {"type":"function_call_output", "call_id":"call_mock", "output":"keep tool result"},
        {"type":"message", "role":"user", "content":"continue"}
    ])
}

fn encrypted_rejection() -> Value {
    json!({"error":{"code":"invalid_encrypted_content", "type":"invalid_request_error", "message":"Mock encrypted content rejected"}})
}

#[tokio::test]
async fn encrypted_recovery_http_replays_once_preserving_history_and_single_usage() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = Arc::clone(&attempts);
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(move |_: &wiremock::Request| {
            if count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                ResponseTemplate::new(400).set_body_json(encrypted_rejection())
            } else {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(encrypted_success_sse())
            }
        })
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    let input = encrypted_recovery_history();
    let operation = encrypted_recovery_operation(input.clone(), Some("encrypted-session"), false);
    let mut stream = provider
        .execute(
            planned_request("openai", operation),
            context("req_encrypted_recovery", CancellationToken::new()),
        )
        .await
        .unwrap();
    let mut usage_count = 0;
    let mut cost_count = 0;
    let mut completed_count = 0;
    while let Some(event) = stream.next().await {
        for fact in event
            .expect("invalid ciphertext should recover")
            .canonical_facts()
        {
            usage_count += usize::from(matches!(fact, GatewayEvent::Usage(_)));
            cost_count += usize::from(matches!(fact, GatewayEvent::CalculatedCost(_)));
            completed_count += usize::from(matches!(fact, GatewayEvent::Completed(_)));
        }
    }
    assert_eq!((usage_count, completed_count, cost_count), (1, 1, 1));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let first = captured_request_body(&requests[0]);
    let second = captured_request_body(&requests[1]);
    assert_eq!(first["input"], input);
    let mut expected = first;
    expected["input"].as_array_mut().unwrap().remove(1);
    assert_eq!(second, expected);
    assert_eq!(
        requests[0].headers.get("authorization"),
        requests[1].headers.get("authorization")
    );
}

fn encrypted_success_sse() -> String {
    format!(
        "event: response.created\ndata: {}\n\n{}",
        json!({"type":"response.created", "response":{"id":"resp_scope_capture", "model":"gpt-5.4", "status":"in_progress"}}),
        CAPTURE_COMPLETED_SSE
    )
}

#[tokio::test]
async fn encrypted_recovery_http_rejection_budget_and_fail_closed_inputs() {
    let mut orphan = encrypted_recovery_history();
    orphan.as_array_mut().unwrap().remove(2);
    for (input, code, status, expected) in [
        (
            encrypted_recovery_history(),
            "invalid_encrypted_content",
            400,
            2,
        ),
        (
            encrypted_recovery_history(),
            "INVALID_ENCRYPTED_CONTENT",
            400,
            1,
        ),
        (
            encrypted_recovery_history(),
            "invalid_request_error",
            400,
            1,
        ),
        (
            encrypted_recovery_history(),
            "invalid_encrypted_content",
            401,
            1,
        ),
        (
            encrypted_recovery_history(),
            "invalid_encrypted_content",
            429,
            1,
        ),
        (
            encrypted_recovery_history(),
            "invalid_encrypted_content",
            500,
            1,
        ),
        (
            json!([{"role":"user", "content":"no ciphertext"}]),
            "invalid_encrypted_content",
            400,
            1,
        ),
        (
            json!([{"type":"compaction", "encrypted_content":"mock-compaction"}]),
            "invalid_encrypted_content",
            400,
            1,
        ),
        (
            json!([{"role":"user", "content":"keep"}, {"type":"reasoning", "encrypted_content":"mock"}, {"type":"item_reference", "id":"item_mock"}]),
            "invalid_encrypted_content",
            400,
            1,
        ),
        (orphan, "invalid_encrypted_content", 400, 1),
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/codex/responses"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({"error":{"code":code,"message":"invalid_encrypted_content in free text is insufficient"}})))
            .mount(&server).await;
        let provider = provider_with_base_url(&store, server.uri());
        let mut stream = provider
            .execute(
                planned_request(
                    "openai",
                    encrypted_recovery_operation(input.clone(), None, false),
                ),
                context("req_encrypted_budget", CancellationToken::new()),
            )
            .await
            .unwrap();
        let error = loop {
            match stream.next().await {
                Some(Ok(_)) => {}
                Some(Err(error)) => break error,
                None => panic!("expected failure"),
            }
        };
        if code == "invalid_encrypted_content" {
            assert_eq!(
                error.continuation_recovery_disposition(),
                Some(ContinuationRecoveryDisposition::ClientReplayRequired)
            );
        }
        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests.len(),
            expected,
            "code={code}, status={status}, input={input}"
        );
        assert_eq!(captured_request_body(&requests[0])["input"], input);
    }
}

#[tokio::test]
async fn encrypted_recovery_cache_is_session_scoped_preserves_new_reasoning_and_expires() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(|request: &wiremock::Request| {
            let body = captured_request_body(request);
            if body["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["encrypted_content"] == "mock-invalid-ciphertext")
            {
                ResponseTemplate::new(400).set_body_json(encrypted_rejection())
            } else {
                ResponseTemplate::new(200)
                    .set_body_raw(encrypted_success_sse(), "text/event-stream")
            }
        })
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    for (index, session, expected_count) in [
        (0, Some("session-one"), 2),
        (1, Some("session-one"), 1),
        (2, Some("session-two"), 2),
        (3, None, 2),
        (4, None, 2),
        (5, Some("session-one"), 2),
    ] {
        if index == 5 {
            // 仅推进本测试的缓存时钟；真实网络 I/O 前恢复时钟，避免自动时间推进。
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(601)).await;
            tokio::time::resume();
        }
        let before = server.received_requests().await.unwrap().len();
        let mut input = encrypted_recovery_history();
        if index == 1 {
            input.as_array_mut().unwrap().push(json!({"type":"reasoning", "summary":[], "encrypted_content":"mock-new-valid-ciphertext"}));
        }
        let mut stream = provider
            .execute(
                planned_request(
                    "openai",
                    encrypted_recovery_operation(input, session, false),
                ),
                context(&format!("req_cache_{index}"), CancellationToken::new()),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.expect("recovery or preclean succeeds");
        }
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len() - before, expected_count, "step {index}");
        if index == 1 {
            assert_eq!(
                captured_request_body(requests.last().unwrap())["input"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()["encrypted_content"],
                "mock-new-valid-ciphertext"
            );
        }
    }
}

#[tokio::test]
async fn encrypted_recovery_stream_respects_output_and_tool_boundaries() {
    for websocket in [false, true] {
        for (prefix, should_recover) in [
            (vec![], true),
            (
                vec![
                    json!({"type":"response.created", "response":{"id":"resp_mock", "model":"gpt-5.4", "output":[]}}),
                ],
                true,
            ),
            (
                vec![
                    json!({"type":"response.created", "response":{"id":"resp_mock", "model":"gpt-5.4"}}),
                    json!({"type":"response.output_text.delta", "output_index":0, "content_index":0, "delta":"delivered"}),
                ],
                false,
            ),
            (
                vec![
                    json!({"type":"response.output_item.added", "output_index":0, "item":{"type":"function_call", "call_id":"call_side_effect", "name":"write", "arguments":""}}),
                ],
                false,
            ),
            (
                vec![
                    json!({"type":"response.web_search_call.in_progress", "output_index":0, "item_id":"tool_mock"}),
                ],
                false,
            ),
            (
                vec![json!({"type":"response.unknown_event", "payload":"unknown output state"})],
                false,
            ),
        ] {
            let store = Arc::new(MemoryAccountStore::default());
            create_account(&store, "acct_provider_contract").await;
            let mut rejected_events = prefix;
            rejected_events.push(json!({"type":"response.failed", "response":{"id":"resp_mock", "status":"failed", "error":encrypted_rejection()["error"]}}));
            let captures = Arc::new(Mutex::new(Vec::<Value>::new()));
            let captured = Arc::clone(&captures);
            let (url, http, task) = if websocket {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("http://{}", listener.local_addr().unwrap());
                let task = tokio::spawn(async move {
                    let (socket, _) = listener.accept().await.unwrap();
                    let mut ws = accept_codex_test_websocket(socket).await;
                    let request = ws.next().await.unwrap().unwrap().into_text().unwrap();
                    captured
                        .lock()
                        .unwrap()
                        .push(serde_json::from_str(&request).unwrap());
                    for event in rejected_events {
                        ws.send(Message::Text(event.to_string().into()))
                            .await
                            .unwrap();
                    }
                    // 错误后 transport 可复用或关闭连接；两者均必须只发送一个恢复 payload。
                    if should_recover {
                        let next = timeout(Duration::from_secs(2), ws.next()).await.unwrap();
                        let request = match next {
                            Some(Ok(Message::Text(request))) => request,
                            _ => {
                                let (socket, _) =
                                    timeout(Duration::from_secs(2), listener.accept())
                                        .await
                                        .unwrap()
                                        .unwrap();
                                ws = accept_codex_test_websocket(socket).await;
                                ws.next().await.unwrap().unwrap().into_text().unwrap()
                            }
                        };
                        captured
                            .lock()
                            .unwrap()
                            .push(serde_json::from_str(&request).unwrap());
                        for event in [
                            json!({"type":"response.created", "response":{"id":"resp_success", "model":"gpt-5.4"}}),
                            json!({"type":"response.completed", "response":{"id":"resp_success", "model":"gpt-5.4", "status":"completed", "output":[], "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
                        ] {
                            ws.send(Message::Text(event.to_string().into()))
                                .await
                                .unwrap();
                        }
                    }
                });
                (url, None, Some(task))
            } else {
                let server = MockServer::start().await;
                let frames = rejected_events
                    .iter()
                    .map(|event| {
                        format!(
                            "event: {}\ndata: {event}\n\n",
                            event["type"].as_str().unwrap()
                        )
                    })
                    .collect::<String>();
                Mock::given(method("POST"))
                    .and(path("/codex/responses"))
                    .respond_with(move |request: &wiremock::Request| {
                        let mut bodies = captured.lock().unwrap();
                        bodies.push(captured_request_body(request));
                        ResponseTemplate::new(200).set_body_raw(
                            if bodies.len() == 1 {
                                frames.clone()
                            } else {
                                encrypted_success_sse()
                            },
                            "text/event-stream",
                        )
                    })
                    .mount(&server)
                    .await;
                (server.uri(), Some(server), None)
            };
            let provider = provider_with_base_url(&store, url);
            let mut stream = provider
                .execute(
                    planned_request(
                        "openai",
                        encrypted_recovery_operation(encrypted_recovery_history(), None, websocket),
                    ),
                    context("req_encrypted_stream", CancellationToken::new()),
                )
                .await
                .unwrap();
            let mut failure = None;
            let mut completed = 0;
            while let Some(event) = stream.next().await {
                match event {
                    Ok(event) => {
                        completed += event
                            .canonical_facts()
                            .iter()
                            .filter(|fact| matches!(fact, GatewayEvent::Completed(_)))
                            .count()
                    }
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
            if let Some(task) = task {
                task.await.unwrap();
            }
            assert_eq!(failure.is_none(), should_recover, "websocket={websocket}");
            assert_eq!(completed, usize::from(should_recover));
            let bodies = captures.lock().unwrap();
            assert_eq!(bodies.len(), if should_recover { 2 } else { 1 });
            if should_recover {
                let mut expected = bodies[0].clone();
                expected["input"].as_array_mut().unwrap().remove(1);
                let mut actual = bodies[1].clone();
                // 每次 WS payload 的发送时间由 transport 生成，其余元数据必须保持。
                if websocket {
                    assert!(
                        actual["client_metadata"]["x-codex-ws-stream-request-start-ms"].is_string()
                    );
                    actual["client_metadata"]
                        .as_object_mut()
                        .unwrap()
                        .remove("x-codex-ws-stream-request-start-ms");
                    expected["client_metadata"]
                        .as_object_mut()
                        .unwrap()
                        .remove("x-codex-ws-stream-request-start-ms");
                }
                assert_eq!(actual, expected);
            }
            drop(http);
        }
    }
}

fn encrypted_scope_context(key: &str, account: &str) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_scope_recovery").unwrap(),
            ClientApiKeyId::new(key).unwrap(),
        ),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(10),
        account_policy(),
        AccountAttemptContext::diagnostic(
            BTreeSet::new(),
            ProviderAccountId::new(account).unwrap(),
            None,
        ),
        None,
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn encrypted_recovery_cache_isolates_account_key_model_and_credential_revision() {
    use gateway_core::account::{CredentialCasUpdate, ProviderAccountUpdate};
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    create_account(&store, "acct_scope_other").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(|request: &wiremock::Request| {
            if captured_request_body(request)["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["type"] == "reasoning")
            {
                ResponseTemplate::new(400).set_body_json(encrypted_rejection())
            } else {
                ResponseTemplate::new(200)
                    .set_body_raw(encrypted_success_sse(), "text/event-stream")
            }
        })
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    for (index, key, account, model, count) in [
        (0, "key_one", "acct_provider_contract", "gpt-5.4", 2),
        (1, "key_one", "acct_provider_contract", "gpt-5.4", 1),
        (2, "key_two", "acct_provider_contract", "gpt-5.4", 2),
        (3, "key_one", "acct_scope_other", "gpt-5.4", 2),
        (4, "key_one", "acct_provider_contract", "gpt-5.3-codex", 2),
        (5, "key_one", "acct_provider_contract", "gpt-5.4", 2),
    ] {
        if index == 5 {
            let id = ProviderAccountId::new(account).unwrap();
            let loaded = store.load_current_credential(&id).await.unwrap();
            let update = CredentialCasUpdate::new(
                id.clone(),
                loaded.account.revision(),
                ProviderAccountUpdate {
                    account_id: id,
                    name: account.to_owned(),
                    email: None,
                    plan_type: None,
                },
                loaded.credential,
                false,
                None,
                None,
            )
            .unwrap()
            .preserving_profile();
            store.compare_and_swap_credential(update).await.unwrap();
        }
        let before = server.received_requests().await.unwrap().len();
        let operation = encrypted_recovery_operation(
            encrypted_recovery_history(),
            Some("shared-session"),
            false,
        );
        let mut stream = provider
            .execute(
                planned_request_for_model("openai", operation, model),
                encrypted_scope_context(key, account),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len() - before, count, "scope step {index}");
        assert_eq!(captured_request_body(&requests[before])["model"], model);
    }
}

#[tokio::test]
async fn encrypted_recovery_cache_capacity_evicts_oldest_digest() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(|request: &wiremock::Request| {
            if captured_request_body(request)["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["type"] == "reasoning")
            {
                ResponseTemplate::new(400).set_body_json(encrypted_rejection())
            } else {
                ResponseTemplate::new(200)
                    .set_body_raw(encrypted_success_sse(), "text/event-stream")
            }
        })
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    for (digests, expected) in [(0..1025, 2), (1..2, 1), (0..1, 2)] {
        let before = server.received_requests().await.unwrap().len();
        let mut input = vec![json!({"role":"user", "content":"preserved"})];
        input.extend(digests.map(|index| json!({"type":"reasoning", "summary":[], "encrypted_content":format!("mock-cipher-{index}")})));
        let mut stream = provider
            .execute(
                planned_request(
                    "openai",
                    encrypted_recovery_operation(json!(input), Some("capacity-session"), false),
                ),
                context("req_capacity_cache", CancellationToken::new()),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        assert_eq!(
            server.received_requests().await.unwrap().len() - before,
            expected
        );
    }
}

#[tokio::test]
async fn encrypted_recovery_never_drops_previous_response_or_conversation() {
    for field in ["previous_response_id", "conversation"] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .respond_with(ResponseTemplate::new(400).set_body_json(encrypted_rejection()))
            .mount(&server)
            .await;
        let mut body = json!({"model":"gpt-5.4", "input":encrypted_recovery_history()});
        body[field] = json!("ref_context_must_be_preserved");
        let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
                .unwrap()
                .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))])),
        ));
        let provider = provider_with_base_url(&store, server.uri());
        let mut stream = provider
            .execute(
                planned_request("openai", operation),
                context_with_state_owner("req_previous_ref", "acct_provider_contract"),
            )
            .await
            .unwrap();
        let error = loop {
            match stream.next().await {
                Some(Ok(_)) => {}
                Some(Err(error)) => break error,
                None => panic!("expected rejection"),
            }
        };
        assert_eq!(
            error.continuation_recovery_disposition(),
            Some(ContinuationRecoveryDisposition::ClientReplayRequired)
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let actual = captured_request_body(&requests[0]);
        assert_eq!(actual[field], body[field]);
        assert_eq!(actual["input"], body["input"]);
    }
}

#[tokio::test]
async fn encrypted_recovery_websocket_wrapped_error_is_bounded_and_disconnect_is_not_replayed() {
    for failure_mode in ["error", "disconnect", "timeout"] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let captures = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured = Arc::clone(&captures);
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = accept_codex_test_websocket(socket).await;
            let request = ws.next().await.unwrap().unwrap().into_text().unwrap();
            captured
                .lock()
                .unwrap()
                .push(serde_json::from_str(&request).unwrap());
            if failure_mode == "error" {
                let rejected =
                    json!({"type":"error", "status":400, "error":encrypted_rejection()["error"]});
                ws.send(Message::Text(rejected.to_string().into()))
                    .await
                    .unwrap();
                let next = timeout(Duration::from_secs(2), ws.next()).await.unwrap();
                let request = match next {
                    Some(Ok(Message::Text(request))) => request,
                    _ => {
                        let (socket, _) = timeout(Duration::from_secs(2), listener.accept())
                            .await
                            .unwrap()
                            .unwrap();
                        ws = accept_codex_test_websocket(socket).await;
                        ws.next().await.unwrap().unwrap().into_text().unwrap()
                    }
                };
                captured
                    .lock()
                    .unwrap()
                    .push(serde_json::from_str(&request).unwrap());
                ws.send(Message::Text(rejected.to_string().into()))
                    .await
                    .unwrap();
            } else if failure_mode == "timeout" {
                tokio::time::sleep(Duration::from_millis(800)).await;
            } else {
                ws.close(None).await.unwrap();
            }
        });
        let provider = provider_with_base_url(&store, url);
        let deadline_context = AttemptContext::new(
            RequestAttemptContext::new(
                ModelRequestId::new("req_ws_no_repeat").unwrap(),
                ClientApiKeyId::new("key_openai_contract").unwrap(),
            ),
            NonZeroU32::new(1).unwrap(),
            SystemTime::now()
                + Duration::from_millis(if failure_mode == "timeout" { 400 } else { 5000 }),
            account_policy(),
            AccountAttemptContext::new(BTreeSet::new(), None, None)
                .with_account_scope(contract_account_scope()),
            None,
            CancellationToken::new(),
        );
        let mut stream = provider
            .execute(
                planned_request(
                    "openai",
                    encrypted_recovery_operation(encrypted_recovery_history(), None, true),
                ),
                deadline_context,
            )
            .await
            .unwrap();
        let error = loop {
            match stream.next().await {
                Some(Ok(_)) => {}
                Some(Err(error)) => break error,
                None => panic!("expected rejection"),
            }
        };
        task.await.unwrap();
        assert_eq!(
            captures.lock().unwrap().len(),
            if failure_mode == "error" { 2 } else { 1 }
        );
        if failure_mode == "error" {
            assert_eq!(
                error.continuation_recovery_disposition(),
                Some(ContinuationRecoveryDisposition::ClientReplayRequired)
            );
        } else if failure_mode == "timeout" {
            assert_eq!(error.kind(), ProviderErrorKind::Timeout);
        }
    }
}

#[tokio::test]
async fn encrypted_recovery_success_keeps_opaque_reasoning_and_never_replays_completed_output() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    let late_error = json!({"type":"error", "error":encrypted_rejection()["error"]});
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(
                "{}event: error\ndata: {late_error}\n\n",
                encrypted_success_sse()
            ),
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    let input = encrypted_recovery_history();
    let mut stream = provider
        .execute(
            planned_request(
                "openai",
                encrypted_recovery_operation(input.clone(), None, false),
            ),
            context("req_normal_reasoning", CancellationToken::new()),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        if event.is_err() {
            break;
        }
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(captured_request_body(&requests[0])["input"], input);
}
