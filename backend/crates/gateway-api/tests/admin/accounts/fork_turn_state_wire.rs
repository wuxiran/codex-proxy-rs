//! fork：「已过期」筛选、turn-state 固定 / 寻路 / 自动续期与观澜复活的管理端 wire 校验。

use gateway_admin::model::accounts::AccountListStatus;
use gateway_api::admin::accounts::{ListQuery, RotateAccountRequest};
use gateway_core::{engine::probe::AccountProbeErrorSource, error::GatewayErrorKind};
use serde_json::json;

#[test]
fn account_query_should_parse_ticket_expired_status() {
    let query: ListQuery = serde_json::from_value(json!({
        "status": "expired"
    }))
    .expect("deserialize account query");

    assert_eq!(
        query.validate().expect("validate account query").status,
        Some(AccountListStatus::TicketExpired)
    );
}

#[test]
fn rotate_should_validate_turn_state_auto_hunt_and_reject_mixed_material() {
    let request = |auto: serde_json::Value| -> RotateAccountRequest {
        serde_json::from_value(json!({
            "provider": "openai",
            "accountId": "acct_1",
            "turnStateAutoHunt": auto
        }))
        .expect("decode rotate request")
    };
    request(
        json!({ "enabled": true, "modelId": "gpt-6-astra", "attempts": 5, "includeDirect": false }),
    )
    .validate()
    .expect("valid auto hunt");
    request(json!({ "enabled": false }))
        .validate()
        .expect("disabling needs no parameters");
    for invalid in [
        json!({ "enabled": true, "modelId": " ", "attempts": 5 }),
        json!({ "enabled": true, "modelId": "gpt-6-astra", "attempts": 0 }),
        json!({ "enabled": true, "modelId": "gpt-6-astra", "attempts": 201 }),
    ] {
        assert_eq!(
            request(invalid).validate().unwrap_err().field(),
            "turnStateAutoHunt"
        );
    }
    let mixed: RotateAccountRequest = serde_json::from_value(json!({
        "provider": "openai",
        "accountId": "acct_1",
        "accessToken": "token",
        "turnStateAutoHunt": { "enabled": false }
    }))
    .expect("decode rotate request");
    assert_eq!(mixed.validate().unwrap_err().field(), "pinTurnState");
}

#[test]
fn turn_state_hunt_query_should_default_and_bound_attempts() {
    use gateway_api::admin::accounts::TurnStateHuntQuery;

    let query: TurnStateHuntQuery =
        serde_json::from_value(json!({ "accountId": "acct_1", "modelId": "gpt-6-astra" }))
            .expect("decode hunt query");
    assert_eq!(query.attempts, 5);
    assert!(!query.include_direct);
    query.validate().expect("defaults are valid");
    for attempts in [0, 201] {
        let query: TurnStateHuntQuery = serde_json::from_value(json!({
            "accountId": "acct_1",
            "modelId": "gpt-6-astra",
            "attempts": attempts
        }))
        .expect("decode hunt query");
        assert_eq!(query.validate().unwrap_err().field(), "attempts");
    }
}

#[test]
fn turn_state_hunt_events_should_expose_lengths_only() {
    use gateway_admin::model::accounts::{
        TurnStateHuntAttemptError, TurnStateHuntEgress, TurnStateHuntEvent,
    };
    use gateway_api::admin::accounts::turn_state_hunt_event_data;

    let egress = TurnStateHuntEgress {
        proxy_id: Some("proxy_1".to_owned()),
        name: "东京".to_owned(),
        endpoint: Some("http://10.0.0.1:8080/".to_owned()),
    };
    let expires_at = chrono::DateTime::parse_from_rfc3339("2026-09-20T01:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let events = [
        TurnStateHuntEvent::Started {
            model: "gpt-6-astra".to_owned(),
            expected_length: 332,
            attempts: 5,
            egresses: vec![egress.clone()],
        },
        TurnStateHuntEvent::EgressStarted {
            egress,
            index: 0,
            total: 1,
        },
        TurnStateHuntEvent::Attempt {
            proxy_id: Some("proxy_1".to_owned()),
            index: 1,
            length: None,
            matched: false,
            error: Some(TurnStateHuntAttemptError {
                code: GatewayErrorKind::Timeout,
                source: AccountProbeErrorSource::Upstream,
                upstream_status: None,
                message: "timed out".to_owned(),
            }),
        },
        TurnStateHuntEvent::Attempt {
            proxy_id: None,
            index: 2,
            length: Some(332),
            matched: true,
            error: None,
        },
        TurnStateHuntEvent::EgressFinished {
            proxy_id: Some("proxy_1".to_owned()),
            attempts: 2,
            matched: true,
            skipped: None,
        },
        TurnStateHuntEvent::Hit {
            proxy_id: Some("proxy_1".to_owned()),
            attempt_index: 2,
            length: 332,
        },
        TurnStateHuntEvent::Bound {
            proxy_id: Some("proxy_1".to_owned()),
            changed: true,
        },
        TurnStateHuntEvent::Pinned {
            model: "gpt-6-astra".to_owned(),
            length: 332,
            expires_at,
        },
        TurnStateHuntEvent::Completed {
            success: true,
            requests: 2,
        },
        TurnStateHuntEvent::Failed {
            code: "account_busy",
            message: "busy".to_owned(),
        },
    ]
    .map(turn_state_hunt_event_data);

    assert_eq!(
        events,
        [
            json!({
                "type": "hunt_start", "model": "gpt-6-astra", "expectedLength": 332, "attempts": 5,
                "proxies": [{ "proxyId": "proxy_1", "name": "东京", "endpoint": "http://10.0.0.1:8080/" }]
            }),
            json!({
                "type": "proxy_start", "proxyId": "proxy_1", "name": "东京",
                "endpoint": "http://10.0.0.1:8080/", "index": 0, "total": 1
            }),
            json!({
                "type": "attempt", "proxyId": "proxy_1", "index": 1, "length": null, "matched": false,
                "error": { "code": "timeout", "source": "upstream", "upstreamStatus": null, "message": "timed out" }
            }),
            json!({ "type": "attempt", "proxyId": null, "index": 2, "length": 332, "matched": true, "error": null }),
            json!({ "type": "proxy_done", "proxyId": "proxy_1", "attempts": 2, "matched": true, "skipped": null }),
            json!({ "type": "hit", "proxyId": "proxy_1", "attemptIndex": 2, "length": 332 }),
            json!({ "type": "bound", "proxyId": "proxy_1", "changed": true }),
            json!({ "type": "pinned", "model": "gpt-6-astra", "length": 332, "expiresAt": "2026-09-20T01:00:00Z" }),
            json!({ "type": "hunt_complete", "success": true, "requests": 2 }),
            json!({ "type": "error", "code": "account_busy", "message": "busy" }),
        ]
    );
}

#[test]
fn turn_state_pin_wire_rejects_external_state_and_mixed_credentials() {
    for enabled in [true, false] {
        let value = json!({"provider":"openai", "accountId":"acct_pin", "pinTurnState":enabled});
        assert!(
            serde_json::from_value::<RotateAccountRequest>(value.clone())
                .unwrap()
                .validate()
                .is_ok()
        );
        for key in [
            "accessToken",
            "refreshToken",
            "idToken",
            "baseUrl",
            "apiKey",
            "transport",
        ] {
            let mut mixed = value.clone();
            mixed[key] = json!("untrusted");
            assert!(
                serde_json::from_value::<RotateAccountRequest>(mixed)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        let mut raw = value;
        raw["turnState"] = json!("not-accepted");
        assert!(serde_json::from_value::<RotateAccountRequest>(raw).is_err());
    }
}

#[test]
fn guanlan_revive_wire_requires_explicit_exclusive_action() {
    let valid = json!({"provider":"openai", "accountId":"acct_revive", "guanlanRevive":true});
    assert!(
        serde_json::from_value::<RotateAccountRequest>(valid.clone())
            .unwrap()
            .validate()
            .is_ok()
    );
    for (key, value) in [
        ("guanlanRevive", json!(false)),
        ("provider", json!("xai")),
        ("pinTurnState", json!(false)),
        ("accessToken", json!("test-token")),
        ("refreshToken", json!("test-token")),
        ("idToken", json!("test-token")),
        ("baseUrl", json!("https://example.com")),
        ("apiKey", json!("test-key")),
        ("transport", json!("http")),
        (
            "settings",
            json!({"accountId":"acct_revive","enabled":true,"concurrencyLimit":null,"weight":1,"groupIds":[]}),
        ),
    ] {
        let mut mixed = valid.clone();
        mixed[key] = value;
        assert!(
            serde_json::from_value::<RotateAccountRequest>(mixed)
                .unwrap()
                .validate()
                .is_err(),
            "{key}"
        );
    }
}

#[test]
fn test_bench_defaults_to_business_and_exposes_observations_without_secrets() {
    use gateway_api::admin::accounts::AccountTestBenchRequest;
    use gateway_core::engine::probe::{AccountProbeExecution, AccountProbeMode};
    let body = json!({"accountId":"acct_1","modelId":"gpt-6-astra","prompt":"question"});
    let parsed: AccountTestBenchRequest = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(parsed.mode, AccountProbeMode::Business);
    let mut diagnostic = body.clone();
    diagnostic["mode"] = json!("diagnostic");
    assert_eq!(
        serde_json::from_value::<AccountTestBenchRequest>(diagnostic)
            .unwrap()
            .mode,
        AccountProbeMode::Diagnostic
    );
    let mut invalid = body;
    invalid["mode"] = json!("unknown");
    assert!(serde_json::from_value::<AccountTestBenchRequest>(invalid).is_err());
    let event = gateway_api::admin::accounts::AccountConnectionTestEvent::from(
        gateway_admin::model::accounts::AccountConnectionTestEvent::Execution {
            mode: AccountProbeMode::Business,
            details: AccountProbeExecution {
                ticket_attached: Some(false),
                warm_pool_used: Some(true),
                warm_verified: Some(false),
                warm_verification_status: Some("expired".to_owned()),
                warm_verified_at_ms: Some(1700000000000),
                warm_verification_age_ms: Some(300000),
                connection_reused: Some(true),
                ..Default::default()
            },
        },
    );
    assert_eq!(event.data["type"], "execution");
    assert_eq!(event.data["details"]["ticketAttached"], false);
    assert_eq!(event.data["details"]["warmPoolUsed"], true);
    assert_eq!(event.data["details"]["warmVerified"], false);
    assert_eq!(event.data["details"]["warmVerificationStatus"], "expired");
    assert_eq!(event.data["details"]["warmVerifiedAtMs"], 1700000000000u64);
    assert_eq!(event.data["details"]["warmVerificationAgeMs"], 300000);
    assert!(event.data["details"]["connectionId"].is_null());
}
