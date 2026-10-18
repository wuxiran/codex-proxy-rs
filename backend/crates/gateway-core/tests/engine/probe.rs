//! fork：探针执行事实从 trace 还原时，暖池验收状态与证明有效性分开记录。

use gateway_core::engine::probe::AccountProbeExecution;
use serde_json::json;

#[test]
fn warm_origin_is_separate_from_a_proof_that_no_longer_matches() {
    let execution = AccountProbeExecution::from_trace(Some(json!({"events": [{
        "stage": "upstream.connection", "data": {
            "reused": true, "warmVerified": false, "warmVerificationStatus": "conditions_changed",
            "warmVerifiedAtMs": 1700000000000u64, "warmVerificationAgeMs": 241000,
            "connectionId": "fixture-warm"
        }
    }]})));
    assert_eq!(execution.warm_pool_used, Some(true));
    assert_eq!(execution.warm_verified, Some(false));
    assert_eq!(
        execution.warm_verification_status.as_deref(),
        Some("conditions_changed")
    );
    assert_eq!(execution.warm_verification_age_ms, Some(241000));
}

#[test]
fn retry_or_http_exchange_cannot_inherit_a_previous_websocket_verification() {
    for reset in [
        json!({"stage": "attempt.started", "data": {}}),
        json!({"stage": "upstream.exchange.started", "data": {"transport": "http_sse"}}),
    ] {
        let execution = AccountProbeExecution::from_trace(Some(json!({"events": [{
            "stage": "upstream.connection", "data": {
                "reused": true, "warmVerified": true, "warmVerificationStatus": "fresh",
                "warmVerifiedAtMs": 1700000000000u64, "warmVerificationAgeMs": 8000
            }
        }, reset]})));
        assert_eq!(execution.warm_verified, None);
        assert_eq!(execution.warm_verification_status, None);
        assert_eq!(execution.warm_verified_at_ms, None);
        assert_eq!(execution.warm_verification_age_ms, None);
    }
}
