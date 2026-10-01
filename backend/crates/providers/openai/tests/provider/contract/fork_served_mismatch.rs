//! fork：上游换模型时，这一轮的 turn-state 不入 pin，正在复用的 pin 失效。

use super::*;

fn sse_for(model: &str) -> String {
    format!(
        concat!(
            "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_served\",\"model\":\"{model}\"}}}}\n\n",
            "event: response.completed\n",
            "data: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"resp_served\",\"model\":\"{model}\",\"status\":\"completed\",\"output\":[],\"usage\":{{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}}}}\n\n",
        ),
        model = model
    )
}

#[tokio::test]
async fn served_model_mismatch_rejects_the_candidate_and_drops_the_reused_pin() {
    let store = Arc::new(MemoryAccountStore::default());
    let account = "acct_provider_contract";
    create_account(&store, account).await;
    store.set_turn_state_pin(account, true);
    let server = MockServer::start().await;
    let provider = provider_with_base_url(&store, server.uri());
    let good = "a".repeat(780);
    let recovered = "c".repeat(780);
    // (上游签发的 state, 上游声明的模型)
    let rounds = [
        // 换了模型：这张票不能成为模板。
        ("x".repeat(780), "gpt-5.6-luna"),
        (good.clone(), "gpt-5.4"),
        // 复用 good 的这一轮被换了模型：good 失效，新签发的也不收。
        ("b".repeat(780), "gpt-5.6-luna"),
        (recovered.clone(), "gpt-5.4"),
        ("d".repeat(780), "gpt-5.4"),
    ];
    for (index, (response_state, served)) in rounds.into_iter().enumerate() {
        let mock = Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .insert_header("x-codex-turn-state", response_state)
                    .set_body_string(sse_for(served)),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let mut stream = Arc::clone(&provider)
            .execute(
                planned_request("openai", http_generate_operation()),
                context(&format!("req_served_{index}"), CancellationToken::new()),
            )
            .await
            .unwrap();
        let mut recorded = None;
        while let Some(event) = stream.next().await {
            let event = event.unwrap();
            if let Some(metadata) = event
                .response_observation()
                .and_then(|observation| observation.provider_metadata())
            {
                let metadata: Value = serde_json::from_str(metadata.as_json()).unwrap();
                recorded = metadata
                    .get("servedMatch")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
        }
        // 落库的 Provider 元数据带着对照结果，按节点统计换模型率靠它。
        let expected = if served == "gpt-5.4" {
            "match"
        } else {
            "mismatch"
        };
        assert_eq!(recorded.as_deref(), Some(expected), "round {index}");
        drop(mock);
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 5);
    let sent = |index: usize| captured_header_values(&requests[index], "x-codex-turn-state");
    assert!(sent(0).is_empty());
    assert!(sent(1).is_empty(), "a mismatched turn must not be pinned");
    assert_eq!(sent(2), vec![good.as_bytes().to_vec()]);
    assert!(
        sent(3).is_empty(),
        "the pin reused by a mismatched turn must be dropped"
    );
    assert_eq!(sent(4), vec![recovered.as_bytes().to_vec()]);
}

#[tokio::test]
async fn a_matching_terminal_in_the_same_chunk_cannot_hide_an_earlier_mismatch() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    store.set_turn_state_pin("acct_provider_contract", true);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .insert_header("x-codex-turn-state", "x".repeat(780))
            .set_body_string(concat!(
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_same_chunk\",\"model\":\"gpt-other\"}}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_same_chunk\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n"
            )))
        .expect(2).mount(&server).await;
    let provider = provider_with_base_url(&store, server.uri());
    for index in 0..2 {
        let mut stream = Arc::clone(&provider)
            .execute(
                planned_request("openai", http_generate_operation()),
                context(&format!("req_same_chunk_{index}"), CancellationToken::new()),
            )
            .await
            .unwrap();
        let mut mismatch_seen = false;
        while let Some(event) = stream.next().await {
            let event = event.unwrap();
            if let Some(metadata) = event
                .response_observation()
                .and_then(|observation| observation.provider_metadata())
            {
                let metadata: Value = serde_json::from_str(metadata.as_json()).unwrap();
                mismatch_seen |= metadata["servedMatch"] == "mismatch";
            }
        }
        assert!(mismatch_seen);
    }
    for request in server.received_requests().await.unwrap() {
        assert!(captured_header_values(&request, "x-codex-turn-state").is_empty());
    }
}

#[tokio::test]
async fn a_matching_sse_model_header_cannot_hide_a_mismatched_http_header() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    let body = sse_for("gpt-5.4").replace(
        "\"model\":\"gpt-5.4\"",
        "\"model\":\"gpt-5.4\",\"headers\":{\"openai-model\":\"gpt-5.4\"}",
    );
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("openai-model", "gpt-other")
                .set_body_string(body),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    let mut stream = provider
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_header_mismatch", CancellationToken::new()),
        )
        .await
        .unwrap();
    let mut mismatch = false;
    while let Some(event) = stream.next().await {
        if let Some(metadata) = event
            .unwrap()
            .response_observation()
            .and_then(|observation| observation.provider_metadata())
        {
            let metadata: Value = serde_json::from_str(metadata.as_json()).unwrap();
            mismatch |= metadata["servedMatch"] == "mismatch";
        }
    }
    assert!(
        mismatch,
        "the HTTP opening declaration must remain part of the verdict"
    );
}

#[tokio::test]
async fn every_model_header_declaration_participates_in_the_verdict() {
    for location in ["http", "event_alias", "event_array"] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let server = MockServer::start().await;
        let mut body = sse_for("gpt-5.4");
        let mut response =
            ResponseTemplate::new(200).insert_header("content-type", "text/event-stream");
        if location == "http" {
            response = response
                .insert_header("openai-model", "gpt-other")
                .insert_header("x-openai-model", "GPT-5.4");
        } else {
            let headers = if location == "event_alias" {
                json!({"openai-model":"gpt-5.4", "x-openai-model":"gpt-other"})
            } else {
                json!({"openai-model":["gpt-5.4", "gpt-other"]})
            };
            body = body.replace(
                "\"model\":\"gpt-5.4\"",
                &format!("\"model\":\"gpt-5.4\",\"headers\":{headers}"),
            );
        }
        Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .respond_with(response.set_body_string(body))
            .mount(&server)
            .await;
        let provider = provider_with_base_url(&store, server.uri());
        let mut stream = provider
            .execute(
                planned_request("openai", http_generate_operation()),
                context("req_all_model_headers", CancellationToken::new()),
            )
            .await
            .unwrap();
        let mut mismatch = false;
        while let Some(event) = stream.next().await {
            if let Some(metadata) = event
                .unwrap()
                .response_observation()
                .and_then(|observation| observation.provider_metadata())
            {
                let metadata: Value = serde_json::from_str(metadata.as_json()).unwrap();
                mismatch |= metadata["servedMatch"] == "mismatch";
            }
        }
        assert!(mismatch, "{location}");
    }
}
