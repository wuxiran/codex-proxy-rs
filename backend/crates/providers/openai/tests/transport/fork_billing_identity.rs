//! fork：计费身份（按发送模型计价、响应模型只作观测、不伪造上游费用）的解码测试。

use gateway_core::event::{GatewayEvent, ProviderEvent};
use provider_openai::transport::canonical::{CodexCanonicalDecoder, CodexCanonicalOutcome};
use serde_json::json;

trait CanonicalOutcomeAssertions {
    fn expect(self, message: &str) -> Vec<ProviderEvent>;
}

impl CanonicalOutcomeAssertions for CodexCanonicalOutcome {
    fn expect(self, message: &str) -> Vec<ProviderEvent> {
        match self {
            Self::Events(events) => events,
            Self::Failed(failure) => panic!("{message}: {failure:?}"),
        }
    }
}

fn canonical_facts(events: &[ProviderEvent]) -> Vec<&GatewayEvent> {
    events
        .iter()
        .flat_map(ProviderEvent::canonical_facts)
        .collect()
}

#[test]
fn billing_identity_prices_sent_model_keeps_luna_response_and_never_forges_upstream_cost() {
    for ticks in [None, Some(0), Some(123)] {
        let mut usage = json!({"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0},"total_tokens":110});
        if let Some(ticks) = ticks {
            usage["cost_in_usd_ticks"] = json!(ticks);
        }
        let created = json!({"type":"response.created","response":{"id":"resp_identity","model":"gpt-6-astra"}});
        let completed = json!({"type":"response.completed","response":{"id":"resp_identity","model":"gpt-5.6-luna","status":"completed","output":[],"usage":usage}});
        let body = format!(
            "event: response.created\ndata: {created}\n\nevent: response.completed\ndata: {completed}\n\n"
        );
        let events = CodexCanonicalDecoder::new("gpt-6-astra")
            .push(body.as_bytes())
            .expect("decode billing identity");
        let facts = canonical_facts(&events);
        assert!(facts.iter().any(|event| matches!(event, GatewayEvent::Completed(meta)
            // 本地估价按发送模型 astra 计算，计费身份必须与之一致；luna 只是响应回显。
            if meta.observed_model() == Some("gpt-5.6-luna") && meta.billing_model() == Some("gpt-6-astra"))));
        assert!(
            facts
                .iter()
                .any(|event| matches!(event, GatewayEvent::CalculatedCost(_)))
        );
        let actual = facts.iter().find_map(|event| match event {
            GatewayEvent::ProviderCost(cost) => Some(cost.total().amount().scaled()),
            _ => None,
        });
        assert_eq!(actual, ticks.map(|ticks| ticks as u128));
    }
}

#[test]
fn billing_identity_does_not_present_fallback_as_observed_model() {
    let created = json!({"type":"response.created","response":{"id":"resp_missing"}});
    let completed = json!({"type":"response.completed","response":{"id":"resp_missing","status":"completed","output":[],"usage":{"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0},"total_tokens":110}}});
    let body = format!(
        "event: response.created\ndata: {created}\n\nevent: response.completed\ndata: {completed}\n\n"
    );
    let events = CodexCanonicalDecoder::new("gpt-6-astra")
        .push(body.as_bytes())
        .expect("decode missing identity");
    assert!(
        canonical_facts(&events)
            .iter()
            .any(|event| matches!(event, GatewayEvent::Completed(meta)
        if meta.observed_model().is_none() && meta.billing_model() == Some("gpt-6-astra")))
    );
}
