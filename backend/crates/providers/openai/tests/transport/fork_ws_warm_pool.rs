//! fork：WS 暖池（保活连接被业务新对话领养）的连接池测试。

use super::*;

#[tokio::test]
async fn adopted_connection_cannot_start_a_new_chain_after_probe_proof_expires() {
    verification_lifecycle_case("expiry").await;
}

#[tokio::test]
async fn adopted_connection_cannot_keep_old_verification_after_ticket_changes() {
    verification_lifecycle_case("ticket").await;
}

#[tokio::test]
async fn adopted_connection_cannot_keep_old_verification_after_policy_changes() {
    verification_lifecycle_case("policy").await;
}

#[tokio::test]
async fn proof_expiry_does_not_break_an_existing_connection_local_continuation() {
    verification_lifecycle_case("continuation").await;
}

async fn verification_lifecycle_case(change: &str) {
    let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap());
    let address = listener.local_addr().unwrap();
    let accepts = Arc::clone(&listener);
    let continued = change == "continuation";
    let server = tokio::spawn(async move {
        let (stream, _) = accepts.accept().await.unwrap();
        let mut ws = accept_codex_test_websocket(stream).await;
        let mut received = 0;
        while let Some(Ok(message)) = ws.next().await {
            match message {
                Message::Text(text) => {
                    received += 1;
                    let id = match received {
                        1 => "resp_lifecycle_warm",
                        2 => "resp_lifecycle_business",
                        3 if continued => {
                            let body: serde_json::Value = serde_json::from_str(&text).unwrap();
                            assert_eq!(body["previous_response_id"], "resp_lifecycle_business");
                            "resp_lifecycle_continued"
                        }
                        _ => panic!(
                            "a new chain reached a connection whose verification no longer matches"
                        ),
                    };
                    ws.send(Message::Text(completed_websocket_response(id, 1, 0).into()))
                        .await
                        .unwrap();
                    if received == 3 {
                        ws.close(None).await.unwrap();
                        break;
                    }
                }
                Message::Ping(bytes) => ws.send(Message::Pong(bytes)).await.unwrap(),
                Message::Close(_) => break,
                _ => {}
            }
        }
        assert_eq!(received, if continued { 3 } else { 2 });
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{address}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::new(Duration::from_secs(3000))));
    let mut warm = pooled_websocket_request("__cpr_warm__:0");
    let approval = provider_openai::transport::WarmConnectionApproval::new(
        warm.model().to_owned(),
        Duration::from_secs(3000),
    );
    warm.warm_approval = Some(approval.clone());
    backend
        .create_response(
            &warm,
            request_context("req_lifecycle_warm", Some("fixture-account")),
        )
        .await
        .unwrap();
    approval.publish_scoped(
        true,
        Duration::from_secs(240),
        Some("synthetic-a"),
        Some([1; 32]),
    );
    let mut business = pooled_websocket_request("verification-lifecycle");
    business.set_previous_response_id(None);
    business.previous_response_scope = None;
    business.warm_verification_policy = Some([1; 32]);
    business.require_verified_warm = true;
    let first = backend
        .create_response(
            &business,
            request_context("req_lifecycle_first", Some("fixture-account")),
        )
        .await
        .unwrap();
    assert_eq!(first.websocket_pool_decision.unwrap().kind(), "reuse");
    match change {
        "ticket" => business.turn_state = Some("synthetic-b".to_owned()),
        "policy" => business.warm_verification_policy = Some([2; 32]),
        _ => {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(241)).await;
            tokio::time::resume();
        }
    }
    if continued {
        business.require_verified_warm = false;
        business.set_previous_response_id(Some("resp_lifecycle_business".to_owned()));
        business.previous_response_scope = Some(PreviousResponseScope::ConnectionLocal);
        let response = backend
            .create_response(
                &business,
                request_context("req_lifecycle_continue", Some("fixture-account")),
            )
            .await
            .unwrap();
        assert!(response.body.contains("resp_lifecycle_continued"));
        assert_eq!(response.websocket_pool_decision.unwrap().kind(), "reuse");
    } else {
        let error = backend
            .create_response(
                &business,
                request_context("req_lifecycle_rejected", Some("fixture-account")),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            CodexClientError::WebSocket(CodexWebSocketExchangeError::WarmUnavailable)
        ));
    }
    timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
    assert!(
        timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn verified_only_request_never_opens_an_unverified_upstream_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::new(Duration::from_secs(60))));
    let mut request = pooled_websocket_request("verified-required");
    request.require_verified_warm = true;
    let error = timeout(
        Duration::from_secs(4),
        backend.create_response(&request, request_context("req_verified_only", Some("acct"))),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(matches!(
        error,
        CodexClientError::WebSocket(CodexWebSocketExchangeError::WarmUnavailable)
    ));
    assert!(
        timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
    request.force_http_sse = true;
    let error = backend
        .create_response(
            &request,
            request_context("req_verified_http_only", Some("acct")),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        CodexClientError::WebSocket(CodexWebSocketExchangeError::WarmUnavailable)
    ));
    assert!(
        timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn business_should_not_adopt_an_unverified_warm_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut candidate = accept_codex_test_websocket(stream).await;
        candidate.next().await.unwrap().unwrap();
        candidate
            .send(Message::Text(
                completed_websocket_response("resp_candidate", 1, 0).into(),
            ))
            .await
            .unwrap();
        // 两条路径都回包，让错误复用表现为断言失败，而不是等待第二条连接直到超时。
        tokio::select! {
            accepted = listener.accept() => {
                let mut business = accept_codex_test_websocket(accepted.unwrap().0).await;
                business.next().await.unwrap().unwrap();
                business.send(Message::Text(completed_websocket_response("resp_checked_business", 1, 0).into())).await.unwrap();
                business.close(None).await.unwrap();
            }
            _ = candidate.next() => {
                candidate.send(Message::Text(completed_websocket_response("resp_unchecked_business", 1, 0).into())).await.unwrap();
            }
        }
        candidate.close(None).await.unwrap();
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::new(Duration::from_mins(5))));
    backend
        .create_response(
            &pooled_websocket_request("__cpr_warm__:0"),
            request_context("req_candidate", Some("chatgpt-account")),
        )
        .await
        .unwrap();
    let business = backend
        .create_response(
            &pooled_websocket_request("business-after-unverified"),
            request_context("req_business_after_unverified", Some("chatgpt-account")),
        )
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(business.websocket_pool_decision.unwrap().kind(), "new");
    assert!(business.body.contains("resp_checked_business"));
}

#[tokio::test]
async fn business_new_chain_should_adopt_a_held_warm_connection() {
    // 一条在保活 conversation（__cpr_warm__:*）上建立的连接，应被后续新对话（不同 conversation）
    // 领养复用，而不是新拨一条 TCP 连接。这验证 acquire 的暖池领养分支。
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        accepted_for_server.fetch_add(1, Ordering::SeqCst);
        let mut websocket = accept_codex_test_websocket(stream).await;
        for response_id in ["resp_warm_open", "resp_business_adopt"] {
            let _message = websocket.next().await.unwrap().unwrap();
            websocket
                .send(Message::Text(
                    completed_websocket_response(response_id, 1, 0).into(),
                ))
                .await
                .unwrap();
        }
        websocket.close(None).await.unwrap();
    });

    let pool = Arc::new(CodexWebSocketPool::new(Duration::from_mins(5)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool));

    // warmer 打开一条保活连接（conversation 以保活前缀命名）。
    let mut warm_request = pooled_websocket_request("__cpr_warm__:0");
    let approval = provider_openai::transport::WarmConnectionApproval::new(
        warm_request.model().to_owned(),
        Duration::from_mins(5),
    );
    warm_request.warm_approval = Some(approval.clone());
    let warm = backend
        .create_response(
            &warm_request,
            request_context("req_warm_open", Some("chatgpt-account")),
        )
        .await
        .expect("warm connection should open");
    assert_eq!(warm.websocket_pool_decision.unwrap().kind(), "new");
    approval.publish(true);

    // 一条全新业务对话：没有自己的连接，应领养上面那条保活连接（reuse，不新拨）。
    let business = backend
        .create_response(
            &pooled_websocket_request("business-conversation"),
            request_context("req_business_adopt", Some("chatgpt-account")),
        )
        .await
        .expect("business request should adopt the warm connection");
    server.await.unwrap();

    assert!(warm.body.contains("resp_warm_open"));
    assert!(business.body.contains("resp_business_adopt"));
    assert_eq!(
        business.websocket_pool_decision.unwrap().kind(),
        "reuse",
        "business new-chain should adopt the warm connection"
    );
    assert_eq!(
        accepted_connections.load(Ordering::SeqCst),
        1,
        "adoption must not open a second upstream connection"
    );
}

#[tokio::test]
async fn business_should_not_adopt_when_warm_reuse_is_disabled() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        for response_id in ["resp_warm_open2", "resp_business_fresh"] {
            let (stream, _) = listener.accept().await.unwrap();
            accepted_for_server.fetch_add(1, Ordering::SeqCst);
            let mut websocket = accept_codex_test_websocket(stream).await;
            let _message = websocket.next().await.unwrap().unwrap();
            websocket
                .send(Message::Text(
                    completed_websocket_response(response_id, 1, 0).into(),
                ))
                .await
                .unwrap();
            websocket.close(None).await.unwrap();
        }
    });

    let pool = Arc::new(CodexWebSocketPool::new(Duration::from_mins(5)));
    pool.set_warm_reuse(false);
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::clone(&pool));

    let _warm = backend
        .create_response(
            &pooled_websocket_request("__cpr_warm__:0"),
            request_context("req_warm_open2", Some("chatgpt-account")),
        )
        .await
        .expect("warm connection should open");
    let business = backend
        .create_response(
            &pooled_websocket_request("business-conversation-2"),
            request_context("req_business_fresh", Some("chatgpt-account")),
        )
        .await
        .expect("business request should open its own connection");
    server.await.unwrap();

    assert_eq!(
        business.websocket_pool_decision.unwrap().kind(),
        "new",
        "with reuse disabled the business request must dial fresh"
    );
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}
