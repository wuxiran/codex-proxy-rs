//! fork 专属：turn-state 账号级 pin（自身成功候选冻结/失效号不冻结/WS 元数据回放）契约测试。
//! 与上游共享 contract.rs 解耦，避免每次合并在大文件里反复冲突。
//! 作为 contract 的子模块（mod fork），经 `super::*` 复用父模块的私有测试 helper 与 use 导入。
#![allow(unused_imports)]
use super::*;

#[tokio::test]
async fn turn_state_pin_freezes_own_successful_candidate_and_reset_or_disable_stops_replay() {
    let store = Arc::new(MemoryAccountStore::default());
    let account = "acct_provider_contract";
    create_account(&store, account).await;
    store.set_turn_state_pin(account, true);
    let server = MockServer::start().await;
    let provider = provider_with_base_url(&store, server.uri());
    let candidate = "a".repeat(292);
    for (index, response_state) in [
        candidate.clone(),
        "b".repeat(312),
        "c".repeat(292),
        "d".repeat(312),
        "e".repeat(312),
    ]
    .into_iter()
    .enumerate()
    {
        if index == 3 {
            store.set_turn_state_pin(account, true);
        }
        if index == 4 {
            store.set_turn_state_pin(account, false);
        }
        let mock = Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .insert_header("x-codex-turn-state", response_state)
                    .set_body_string(format!("data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_scope_capture\",\"model\":\"gpt-5.4\"}}}}\n\n{CAPTURE_COMPLETED_SSE}")),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let mut stream = provider
            .execute(
                planned_request("openai", http_generate_operation()),
                context(&format!("req_pin_{index}"), CancellationToken::new()),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        drop(mock);
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 5);
    for index in [0, 3, 4] {
        assert!(captured_header_values(&requests[index], "x-codex-turn-state").is_empty());
    }
    for index in [1, 2] {
        assert_eq!(
            captured_header_values(&requests[index], "x-codex-turn-state"),
            vec![candidate.as_bytes().to_vec()]
        );
    }
}

#[tokio::test]
async fn turn_state_pin_ignores_failed_responses_and_client_supplied_candidates() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    store.set_turn_state_pin("acct_provider_contract", true);
    let server = MockServer::start().await;
    let provider = provider_with_base_url(&store, server.uri());
    let candidate = "a".repeat(292);
    for index in 0..3 {
        let body = if index == 0 {
            "data: {\"type\":\"response.failed\",\"response\":{\"id\":\"resp_pin_failed\",\"status\":\"failed\",\"error\":{\"code\":\"server_is_overloaded\",\"message\":\"busy\"}}}\n\n"
        } else {
            CAPTURE_COMPLETED_SSE
        };
        let mock = Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .insert_header(
                        "x-codex-turn-state",
                        if index == 0 {
                            candidate.clone()
                        } else {
                            "b".repeat(312)
                        },
                    )
                    .set_body_string(body),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(
                "openai",
                json!({"model":"gpt-5.4", "input":"test"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap()
            .with_context(Map::from_iter([
                ("use_websocket".to_owned(), json!(false)),
                ("turn_state".to_owned(), json!(candidate)),
            ])),
        ));
        let mut stream = provider
            .execute(
                planned_request("openai", operation),
                context(&format!("req_pin_failed_{index}"), CancellationToken::new()),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            if index == 0 {
                if event.is_err() {
                    break;
                }
            } else {
                event.unwrap();
            }
        }
        drop(mock);
    }
    for request in server.received_requests().await.unwrap() {
        assert!(captured_header_values(&request, "x-codex-turn-state").is_empty());
    }
}

#[tokio::test]
async fn turn_state_pin_websocket_metadata_is_replayed_in_next_turn_payload() {
    for (plan, model, length) in [
        ("pro", "gpt-5.4", 292),
        ("pro", "gpt-5.6-terra", 292),
        ("team", "gpt-5.5", 332),
        ("team", "gpt-5.6-sol", 332),
        ("team", "gpt-5.6-terra", 356),
        ("team", "gpt-6-astra", 332),
        ("business", "gpt-5.6-terra", 356),
        ("self_serve_business_prolite", "gpt-5.6-terra", 356),
        ("self_serve_business_usage_based", "gpt-6-astra", 332),
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        let mut account_profile = profile("chatgpt-ws-pin");
        account_profile.plan_type = Some(plan.to_owned());
        store
            .seed_oauth_credential(ImportCodexOAuthCredential {
                account_id: "acct_websocket_turn_state".to_owned(),
                name: "ws-pin".to_owned(),
                secret: secret("ws-pin-test-token"),
                verified_account: account_profile,
                next_refresh_at: None,
                enabled: true,
            })
            .await;
        store.set_turn_state_pin("acct_websocket_turn_state", true);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let candidate = "w".repeat(length);
        let expected = candidate.clone();
        let server = tokio::spawn(async move {
            let mut received = Vec::new();
            for round in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut websocket = accept_codex_test_websocket(stream).await;
                let request = websocket.next().await.unwrap().unwrap();
                let request: Value = serde_json::from_str(request.to_text().unwrap()).unwrap();
                received.push(request);
                for event in [
                    json!({"type":"response.created","response":{"id":format!("resp_pin_ws_{round}"),"model":model}}),
                    json!({"type":"response.metadata","headers":{"x-codex-turn-state": if round==0 {candidate.clone()} else {"b".repeat(312)}}}),
                    json!({"type":"response.completed","response":{"id":format!("resp_pin_ws_{round}"),"model":model,"status":"completed","output":[],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
                ] {
                    websocket
                        .send(Message::Text(event.to_string().into()))
                        .await
                        .unwrap();
                }
                websocket.close(None).await.unwrap();
            }
            received
        });
        let provider = provider_with_base_url(&store, base_url);
        for index in 0..2 {
            let mut stream = provider
                .execute(
                    planned_request_for_model(
                        "openai",
                        Operation::Generate(GenerateRequest::from_protocol_payload(
                            ProtocolPayload::json_object(
                                "openai",
                                Map::from_iter([
                                    ("model".to_owned(), json!(model)),
                                    ("input".to_owned(), json!("hello")),
                                ]),
                            )
                            .unwrap(),
                        )),
                        model,
                    ),
                    context(&format!("req_pin_ws_{index}"), CancellationToken::new()),
                )
                .await
                .unwrap();
            while let Some(event) = stream.next().await {
                event.unwrap();
            }
        }
        let requests = timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        assert!(
            requests[0]
                .pointer("/client_metadata/x-codex-turn-state")
                .is_none()
        );
        assert_eq!(
            requests[1].pointer("/client_metadata/x-codex-turn-state"),
            Some(&json!(expected))
        );
    }
}
