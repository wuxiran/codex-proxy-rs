//! 实际 worker、代理隧道、连接池及业务 Provider 的完整预热链路。

use crate::{
    admin::{
        TestOAuthPending, initialized_attempt_context, initialized_provider_request,
        provider_ports_with, valid_config,
    },
    support::{MemoryAccountStore, profile, secret},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use gateway_core::{
    account::ProviderAccountId,
    lifecycle::CancellationToken,
    operation::{GenerateRequest, Operation, ProtocolPayload},
    task::{WorkerContribution, WorkerRunnable},
};
use provider_openai::credential::ImportCodexOAuthCredential;
use serde_json::{Map, Value, json};
use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    time::timeout,
};
use tokio_tungstenite::tungstenite::Message;

const ACCOUNT: &str = "acct_warm_proxy";
const MODEL: &str = "gpt-5.4";

#[tokio::test]
async fn warm_pool_reprobes_at_capacity_even_when_target_is_larger_than_capacity() {
    reprobe_case(ReprobeCase::Full).await;
}

#[tokio::test]
async fn warm_pool_reprobes_while_a_missing_slot_is_cooling_down() {
    reprobe_case(ReprobeCase::Cooling).await;
}

async fn advance_warm_clock(seconds: u64) {
    for _ in 0..seconds {
        // 分步推进，让本地 TCP 与 ping/pong 有机会处理，避免把网络失活误当调度错误。
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::time::resume();
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

async fn wait_warm_report(bundle: &provider_openai::ProviderBundle, verdict: &str, opened: bool) {
    timeout(Duration::from_secs(5), async {
        loop {
            let view = bundle
                .admin_provider()
                .account_configuration(&ProviderAccountId::new(ACCOUNT).unwrap())
                .await
                .unwrap()
                .unwrap();
            let warm = &view.expose_to_provider().expose_to_provider()["warmPool"];
            if warm["last"]["verdict"] == verdict && warm["last"]["opened"] == opened {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn frequent_reprobes_do_not_starve_refilling_a_missing_slot() {
    reprobe_case(ReprobeCase::Frequent).await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReprobeCase {
    Full,
    Cooling,
    Frequent,
}

async fn reprobe_case(case: ReprobeCase) {
    let cooling_slot = case == ReprobeCase::Cooling;
    let opens_second = case != ReprobeCase::Full;
    let (second_tx, second_rx) = oneshot::channel();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (reprobe_tx, reprobe_rx) = oneshot::channel();
    let (stop_tx, mut stop_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = crate::transport::accept_codex_test_websocket(stream).await;
        let extra_slot = tokio::spawn(async move {
            if opens_second {
                let (stream, _) = listener.accept().await.unwrap();
                let mut failed = crate::transport::accept_codex_test_websocket(stream).await;
                failed.next().await.unwrap().unwrap();
                failed
                    .send(Message::Text(
                        json!({"type":"response.output_text.delta","delta": if cooling_slot { "29" } else { "21" }})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                failed
                    .send(Message::Text(completed("resp_bad_slot").into()))
                    .await
                    .unwrap();
                let _ = second_tx.send(());
                while let Some(Ok(message)) = failed.next().await {
                    match message {
                        Message::Close(_) => break,
                        Message::Ping(data) => {
                            failed.send(Message::Pong(data)).await.unwrap();
                        }
                        _ => {}
                    }
                }
            } else {
                // 满池复探不能通过偷偷新建 TCP 连接完成。
                assert!(
                    timeout(Duration::from_secs(5), listener.accept())
                        .await
                        .is_err()
                );
            }
        });
        let mut reprobe_tx = Some(reprobe_tx);
        let mut requests = 0;
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                next = ws.next() => match next {
                    Some(Ok(Message::Text(_))) => {
                        requests += 1;
                        ws.send(Message::Text(json!({"type":"response.output_text.delta","delta":"21"}).to_string().into())).await.unwrap();
                        ws.send(Message::Text(completed(&format!("resp_probe_{requests}")).into())).await.unwrap();
                        if requests == 2 { reprobe_tx.take().unwrap().send(()).unwrap(); }
                    },
                    Some(Ok(Message::Ping(data))) => { ws.send(Message::Pong(data)).await.unwrap(); },
                    other => panic!("verified connection unexpectedly closed: {other:?}"),
                }
            }
        }
        if cooling_slot {
            extra_slot.await.unwrap();
        } else {
            extra_slot.abort();
        }
    });
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: ACCOUNT.to_owned(),
            name: "reprobe fixture".to_owned(),
            secret: secret("synthetic-reprobe-access"),
            verified_account: profile("synthetic-reprobe-user"),
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    store.set_turn_state_pin(ACCOUNT, true);
    let mut config = valid_config();
    config.config.api.base_url = format!("http://{addr}");
    let mut bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(store, Arc::new(TestOAuthPending::default())),
    )
    .await
    .unwrap();
    let service = bundle.turn_state_service();
    let mut settings = service.settings();
    settings.warm_pool.models = vec![MODEL.to_owned()];
    settings.warm_pool.connections_per_account = 2;
    settings.warm_pool.max_total_connections = if opens_second { 2 } else { 1 };
    settings.warm_pool.reprobe_seconds = if cooling_slot { 60 } else { 15 };
    settings.warm_pool.probe_retries = 0;
    settings.warm_pool.cooldown_seconds = 600;
    settings.warm_pool.require_verified = false;
    service.update_settings(settings).unwrap();
    let task = bundle
        .take_worker_contributions()
        .into_iter()
        .find_map(|contribution| {
            if let WorkerContribution::Registration(registration) = contribution
                && registration.id.owner() == "openai-ws-warm-pool"
                && let WorkerRunnable::Daemon { task, .. } = registration.runnable
            {
                Some(task)
            } else {
                None
            }
        })
        .unwrap();
    let cancel = CancellationToken::new();
    let worker_cancel = cancel.clone();
    let worker = tokio::spawn(async move { task.run(worker_cancel).await });
    wait_warm_report(&bundle, "verified", true).await;
    let snapshot = service.pool_snapshot().await.unwrap();
    let account = snapshot
        .accounts
        .iter()
        .find(|row| row.account_id == ACCOUNT)
        .unwrap();
    assert!(account.participating);
    assert_eq!(
        account
            .connections
            .iter()
            .filter(|row| row.available && row.verification == "fresh")
            .count(),
        1
    );
    assert!(
        account
            .connections
            .iter()
            .any(|row| row.model == MODEL && row.verified_at_ms.is_some())
    );
    assert_eq!(snapshot.totals.verified_connections, 1);
    advance_warm_clock(21).await;
    if cooling_slot {
        wait_warm_report(&bundle, "degraded", true).await;
        advance_warm_clock(60).await;
    }
    timeout(Duration::from_secs(3), reprobe_rx)
        .await
        .unwrap()
        .unwrap();
    wait_warm_report(&bundle, "verified", false).await;
    if case == ReprobeCase::Frequent {
        advance_warm_clock(20).await;
        timeout(Duration::from_secs(3), second_rx)
            .await
            .unwrap()
            .unwrap();
        wait_warm_report(&bundle, "verified", true).await;
    }

    cancel.cancel();
    worker.await.unwrap().unwrap();
    stop_tx.send(()).unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn rejected_warm_ticket_must_not_publish_route() {
    publication_side_effects(PublicationCase::RejectedTicket).await;
}

#[tokio::test]
async fn observation_warm_must_not_publish_route_or_ticket() {
    publication_side_effects(PublicationCase::Observation).await;
}

#[tokio::test]
async fn accepted_warm_can_publish_route_and_ticket() {
    publication_side_effects(PublicationCase::Accepted).await;
}

#[tokio::test]
async fn warm_ticket_storage_failure_restores_the_previous_route() {
    publication_side_effects(PublicationCase::StorageFailure).await;
}

#[tokio::test]
async fn warm_publication_rollback_preserves_a_concurrent_route_update() {
    publication_side_effects(PublicationCase::ConcurrentRouteChange).await;
}

#[derive(Clone, Copy)]
enum PublicationCase {
    RejectedTicket,
    Observation,
    Accepted,
    StorageFailure,
    ConcurrentRouteChange,
}

async fn publication_side_effects(case: PublicationCase) {
    let reject_ticket = matches!(case, PublicationCase::RejectedTicket);
    let business_reuse = !matches!(case, PublicationCase::Observation);
    let fails = matches!(
        case,
        PublicationCase::RejectedTicket
            | PublicationCase::StorageFailure
            | PublicationCase::ConcurrentRouteChange
    );
    let expected = match case {
        PublicationCase::Accepted => (1, 2),
        PublicationCase::StorageFailure | PublicationCase::ConcurrentRouteChange => (0, 2),
        _ => (0, 0),
    };
    use gateway_core::account::ProviderAccountStore as _;
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy.local_addr().unwrap();
    let (stop_tx, mut stop_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = proxy.accept().await.unwrap();
        let stream = accept_proxy_tunnel(stream).await;
        let oailb = format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(json!({"host":"chat.gateway.unified-76.api.openai.com","exp":Utc::now().timestamp()+3600}).to_string()));
        let mut ws =
            crate::transport::accept_codex_test_websocket_with(stream, move |_, response| {
                response
                    .headers_mut()
                    .append("set-cookie", "__cflb=review-route; Path=/".parse().unwrap());
                response.headers_mut().append(
                    "set-cookie",
                    format!("__oailb={oailb}; Path=/").parse().unwrap(),
                );
                response.headers_mut().insert(
                    "x-codex-turn-state",
                    "r".repeat(if reject_ticket { 300 } else { 780 })
                        .parse()
                        .unwrap(),
                );
            })
            .await;
        ws.next().await.unwrap().unwrap();
        ws.send(Message::Text(
            json!({"type":"response.output_text.delta","delta":"21"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        ws.send(Message::Text(completed("resp_rejected_ticket").into()))
            .await
            .unwrap();
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                next = ws.next() => match next {
                    Some(Ok(Message::Ping(data))) => { ws.send(Message::Pong(data)).await.unwrap(); },
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    });
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: ACCOUNT.to_owned(),
            name: "rejected ticket fixture".to_owned(),
            secret: secret("synthetic-rejected-access"),
            verified_account: profile("synthetic-rejected-user"),
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    store.set_turn_state_pin(ACCOUNT, true);
    let mut config = valid_config();
    config.config.api.base_url = "http://127.0.0.1:9".to_owned();
    let mut bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(store.clone(), Arc::new(TestOAuthPending::default())),
    )
    .await
    .unwrap();
    let account_id = ProviderAccountId::new(ACCOUNT).unwrap();
    if matches!(
        case,
        PublicationCase::StorageFailure | PublicationCase::ConcurrentRouteChange
    ) {
        // 先保留一对旧路由，再用文件占据账号桶目录，确定性制造票据落盘失败。
        let account = store.get_account(&account_id).await.unwrap().unwrap();
        let mut data = store
            .repository()
            .load_complete_data(&account)
            .await
            .unwrap();
        for (name, value) in [("__cflb", "old-cflb"), ("__oailb", "old-oailb")] {
            data.cookies_mut()
                .unwrap()
                .push(provider_openai::credential::CodexCookie {
                    name: name.to_owned(),
                    value: value.to_owned(),
                    domain: "127.0.0.1".to_owned(),
                    path: "/".to_owned(),
                    host_only: true,
                    secure: false,
                    expires_at: None,
                });
        }
        store
            .repository()
            .compare_and_swap_data(&account, data)
            .await
            .unwrap();
        std::fs::write(
            config
                .config
                .turn_state_data_dir()
                .join("buckets")
                .join(ACCOUNT),
            b"occupied",
        )
        .unwrap();
    }
    if matches!(case, PublicationCase::ConcurrentRouteChange) {
        let repository = store.repository();
        store.on_credential_load(Arc::new(move |store, id, _| {
            let repository = repository.clone();
            Box::pin(async move {
                let loaded = store.load_current_credential(id).await?;
                let mut data = provider_openai::credential::CodexCredentialCodec::decode_complete(
                    &loaded.credential,
                )
                .unwrap();
                if data
                    .cookies()
                    .iter()
                    .any(|cookie| cookie.name == "__cflb" && cookie.value == "review-route")
                {
                    // 回滚读取时模拟另一请求提交更新，旧发布者不得覆盖它。
                    for cookie in data.cookies_mut().unwrap() {
                        cookie.value = "newer-route".to_owned();
                    }
                    repository
                        .compare_and_swap_data(&loaded.account, data)
                        .await
                        .unwrap();
                }
                Ok(())
            })
        }));
    }
    let service = bundle.turn_state_service();
    let mut settings = service.settings();
    settings.template_lengths = vec![780];
    settings.cloud_mint.enabled = true;
    settings.cloud_mint.upstream_proxy_url = format!("http://{proxy_addr}");
    settings.warm_pool.enabled = true;
    settings.warm_pool.require_verified = false;
    settings.warm_pool.business_reuse = business_reuse;
    settings.warm_pool.models = vec![MODEL.to_owned()];
    settings.warm_pool.connections_per_account = 1;
    settings.warm_pool.probe_retries = 0;
    service.update_settings(settings).unwrap();
    let task = bundle
        .take_worker_contributions()
        .into_iter()
        .find_map(|contribution| {
            if let WorkerContribution::Registration(registration) = contribution
                && registration.id.owner() == "openai-ws-warm-pool"
                && let WorkerRunnable::Daemon { task, .. } = registration.runnable
            {
                Some(task)
            } else {
                None
            }
        })
        .unwrap();
    let cancel = CancellationToken::new();
    let worker_cancel = cancel.clone();
    let worker = tokio::spawn(async move { task.run(worker_cancel).await });
    let account_id = ProviderAccountId::new(ACCOUNT).unwrap();
    timeout(Duration::from_secs(8), async {
        loop {
            let view = bundle
                .admin_provider()
                .account_configuration(&account_id)
                .await
                .unwrap()
                .unwrap();
            let warm = &view.expose_to_provider().expose_to_provider()["warmPool"];
            if (fails && warm["last"]["error"] == "candidate_publication_failed")
                || (!fails && warm["last"]["verdict"] == "verified")
            {
                assert_eq!(warm["held"], if fails { 0 } else { 1 });
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    worker.await.unwrap().unwrap();
    let _ = stop_tx.send(());
    server.await.unwrap();
    let pin_count = service.buckets(SystemTime::now()).len();
    let account = store.get_account(&account_id).await.unwrap().unwrap();
    let data = store
        .repository()
        .load_complete_data(&account)
        .await
        .unwrap();
    let route_count = data
        .cookies()
        .iter()
        .filter(|cookie| cookie.name == "__cflb" || cookie.name == "__oailb")
        .count();
    if matches!(case, PublicationCase::StorageFailure) {
        assert!(
            data.cookies()
                .iter()
                .all(|cookie| cookie.value.starts_with("old-")),
            "rollback must restore the original cookie values"
        );
    }
    if matches!(case, PublicationCase::ConcurrentRouteChange) {
        assert!(
            data.cookies()
                .iter()
                .all(|cookie| cookie.value == "newer-route")
        );
    }
    assert_eq!(
        (pin_count, route_count),
        expected,
        "rejected or observation-only warm publication must preserve original pins and route"
    );
}

async fn accept_proxy_tunnel(mut stream: TcpStream) -> TcpStream {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        assert!(head.len() < 16384);
        head.push(stream.read_u8().await.unwrap());
    }
    assert!(
        head.starts_with(b"CONNECT "),
        "candidate must use the dedicated proxy"
    );
    stream
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .unwrap();
    stream
}

fn completed(id: &str) -> String {
    json!({"type":"response.completed","response":{"id":id,"object":"response","status":"completed","model":MODEL,
        "output":[],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}).to_string()
}

fn business_operation() -> Operation {
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!(MODEL)),
            ("input".to_owned(), json!("business request")),
            ("store".to_owned(), json!(false)),
            ("session_id".to_owned(), json!("warm-recovery-session")),
        ]),
    )
    .unwrap()
    .with_context(Map::from_iter([("use_websocket".to_owned(), json!(true))]));
    Operation::Generate(GenerateRequest::from_protocol_payload(payload))
}

#[tokio::test]
async fn warm_pool_checks_each_model_and_labels_unchecked_answers_as_ready() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (opened_tx, mut opened_rx) = tokio::sync::mpsc::channel(2);
    let (stop_tx, stop_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut held = Vec::new();
        for expected in [MODEL, "gpt-6-astra"] {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = crate::transport::accept_codex_test_websocket(stream).await;
            let request = ws.next().await.unwrap().unwrap().into_text().unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&request).unwrap()["model"],
                expected
            );
            ws.send(Message::Text(
                json!({"type":"response.output_text.delta","delta":"29"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let mut response: Value = serde_json::from_str(&completed("resp_unchecked")).unwrap();
            response["response"]["model"] = json!(expected);
            ws.send(Message::Text(response.to_string().into()))
                .await
                .unwrap();
            opened_tx.send(()).await.unwrap();
            held.push(ws);
        }
        stop_rx.await.unwrap();
    });
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: ACCOUNT.to_owned(),
            name: "model coverage fixture".to_owned(),
            secret: secret("synthetic-multi-access"),
            verified_account: profile("synthetic-multi-user"),
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    store.set_turn_state_pin(ACCOUNT, true);
    let mut config = valid_config();
    config.config.api.base_url = format!("http://{addr}");
    let mut bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(store, Arc::new(TestOAuthPending::default())),
    )
    .await
    .unwrap();
    let service = bundle.turn_state_service();
    let mut settings = service.settings();
    settings.warm_pool.models = vec![MODEL.to_owned(), "gpt-6-astra".to_owned()];
    settings.warm_pool.connections_per_account = 1;
    settings.warm_pool.probe = false;
    service.update_settings(settings).unwrap();
    let task = bundle
        .take_worker_contributions()
        .into_iter()
        .find_map(|contribution| {
            if let WorkerContribution::Registration(registration) = contribution
                && registration.id.owner() == "openai-ws-warm-pool"
                && let WorkerRunnable::Daemon { task, .. } = registration.runnable
            {
                Some(task)
            } else {
                None
            }
        })
        .unwrap();
    let cancel = CancellationToken::new();
    let worker_cancel = cancel.clone();
    let worker = tokio::spawn(async move { task.run(worker_cancel).await });
    let account_id = ProviderAccountId::new(ACCOUNT).unwrap();
    for count in 1..=2 {
        timeout(Duration::from_secs(5), opened_rx.recv())
            .await
            .unwrap()
            .unwrap();
        timeout(Duration::from_secs(5), async {
            loop {
                let view = bundle
                    .admin_provider()
                    .account_configuration(&account_id)
                    .await
                    .unwrap()
                    .unwrap();
                let warm = &view.expose_to_provider().expose_to_provider()["warmPool"];
                if warm["held"] == count {
                    assert_eq!(warm["last"]["verdict"], "ready");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        if count == 1 {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(20)).await;
            tokio::time::resume();
        }
    }
    cancel.cancel();
    worker.await.unwrap().unwrap();
    stop_tx.send(()).unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn dynamic_warm_rejects_bad_candidate_then_publishes_and_reuses_the_same_socket() {
    dynamic_candidate_case(false, true).await;
}

#[tokio::test]
async fn dynamic_warm_retries_a_transient_proxy_disconnect() {
    dynamic_candidate_case(true, true).await;
}

#[tokio::test]
async fn dynamic_warm_reuses_verified_socket_with_strict_mode_disabled() {
    dynamic_candidate_case(false, false).await;
}

async fn dynamic_candidate_case(reset_first: bool, require_verified: bool) {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy.local_addr().unwrap();
    // 常规出口只保留监听，不接收请求；业务若没有复用候选会使测试失败。
    let direct = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let direct_addr = direct.local_addr().unwrap();
    let (pending_tx, pending_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut pending_tx = Some(pending_tx);
        let mut finish_rx = Some(finish_rx);
        for (attempt, answer) in [(0, "29"), (1, "21")] {
            let (stream, _) = timeout(Duration::from_secs(10), proxy.accept())
                .await
                .unwrap()
                .unwrap();
            let stream = accept_proxy_tunnel(stream).await;
            if reset_first && attempt == 0 {
                drop(stream);
                continue;
            }
            let oailb = format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(json!({"host":format!("chat.gateway.unified-{}.api.openai.com", 75+attempt),"exp":Utc::now().timestamp()+3600}).to_string()));
            let ticket = if attempt == 0 { "a" } else { "b" }.repeat(780);
            let mut ws = crate::transport::accept_codex_test_websocket_with(
                stream,
                move |request, response| {
                    assert!(
                        !request.headers().contains_key("cookie"),
                        "each new candidate must start without the failed route"
                    );
                    response.headers_mut().append(
                        "set-cookie",
                        format!("__cflb=cflb-{attempt}; Path=/").parse().unwrap(),
                    );
                    response.headers_mut().append(
                        "set-cookie",
                        format!("__oailb={oailb}; Path=/").parse().unwrap(),
                    );
                    response
                        .headers_mut()
                        .insert("x-codex-turn-state", ticket.parse().unwrap());
                },
            )
            .await;
            let request = ws.next().await.unwrap().unwrap().into_text().unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&request).unwrap()["model"],
                MODEL
            );
            ws.send(Message::Text(
                json!({"type":"response.output_text.delta","delta":answer})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            if attempt == 1 {
                pending_tx.take().unwrap().send(()).unwrap();
                finish_rx.take().unwrap().await.unwrap();
            }
            ws.send(Message::Text(
                completed(&format!("resp_warm_{attempt}")).into(),
            ))
            .await
            .unwrap();
            if attempt == 0 {
                // 等待失败连接被关闭，第二个候选必须是新 TCP 隧道。
                while let Some(Ok(message)) = ws.next().await {
                    if matches!(message, Message::Close(_)) {
                        break;
                    }
                    assert!(!matches!(message, Message::Text(_)));
                }
            } else {
                let business = timeout(Duration::from_secs(10), ws.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap()
                    .into_text()
                    .unwrap();
                let business: Value = serde_json::from_str(&business).unwrap();
                assert_eq!(business["model"], MODEL);
                assert!(business.to_string().contains("business request"));
                assert!(!business.to_string().contains("在一个黑色的袋子"));
                ws.send(Message::Text(completed("resp_verified_business").into()))
                    .await
                    .unwrap();
            }
        }
    });
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: ACCOUNT.to_owned(),
            name: "warm fixture".to_owned(),
            secret: secret("synthetic-warm-access"),
            verified_account: profile("synthetic-warm-user"),
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    store.set_turn_state_pin(ACCOUNT, true);
    let mut config = valid_config();
    config.config.api.base_url = format!("http://{direct_addr}");
    config.config.ws_pool.max_connecting = 1;
    let mut bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(store, Arc::new(TestOAuthPending::default())),
    )
    .await
    .unwrap();
    let service = bundle.turn_state_service();
    let mut settings = service.settings();
    settings.cloud_mint.enabled = true;
    settings.cloud_mint.upstream_proxy_url = format!("http://{proxy_addr}");
    settings.warm_pool.enabled = true;
    settings.warm_pool.require_verified = require_verified;
    settings.warm_pool.models = vec![MODEL.to_owned()];
    settings.warm_pool.connections_per_account = 1;
    settings.warm_pool.probe_retries = 1;
    service.update_settings(settings).unwrap();
    let provider = bundle.core_provider();
    if require_verified {
        let mut cold = Arc::clone(&provider)
            .execute(
                initialized_provider_request(business_operation(), ACCOUNT),
                initialized_attempt_context("req_warm_cold", ACCOUNT),
            )
            .await
            .unwrap();
        let cold_error = timeout(Duration::from_secs(4), async {
            loop {
                match cold.next().await {
                    Some(Err(error)) => break error,
                    Some(Ok(_)) => {}
                    None => panic!("a cold verified-only request must not succeed"),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            cold_error.kind(),
            gateway_core::error::ProviderErrorKind::AccountCapacityUnavailable
        );
        assert_eq!(
            cold_error.send_state(),
            gateway_core::upstream::UpstreamSendState::NotSent
        );
        assert!(
            cold_error.upstream_status().is_none(),
            "local readiness is not an upstream response"
        );
        assert!(!provider_openai::openai_failure_affects_account_score(
            &cold_error
        ));
        assert!(cold_error.pre_delivery_retry().is_none());
        drop(cold);
    }
    let task = bundle
        .take_worker_contributions()
        .into_iter()
        .find_map(|contribution| {
            if let WorkerContribution::Registration(registration) = contribution
                && registration.id.owner() == "openai-ws-warm-pool"
                && let WorkerRunnable::Daemon { task, .. } = registration.runnable
            {
                Some(task)
            } else {
                None
            }
        })
        .unwrap();
    let cancel = CancellationToken::new();
    let worker_cancel = cancel.clone();
    let worker = tokio::spawn(async move { task.run(worker_cancel).await });
    timeout(Duration::from_secs(10), pending_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(
        service.buckets(SystemTime::now()).is_empty(),
        "pending and failed candidates must not publish tickets"
    );
    finish_tx.send(()).unwrap();
    let account_id = ProviderAccountId::new(ACCOUNT).unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            let view = bundle
                .admin_provider()
                .account_configuration(&account_id)
                .await
                .unwrap()
                .unwrap();
            if view.expose_to_provider().expose_to_provider()["warmPool"]["last"]["verdict"]
                == "verified"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(service.buckets(SystemTime::now()).len(), 1);
    let operation = business_operation();
    let mut stream = provider
        .execute(
            initialized_provider_request(operation, ACCOUNT),
            initialized_attempt_context("req_warm_business", ACCOUNT),
        )
        .await
        .unwrap();
    while let Some(event) = timeout(Duration::from_secs(8), stream.next())
        .await
        .unwrap()
    {
        event.unwrap();
    }
    cancel.cancel();
    worker.await.unwrap().unwrap();
    server.await.unwrap();
    assert!(
        timeout(Duration::from_millis(30), direct.accept())
            .await
            .is_err(),
        "normal egress must not receive a fresh business connection"
    );
}
