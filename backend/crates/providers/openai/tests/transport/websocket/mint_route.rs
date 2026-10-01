use super::*;
use sha2::{Digest as _, Sha256};

fn fingerprint(label: &str) -> String {
    hex::encode(Sha256::digest(
        format!("cflb-{label}\0oailb-{label}").as_bytes(),
    ))
}

#[tokio::test]
async fn minted_ticket_is_rejected_before_payload_when_handshake_changes_route() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_codex_test_websocket_with(stream, |_, response| {
            for cookie in ["__cflb=cflb-b; Path=/", "__oailb=oailb-b; Path=/"] {
                response
                    .headers_mut()
                    .append("set-cookie", cookie.parse().unwrap());
            }
        })
        .await;
        // 拒绝发生在发送业务正文之前；关闭可能表现为 Close、EOF 或 Windows TCP reset。
        loop {
            match timeout(Duration::from_secs(5), ws.next())
                .await
                .expect("connection discarded")
            {
                Some(Ok(Message::Text(_))) => panic!("ticket was sent to the wrong route"),
                Some(Ok(Message::Ping(payload))) => {
                    let _ = ws.send(Message::Pong(payload)).await;
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            }
        }
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::new(Duration::from_secs(60))));
    let mut request = pooled_websocket_request("mint-handshake-route");
    request.minted_turn_state_route = Some(fingerprint("a"));
    let mut context = request_context("mint-handshake", Some("acct"));
    context.cookie_header = Some("__cflb=cflb-a; __oailb=oailb-a");
    context.turn_state = Some("synthetic-mint-ticket");
    let result = timeout(
        Duration::from_secs(5),
        backend.create_response(&request, context),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    server.await.unwrap();
}

#[tokio::test]
async fn matching_mint_route_reuses_its_websocket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_codex_test_websocket(stream).await;
        for round in 0..2 {
            assert!(matches!(
                ws.next().await.unwrap().unwrap(),
                Message::Text(_)
            ));
            ws.send(Message::Text(
                completed_websocket_response(&format!("resp_mint_{round}"), 1, 1).into(),
            ))
            .await
            .unwrap();
        }
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::new(Duration::from_secs(60))));
    let mut request = pooled_websocket_request("mint-same-route");
    request.minted_turn_state_route = Some(fingerprint("a"));
    for round in 0..2 {
        let mut context = request_context("mint-reuse", Some("acct"));
        context.cookie_header = Some("__cflb=cflb-a; __oailb=oailb-a");
        context.turn_state = Some("synthetic-mint-ticket");
        let result = timeout(
            Duration::from_secs(5),
            backend.create_response(&request, context),
        )
        .await
        .unwrap()
        .unwrap();
        if round > 0 {
            assert!(
                result
                    .websocket_pool_decision
                    .is_some_and(WebSocketPoolDecision::is_reuse)
            );
        }
    }
    server.await.unwrap();
}

#[tokio::test]
async fn new_mint_route_does_not_adopt_the_previous_route_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut held = Vec::new();
        for label in ["a", "b"] {
            let (stream, _) = timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut ws = accept_codex_test_websocket_with(stream, |request, _| {
                assert!(
                    request.headers()["cookie"]
                        .to_str()
                        .unwrap()
                        .contains(&format!("cflb-{label}"))
                );
            })
            .await;
            assert!(matches!(
                ws.next().await.unwrap().unwrap(),
                Message::Text(_)
            ));
            ws.send(Message::Text(
                completed_websocket_response(&format!("resp_route_{label}"), 1, 1).into(),
            ))
            .await
            .unwrap();
            held.push(ws);
        }
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::new(Duration::from_secs(60))));
    let mut request = pooled_websocket_request("mint-changed-route");
    request.set_previous_response_id(None);
    for label in ["a", "b"] {
        request.minted_turn_state_route = Some(fingerprint(label));
        let cookie = format!("__cflb=cflb-{label}; __oailb=oailb-{label}");
        let mut context = request_context("mint-new-route", Some("acct"));
        context.cookie_header = Some(&cookie);
        context.turn_state = Some("synthetic-mint-ticket");
        let result = timeout(
            Duration::from_secs(5),
            backend.create_response(&request, context),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            !result
                .websocket_pool_decision
                .is_some_and(WebSocketPoolDecision::is_reuse)
        );
    }
    server.await.unwrap();
}

#[tokio::test]
async fn connection_local_continuation_cannot_send_a_mint_for_another_route() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_codex_test_websocket(stream).await;
        assert!(matches!(
            ws.next().await.unwrap().unwrap(),
            Message::Text(_)
        ));
        ws.send(Message::Text(
            completed_websocket_response("resp_route_a", 1, 1).into(),
        ))
        .await
        .unwrap();
        match timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("old connection discarded")
        {
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {}
            other => panic!("continuation payload reached the old route: {other:?}"),
        }
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::new(Duration::from_secs(60))));
    let mut request = pooled_websocket_request("mint-route-continuation");
    request.minted_turn_state_route = Some(fingerprint("a"));
    let mut context = request_context("mint-route-a", Some("acct"));
    context.cookie_header = Some("__cflb=cflb-a; __oailb=oailb-a");
    backend.create_response(&request, context).await.unwrap();
    request.set_previous_response_id(Some("resp_route_a".to_owned()));
    request.previous_response_scope = Some(PreviousResponseScope::ConnectionLocal);
    request.minted_turn_state_route = Some(fingerprint("b"));
    let mut context = request_context("mint-route-b", Some("acct"));
    context.cookie_header = Some("__cflb=cflb-b; __oailb=oailb-b");
    context.turn_state = Some("synthetic-new-route-ticket");
    assert!(
        timeout(
            Duration::from_secs(5),
            backend.create_response(&request, context)
        )
        .await
        .unwrap()
        .is_err()
    );
    server.await.unwrap();
}
