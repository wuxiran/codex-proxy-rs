use std::time::SystemTime;

use ::turn_state::{AccountWidePin, Source, TurnStateService};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use gateway_admin::AdminServices;
use gateway_api::admin::turn_state::{self, UpdateTurnStateSettingsRequest};
use gateway_api::auth::SessionState;
use tower::ServiceExt as _;

use super::{AdminTestFixture, AdminTestState};

/// 带 turn-state 服务的会话状态；`AdminTestState` 保持默认（无服务）。
#[derive(Clone)]
struct TurnStateTestState(AdminServices, TurnStateService);

impl SessionState for TurnStateTestState {
    fn admin_services(&self) -> &AdminServices {
        &self.0
    }

    fn turn_state(&self) -> Option<&TurnStateService> {
        Some(&self.1)
    }
}

#[test]
fn update_request_requires_every_field_and_rejects_unknown_ones() {
    let request: UpdateTurnStateSettingsRequest = serde_json::from_str(
        r#"{"ttlSeconds":3600,"injectMode":"replace-only","dryRun":true,"logDecisions":true,"templateLengths":[292],"degradedLengths":[312]}"#,
    )
    .expect("full request");
    let settings = request.into_settings(&::turn_state::Settings::default());
    assert_eq!(settings.inject_mode, ::turn_state::InjectMode::ReplaceOnly);
    assert!(!settings.cloud_mint.enabled);
    assert!(serde_json::from_str::<UpdateTurnStateSettingsRequest>(r#"{"dryRun":true}"#).is_err());
    assert!(
        serde_json::from_str::<UpdateTurnStateSettingsRequest>(
            r#"{"ttlSeconds":3600,"injectMode":"always","dryRun":false,"logDecisions":true,"templateLengths":[],"degradedLengths":[],"extra":1}"#
        )
        .is_err()
    );
}

#[tokio::test]
async fn settings_round_trip_and_validation() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let service = TurnStateService::in_memory();
    let router = app(TurnStateTestState(fixture.services.clone(), service));

    let body = get_json(&router, "/api/admin/turn-state/settings").await;
    assert_eq!(body["data"]["injectMode"], "always");
    assert_eq!(body["data"]["ttlSeconds"], 3600);
    assert_eq!(body["data"]["templateLengths"], serde_json::json!([]));

    let (status, body) = post_json(
        &router,
        "/api/admin/turn-state/settings/update",
        r#"{"ttlSeconds":1800,"injectMode":"replace-only","dryRun":true,"logDecisions":false,"templateLengths":[312,292],"degradedLengths":[332]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["templateLengths"],
        serde_json::json!([292, 312])
    );
    let body = get_json(&router, "/api/admin/turn-state/settings").await;
    assert_eq!(body["data"]["injectMode"], "replace-only");
    assert_eq!(body["data"]["dryRun"], true);

    let (status, _) = post_json(
        &router,
        "/api/admin/turn-state/settings/update",
        r#"{"ttlSeconds":1800,"injectMode":"always","dryRun":false,"logDecisions":true,"templateLengths":[292],"degradedLengths":[292]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn buckets_list_filter_clear_and_observations() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let service = TurnStateService::in_memory();
    let now = SystemTime::now();
    let value = "t".repeat(292);
    service
        .pin_account_wide(AccountWidePin {
            account: "acct_a",
            binding: "bind".to_owned(),
            model: "gpt-6-astra",
            egress: "egress".to_owned(),
            value: &value,
            captured_at: now,
            now,
            source: Source::Hunt,
            ttl: None,
            gateway: None,
        })
        .expect("pinned");
    let router = app(TurnStateTestState(fixture.services.clone(), service));

    let body = get_json(&router, "/api/admin/turn-state/buckets").await;
    assert_eq!(body["data"]["buckets"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["data"]["buckets"][0]["scope"], "account");
    assert_eq!(body["data"]["buckets"][0]["len"], 292);
    assert!(body["data"]["buckets"][0].get("value").is_none());
    assert!(!body.to_string().contains(&"t".repeat(20)));
    let body = get_json(&router, "/api/admin/turn-state/buckets?account=other").await;
    assert_eq!(body["data"]["buckets"].as_array().map(Vec::len), Some(0));

    let body = get_json(&router, "/api/admin/turn-state/observations").await;
    assert_eq!(body["data"]["version"], 1);
    assert!(body["data"]["buckets"].is_object());

    let (status, body) = post_json(
        &router,
        "/api/admin/turn-state/buckets/clear",
        r#"{"account":"acct_a","model":"gpt-6-astra"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["cleared"], 1);
    let body = get_json(&router, "/api/admin/turn-state/buckets").await;
    assert_eq!(body["data"]["buckets"].as_array().map(Vec::len), Some(0));
}

#[tokio::test]
async fn without_a_service_the_routes_report_unavailable() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let router = turn_state::router::<AdminTestState>().with_state(fixture.state());
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/admin/turn-state/settings")
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_turn_state_unavailable")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

fn app(state: TurnStateTestState) -> Router {
    turn_state::router::<TurnStateTestState>().with_state(state)
}

async fn get_json(router: &Router, uri: &str) -> serde_json::Value {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_turn_state")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.expect("body"))
        .expect("JSON")
}

async fn post_json(router: &Router, uri: &str, body: &str) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_turn_state")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_owned()))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.expect("body"))
            .expect("JSON");
    (status, body)
}

#[tokio::test]
async fn cloud_mint_settings_round_trip_keeps_the_key_secret() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let service = TurnStateService::in_memory();
    let router = app(TurnStateTestState(
        fixture.services.clone(),
        service.clone(),
    ));
    let (status, body) = post_json(
        &router,
        "/api/admin/turn-state/settings/update",
        r#"{"ttlSeconds":3600,"injectMode":"always","dryRun":false,"logDecisions":true,"templateLengths":[],"degradedLengths":[],"cloudMint":{"enabled":true,"relayUrl":"http://127.0.0.1:9000","relayKey":"secret-key","gateway":"unified-95","models":["gpt-6-astra"]}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["cloudMint"]["relayKey"], "<set>");
    assert_eq!(body["data"]["cloudMint"]["ticketTtlSeconds"], 240);
    assert_eq!(service.settings().cloud_mint.relay_key, "secret-key");
    // 再提交时带 <set> 占位：密钥不变；省略 cloudMint：整块保留。
    let (status, _) = post_json(
        &router,
        "/api/admin/turn-state/settings/update",
        r#"{"ttlSeconds":3600,"injectMode":"always","dryRun":true,"logDecisions":true,"templateLengths":[],"degradedLengths":[],"cloudMint":{"enabled":true,"relayUrl":"http://127.0.0.1:9000","relayKey":"<set>","gateway":"unified-88"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(service.settings().cloud_mint.relay_key, "secret-key");
    assert_eq!(service.settings().cloud_mint.gateway, "unified-88");
    let (status, _) = post_json(
        &router,
        "/api/admin/turn-state/settings/update",
        r#"{"ttlSeconds":3600,"injectMode":"always","dryRun":false,"logDecisions":true,"templateLengths":[],"degradedLengths":[]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(service.settings().cloud_mint.enabled);
    let body = get_json(&router, "/api/admin/turn-state/settings").await;
    assert_eq!(body["data"]["cloudMint"]["relayKey"], "<set>");
    assert!(!body.to_string().contains("secret-key"));
}
