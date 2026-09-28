//! fork：请求归属（账号 / 出口 / 响应模型 / 计价模型）追踪与模型计价观测测试。
//!
//! 与上游 `coordinator` 测试共用夹具（`pub(super)` 暴露），上游文件只保留 `Script::AttributedStream`
//! 变体与 `billing` 字段两处 fork 标记。

use std::time::{Duration, SystemTime};

use futures::executor::block_on;
use gateway_core::account::{OutboundProxy, ProviderAccountId};
use gateway_core::engine::EngineError;
use gateway_core::engine::provider::{ProviderCallMetadata, ProviderRequest};
use gateway_core::error::{ProviderError, ProviderErrorKind};
use gateway_core::event::{GatewayEvent, ResponseMeta};
use gateway_core::lifecycle::CancellationToken;
use gateway_core::metering::{CalculatedCost, CostSource, ProviderReportedCost};
use gateway_core::upstream::{UpstreamSendState, UpstreamTransport};
use serde_json::{Value, json};

use super::coordinator::{
    FinalState, Script, complete_stream, coordinator, generate_operation, model_request, plan,
};

/// `Script::AttributedStream` 的调用元数据：与普通流相同，额外带上账号出口（`None` 表示直连）。
pub(super) fn attributed_metadata(
    request: &ProviderRequest,
    account_id: &'static str,
    proxy: Option<&'static str>,
) -> ProviderCallMetadata {
    let proxy = proxy.map(|url| OutboundProxy::parse(url).unwrap());
    let candidate = request.candidate();
    ProviderCallMetadata::new(
        candidate.provider().clone(),
        candidate.upstream_model().cloned().unwrap(),
        ProviderAccountId::new(account_id).unwrap(),
        UpstreamTransport::new("http_sse").unwrap(),
    )
    .with_outbound_proxy(proxy.as_ref())
}

fn finalize_single_stream(items: Vec<Result<GatewayEvent, ProviderError>>) -> FinalState {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, store, _) = coordinator(vec![Script::Stream {
        account_id: "acct_one",
        items,
    }]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    block_on(session.collect_uncommitted()).expect("collect response");
    block_on(session.commit_downstream(Some(200))).expect("commit response");
    let state = store.state.lock().expect("store lock");
    state.finalizations[0].clone()
}

#[test]
fn calculated_cost_is_also_recorded_as_model_billing() {
    let finalization = finalize_single_stream(vec![
        Ok(GatewayEvent::Started(ResponseMeta::new(
            "calculated-cost",
            "grok-4.5",
        ))),
        Ok(GatewayEvent::CalculatedCost(
            CalculatedCost::from_usd_ticks(123).expect("calculated cost"),
        )),
        Ok(GatewayEvent::Completed(ResponseMeta::new(
            "calculated-cost",
            "grok-4.5",
        ))),
    ]);
    assert_eq!(finalization.cost_source, CostSource::Calculated);
    assert_eq!(finalization.cost_ticks, Some(123));
    assert_eq!(
        finalization
            .billing
            .calculated_cost
            .unwrap()
            .amount()
            .scaled(),
        123
    );
}

#[test]
fn provider_reported_cost_keeps_last_calculated_billing_and_models() {
    let finalization = finalize_single_stream(vec![
        Ok(GatewayEvent::Started(ResponseMeta::new(
            "reported-cost",
            "gpt-5.6-luna",
        ))),
        Ok(GatewayEvent::CalculatedCost(
            CalculatedCost::from_usd_ticks(10).expect("first calculated cost"),
        )),
        Ok(GatewayEvent::ProviderCost(
            ProviderReportedCost::from_usd_ticks(25).expect("provider cost"),
        )),
        Ok(GatewayEvent::CalculatedCost(
            CalculatedCost::from_usd_ticks(999).expect("later calculated cost"),
        )),
        Ok(GatewayEvent::Completed(
            ResponseMeta::new("reported-cost", "gpt-5.6-luna")
                .with_billing_model(Some("gpt-5.6-luna".to_owned())),
        )),
    ]);
    assert_eq!(
        (finalization.cost_source, finalization.cost_ticks),
        (CostSource::ProviderReported, Some(25))
    );
    assert_eq!(
        finalization
            .billing
            .calculated_cost
            .unwrap()
            .amount()
            .scaled(),
        999
    );
    assert_eq!(
        finalization.billing.response_model.as_deref(),
        Some("gpt-5.6-luna")
    );
    assert_eq!(
        finalization.billing.billing_model.as_deref(),
        Some("gpt-5.6-luna")
    );
}

#[test]
fn discarded_attempt_billing_never_leaks_into_retry_result() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, store, _) = coordinator(vec![
        Script::Stream {
            account_id: "acct_first",
            items: vec![
                Ok(GatewayEvent::Started(ResponseMeta::new(
                    "discarded",
                    "gpt-5",
                ))),
                Ok(GatewayEvent::CalculatedCost(
                    CalculatedCost::from_usd_ticks(888).expect("discarded calculated cost"),
                )),
                Ok(GatewayEvent::ProviderCost(
                    ProviderReportedCost::from_usd_ticks(999).expect("discarded cost"),
                )),
                Err(
                    ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::Sent)
                        .with_replay_safe(),
                ),
            ],
        },
        Script::Stream {
            account_id: "acct_second",
            items: complete_stream(None),
        },
    ]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .expect("start execution");
    block_on(session.collect_uncommitted()).expect("retry succeeds");
    block_on(session.commit_downstream(Some(200))).expect("commit response");
    let state = store.state.lock().expect("store lock");
    assert!(state.finalizations[0].billing.calculated_cost.is_none());
    assert!(state.finalizations[0].billing.billing_model.is_none());
}

fn attribution_trace(finalization: &FinalState) -> Value {
    serde_json::from_str(finalization.diagnostic_trace_json.as_deref().unwrap()).unwrap()
}

#[test]
fn attribution_keeps_last_valid_route_when_retry_has_no_current_or_eligible_account() {
    let operation = generate_operation();
    let route_plan = plan(&operation);
    let (coordinator, store, provider) = coordinator(vec![
        Script::AttributedStream {
            account_id: "acct_first",
            proxy: Some("http://synthetic-user:synthetic-password@proxy.example:8080"),
            items: vec![Err(ProviderError::new(
                ProviderErrorKind::Unavailable,
                UpstreamSendState::Sent,
            )
            .with_replay_safe())],
        },
        Script::Error(ProviderError::new(
            ProviderErrorKind::NoEligibleAccount,
            UpstreamSendState::NotSent,
        )),
    ]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation,
        route_plan,
        None,
        None,
        CancellationToken::new(),
    ))
    .unwrap();
    assert!(block_on(session.collect_uncommitted()).is_err());
    assert!(session.is_finalized());
    assert_eq!(provider.contexts.lock().unwrap().len(), 2);
    let state = store.state.lock().unwrap();
    assert_eq!(state.intermediate_failures, 1);
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(state.finalizations[0].attempt_count, 1);
    let trace = attribution_trace(&state.finalizations[0]);
    assert_eq!(
        trace["request_attribution"],
        json!({
            "requested_model":"gpt-5", "route_model":"gpt-5", "response_model":null, "billing_model":null,
            "provider_account_id":"acct_first", "outbound_proxy_endpoint":"http://proxy.example:8080/"
        })
    );
    assert!(!trace.to_string().contains("synthetic"));
}

#[test]
fn attribution_preserves_independent_response_and_billing_models_and_both_costs() {
    for proxy in [
        None,
        Some("socks5h://synthetic-user:synthetic-secret@proxy.example:1080"),
    ] {
        let operation = generate_operation();
        let (coordinator, store, _) = coordinator(vec![Script::AttributedStream {
            account_id: "acct_one",
            proxy,
            items: vec![
                Ok(GatewayEvent::Started(ResponseMeta::new(
                    "resp_identity",
                    "gpt-6-astra",
                ))),
                Ok(GatewayEvent::CalculatedCost(
                    CalculatedCost::from_usd_ticks(100).unwrap(),
                )),
                Ok(GatewayEvent::ProviderCost(
                    ProviderReportedCost::from_usd_ticks(0).unwrap(),
                )),
                Ok(GatewayEvent::Completed(
                    ResponseMeta::new("resp_identity", "gpt-5.6-luna")
                        .with_billing_model(Some("gpt-5.6-sol".to_owned())),
                )),
            ],
        }]);
        let mut session = block_on(coordinator.start(
            model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
            operation.clone(),
            plan(&operation),
            None,
            None,
            CancellationToken::new(),
        ))
        .unwrap();
        block_on(session.collect_uncommitted()).unwrap();
        block_on(session.commit_downstream(Some(200))).unwrap();
        let state = store.state.lock().unwrap();
        let finalization = &state.finalizations[0];
        let trace = attribution_trace(finalization);
        let expected_proxy = if proxy.is_some() {
            "socks5h://proxy.example:1080"
        } else {
            "direct"
        };
        assert_eq!(
            trace["request_attribution"],
            json!({
                "requested_model":"gpt-5", "route_model":"gpt-5", "response_model":"gpt-5.6-luna", "billing_model":"gpt-5.6-sol",
                "provider_account_id":"acct_one", "outbound_proxy_endpoint":expected_proxy,
            })
        );
        assert_eq!(
            finalization.billing.response_model.as_deref(),
            Some("gpt-5.6-luna")
        );
        assert_eq!(
            finalization.billing.billing_model.as_deref(),
            Some("gpt-5.6-sol")
        );
        assert_eq!(
            finalization
                .billing
                .calculated_cost
                .unwrap()
                .amount()
                .scaled(),
            100
        );
        assert_eq!(finalization.cost_source, CostSource::ProviderReported);
        assert_eq!(finalization.cost_ticks, Some(0));
        assert!(!trace.to_string().contains("synthetic"));
    }
}

#[test]
fn attribution_retry_keeps_each_account_proxy_but_clears_discarded_billing() {
    let operation = generate_operation();
    let (coordinator, store, _) = coordinator(vec![
        Script::AttributedStream {
            account_id: "acct_first",
            proxy: Some("http://synthetic-user:synthetic-secret@proxy.example:8080"),
            items: vec![
                Ok(GatewayEvent::Started(
                    ResponseMeta::new("discarded", "gpt-5.6-luna")
                        .with_billing_model(Some("gpt-5.6-luna".to_owned())),
                )),
                Ok(GatewayEvent::CalculatedCost(
                    CalculatedCost::from_usd_ticks(100).unwrap(),
                )),
                Err(
                    ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::Sent)
                        .with_replay_safe(),
                ),
            ],
        },
        Script::AttributedStream {
            account_id: "acct_second",
            proxy: None,
            items: vec![
                Ok(GatewayEvent::Started(ResponseMeta::new(
                    "winner",
                    "gpt-6-astra",
                ))),
                Ok(GatewayEvent::ProviderCost(
                    ProviderReportedCost::from_usd_ticks(0).unwrap(),
                )),
                Ok(GatewayEvent::Completed(ResponseMeta::new(
                    "winner",
                    "gpt-6-astra",
                ))),
            ],
        },
    ]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation.clone(),
        plan(&operation),
        None,
        None,
        CancellationToken::new(),
    ))
    .unwrap();
    block_on(session.collect_uncommitted()).unwrap();
    block_on(session.commit_downstream(Some(200))).unwrap();
    assert_eq!(session.budget_charge().amount_usd.scaled(), 100);
    let state = store.state.lock().unwrap();
    let finalization = &state.finalizations[0];
    let trace = attribution_trace(finalization);
    assert_eq!(
        trace["request_attribution"]["provider_account_id"],
        "acct_second"
    );
    assert_eq!(
        trace["request_attribution"]["outbound_proxy_endpoint"],
        "direct"
    );
    assert_eq!(
        trace["request_attribution"]["response_model"],
        "gpt-6-astra"
    );
    assert!(trace["request_attribution"]["billing_model"].is_null());
    assert!(finalization.billing.billing_model.is_none());
    assert!(finalization.billing.calculated_cost.is_none());
    assert_eq!(finalization.cost_ticks, Some(0));
    let attempts = trace["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["stage"] == "request.attempt_attribution")
        .collect::<Vec<_>>();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["attemptIndex"], 1);
    assert_eq!(attempts[1]["attemptIndex"], 2);
    assert_eq!(attempts[0]["data"]["provider_account_id"], "acct_first");
    assert_eq!(
        attempts[0]["data"]["outbound_proxy_endpoint"],
        "http://proxy.example:8080/"
    );
    assert_eq!(attempts[1]["data"]["provider_account_id"], "acct_second");
    assert_eq!(attempts[1]["data"]["outbound_proxy_endpoint"], "direct");
    assert!(!trace.to_string().contains("synthetic"));
}

#[test]
fn attribution_does_not_fabricate_billing_from_provider_cost_or_fallback_response() {
    let operation = generate_operation();
    let (coordinator, store, _) = coordinator(vec![Script::Stream {
        account_id: "acct_one",
        items: vec![
            Ok(GatewayEvent::Started(
                ResponseMeta::new("unknown", "gpt-5").with_observed_model(None),
            )),
            Ok(GatewayEvent::ProviderCost(
                ProviderReportedCost::from_usd_ticks(200).unwrap(),
            )),
            Ok(GatewayEvent::Completed(
                ResponseMeta::new("unknown", "gpt-5").with_observed_model(None),
            )),
        ],
    }]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation.clone(),
        plan(&operation),
        None,
        None,
        CancellationToken::new(),
    ))
    .unwrap();
    block_on(session.collect_uncommitted()).unwrap();
    block_on(session.commit_downstream(Some(200))).unwrap();
    let state = store.state.lock().unwrap();
    let finalization = &state.finalizations[0];
    let trace = attribution_trace(finalization);
    assert!(trace["request_attribution"]["response_model"].is_null());
    assert!(trace["request_attribution"]["billing_model"].is_null());
    assert!(trace["request_attribution"]["outbound_proxy_endpoint"].is_null());
    assert_eq!(finalization.cost_ticks, Some(200));
}

#[test]
fn attribution_rejects_unvalidated_account_metadata_without_fabricating_an_attempt() {
    let operation = generate_operation();
    let (coordinator, store, _) = coordinator(vec![Script::AttributedStream {
        account_id: "acct_out_of_scope",
        proxy: None,
        items: complete_stream(None),
    }]);
    let mut session = block_on(coordinator.start(
        model_request(&operation, SystemTime::now() + Duration::from_secs(30)),
        operation.clone(),
        plan(&operation),
        None,
        None,
        CancellationToken::new(),
    ))
    .unwrap();
    assert!(matches!(
        block_on(session.collect_uncommitted()),
        Err(EngineError::AccountOutsideClientScope)
    ));
    let state = store.state.lock().unwrap();
    let trace = attribution_trace(&state.finalizations[0]);
    assert!(state.attempts.is_empty());
    assert!(trace["request_attribution"]["provider_account_id"].is_null());
    assert!(trace["request_attribution"]["route_model"].is_null());
    assert!(trace["request_attribution"]["outbound_proxy_endpoint"].is_null());
}
