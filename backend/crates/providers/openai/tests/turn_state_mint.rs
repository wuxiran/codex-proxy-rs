//! fork：云打票只认显式配置的专用出口——缺失/非法不能借业务出口，专用出口不通也不回落直连。

use std::time::Duration;

use provider_openai::{MintError, mint_http_client, mint_proxy};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[test]
fn a_missing_or_invalid_mint_proxy_cannot_select_a_business_exit() {
    assert_eq!(mint_proxy("").unwrap_err(), MintError::ProxyRequired);
    assert_eq!(
        mint_proxy("file:///tmp/proxy").unwrap_err(),
        MintError::InvalidProxy
    );
    assert_eq!(
        mint_proxy("http://user:pass@localhost:0").unwrap_err(),
        MintError::InvalidProxy
    );
    assert!(mint_proxy("socks5h://example-user:example-password@proxy.example:1080").is_ok());
}

#[tokio::test]
async fn consecutive_mint_attempts_open_new_connections_to_the_dedicated_proxy() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = mint_proxy(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
        let mut connections = Vec::new();
        for _ in 0..2 {
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut request = Vec::new();
            let mut part = [0; 1024];
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let n = socket.read(&mut part).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&part[..n]);
            }
            assert!(request.starts_with(b"POST http://mint.invalid/codex/responses HTTP/1.1\r\n"));
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nOK",
                )
                .await
                .unwrap();
            connections.push(socket);
        }
        connections.len()
    });
    for _ in 0..2 {
        let response = mint_http_client(&proxy)
            .unwrap()
            .post("http://mint.invalid/codex/responses")
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "OK");
    }
    assert_eq!(server.await.unwrap(), 2);
}

#[tokio::test]
async fn a_failed_dedicated_proxy_does_not_fall_back_to_direct() {
    let upstream = wiremock::MockServer::start().await;
    let unavailable = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = mint_proxy(&format!("http://{}", unavailable.local_addr().unwrap())).unwrap();
    drop(unavailable);
    let result = mint_http_client(&proxy)
        .unwrap()
        .post(upstream.uri())
        .timeout(Duration::from_secs(2))
        .send()
        .await;
    assert!(result.is_err());
    assert!(upstream.received_requests().await.unwrap().is_empty());
}
