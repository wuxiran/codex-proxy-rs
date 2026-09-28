//! fork：WS 暖池（保活连接被业务新对话领养）的连接池测试。

use super::*;

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
    let warm = backend
        .create_response(
            &pooled_websocket_request("__cpr_warm__:0"),
            request_context("req_warm_open", Some("chatgpt-account")),
        )
        .await
        .expect("warm connection should open");
    assert_eq!(warm.websocket_pool_decision.unwrap().kind(), "new");

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
