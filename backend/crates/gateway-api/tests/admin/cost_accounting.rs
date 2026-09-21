use std::sync::Mutex;

use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use chrono::{DateTime, NaiveDate, Utc};
use gateway_admin::{
    model::cost_accounting::*,
    ports::{
        cost_accounting::CostAccountingStore,
        store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult},
    },
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use super::{AdminTestFixture, AdminTestState};

/// 只认识 `acct_known`；用量固定，足以验证路由、校验与响应形状。
#[derive(Default)]
pub(super) struct MemoryCosts(Mutex<Vec<AccountPurchase>>);

#[async_trait]
impl CostAccountingStore for MemoryCosts {
    async fn set_purchase(&self, command: SetAccountPurchase) -> AdminStoreResult<AccountPurchase> {
        if command.account_id != "acct_known" {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::NotFound,
                "account purchase",
                "missing account",
            ));
        }
        let mut purchases = self.0.lock().unwrap();
        if purchases.is_empty() {
            purchases.push(AccountPurchase {
                account_ref: command.account_id.clone(),
                name: "Known".to_owned(),
                email: Some("known@example.com".to_owned()),
                price_cents: None,
                purchased_at: "2026-09-21T02:00:00Z".parse().unwrap(),
                retired_at: None,
                note: None,
            });
        }
        let purchase = &mut purchases[0];
        if let Some(price) = command.price_cents {
            purchase.price_cents = price;
        }
        if let Some(at) = command.purchased_at {
            purchase.purchased_at = at;
        }
        if let Some(note) = command.note {
            purchase.note = note;
        }
        Ok(purchase.clone())
    }
    async fn set_retired(
        &self,
        account_ids: &[String],
        retired_at: Option<DateTime<Utc>>,
    ) -> AdminStoreResult<Vec<String>> {
        let mut changed = Vec::new();
        for purchase in self.0.lock().unwrap().iter_mut() {
            if account_ids.contains(&purchase.account_ref) {
                purchase.retired_at = retired_at;
                changed.push(purchase.account_ref.clone());
            }
        }
        Ok(changed)
    }
    async fn purchases(
        &self,
    ) -> AdminStoreResult<Vec<(AccountPurchase, Option<AccountLiveState>)>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .map(|purchase| {
                (
                    purchase,
                    Some(AccountLiveState {
                        enabled: false,
                        credential_ready: true,
                        quota_exhausted: true,
                        plan_type: Some("team".to_owned()),
                    }),
                )
            })
            .collect())
    }
    async fn refresh_daily(&self, _: NaiveDate, _: NaiveDate) -> AdminStoreResult<()> {
        Ok(())
    }
    async fn daily_usage(
        &self,
        _: NaiveDate,
        _: NaiveDate,
    ) -> AdminStoreResult<Vec<DailyAccountUsage>> {
        Ok(vec![DailyAccountUsage {
            day: "2026-09-21".parse().unwrap(),
            account_ref: "acct_known".to_owned(),
            usage_micros: 535_250_000,
            request_count: 3_000,
            total_tokens: 350_000_000,
        }])
    }
}

async fn request(
    fixture: &AdminTestFixture,
    path: &str,
    body: Option<Value>,
    authenticated: bool,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .header("x-request-id", "req_cost_tests")
        .uri(path)
        .method(if body.is_some() { "POST" } else { "GET" });
    if authenticated {
        builder = builder.header(header::COOKIE, "cpr_session=valid-session");
    }
    let body = body.map_or_else(Body::empty, |body| Body::from(body.to_string()));
    let response = gateway_api::admin::cost_accounting::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

const RANGE: &str = "from=2026-09-21&to=2026-09-21";

#[tokio::test]
async fn purchase_price_flows_into_daily_and_account_accounting() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let (status, saved) = request(
        &fixture,
        "/api/admin/cost-accounting/purchase",
        Some(json!({"accountId": "acct_known", "price": 51.5, "note": "  第一批  "})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["data"]["price"], 51.5);
    assert_eq!(saved["data"]["note"], "第一批");
    assert!(saved["data"]["retiredAt"].is_null());

    let (status, daily) = request(
        &fixture,
        &format!("/api/admin/cost-accounting/daily?{RANGE}"),
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let day = &daily["data"]["days"][0];
    assert_eq!(day["day"], "2026-09-21");
    assert_eq!(day["purchasedCount"], 1);
    assert_eq!(day["spend"], 51.5);
    assert_eq!(day["usageUsd"], 535.25);
    assert!((day["costPerUsd"].as_f64().unwrap() - 51.5 / 535.25).abs() < 1e-12);
    assert_eq!(daily["data"]["totals"]["spend"], 51.5);

    let (_, accounts) = request(
        &fixture,
        &format!("/api/admin/cost-accounting/accounts?{RANGE}"),
        None,
        true,
    )
    .await;
    let row = &accounts["data"]["items"][0];
    assert_eq!(row["accountId"], "acct_known");
    assert_eq!(row["quotaExhausted"], true);
    assert_eq!(row["planType"], "team");
    assert_eq!(row["usageUsd"], 535.25);

    // 省略 price 保留原价，null 清除。
    let (_, kept) = request(
        &fixture,
        "/api/admin/cost-accounting/purchase",
        Some(json!({"accountId": "acct_known", "note": null})),
        true,
    )
    .await;
    assert_eq!(kept["data"]["price"], 51.5);
    assert!(kept["data"]["note"].is_null());
    let (_, cleared) = request(
        &fixture,
        "/api/admin/cost-accounting/purchase",
        Some(json!({"accountId": "acct_known", "price": null})),
        true,
    )
    .await;
    assert!(cleared["data"]["price"].is_null());
}

#[tokio::test]
async fn retired_accounts_leave_the_account_list_but_stay_in_daily_accounting() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    request(
        &fixture,
        "/api/admin/cost-accounting/purchase",
        Some(json!({"accountId": "acct_known", "price": 50})),
        true,
    )
    .await;
    let (status, retired) = request(
        &fixture,
        "/api/admin/cost-accounting/retire",
        Some(json!({"accountIds": ["acct_known"], "retired": true})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retired["data"]["accountIds"], json!(["acct_known"]));

    let (_, hidden) = request(
        &fixture,
        &format!("/api/admin/cost-accounting/accounts?{RANGE}"),
        None,
        true,
    )
    .await;
    assert_eq!(hidden["data"]["items"], json!([]));
    let (_, shown) = request(
        &fixture,
        &format!("/api/admin/cost-accounting/accounts?{RANGE}&includeRetired=true"),
        None,
        true,
    )
    .await;
    assert!(shown["data"]["items"][0]["retiredAt"].is_string());
    let (_, daily) = request(
        &fixture,
        &format!("/api/admin/cost-accounting/daily?{RANGE}"),
        None,
        true,
    )
    .await;
    assert_eq!(daily["data"]["totals"]["spend"], 50.0);
}

#[tokio::test]
async fn cost_routes_require_auth_and_reject_invalid_input() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    for (path, body) in [
        (format!("/api/admin/cost-accounting/daily?{RANGE}"), None),
        (format!("/api/admin/cost-accounting/accounts?{RANGE}"), None),
        (
            "/api/admin/cost-accounting/purchase".to_owned(),
            Some(json!({"accountId": "acct_known"})),
        ),
        (
            "/api/admin/cost-accounting/retire".to_owned(),
            Some(json!({"accountIds": ["a"], "retired": true})),
        ),
    ] {
        assert_eq!(
            request(&fixture, &path, body, false).await.0,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
    for query in [
        "",
        "from=2026-09-21",
        "from=2026-09-22&to=2026-09-21",
        "from=2025-01-01&to=2026-09-21",
        "from=not-a-date&to=2026-09-21",
        "from=2026-09-21&to=2026-09-21&unknown=1",
    ] {
        let status = request(
            &fixture,
            &format!("/api/admin/cost-accounting/daily?{query}"),
            None,
            true,
        )
        .await
        .0;
        assert!(status.is_client_error(), "{query}: {status}");
    }
    for body in [
        json!({"accountId": "acct_known", "price": -1}),
        json!({"accountId": "acct_known", "price": 1e13}),
        json!({"accountId": "", "price": 1}),
        json!({"accountId": "acct_known", "price": 1, "extra": true}),
        json!({"accountId": "acct_known", "purchasedAt": "2999-01-01T00:00:00Z"}),
    ] {
        let status = request(
            &fixture,
            "/api/admin/cost-accounting/purchase",
            Some(body.clone()),
            true,
        )
        .await
        .0;
        assert!(status.is_client_error(), "{body}: {status}");
    }
    assert_eq!(
        request(
            &fixture,
            "/api/admin/cost-accounting/purchase",
            Some(json!({"accountId": "acct_missing", "price": 1})),
            true
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let status = request(
        &fixture,
        "/api/admin/cost-accounting/retire",
        Some(json!({"accountIds": [], "retired": true})),
        true,
    )
    .await
    .0;
    assert!(status.is_client_error());
}
