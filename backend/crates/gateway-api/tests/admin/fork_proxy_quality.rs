//! fork：代理质量检测、出口地理与批量增删的管理路由。

use axum::http::StatusCode;
use serde_json::json;

use super::{AdminTestFixture, proxies::request};

#[tokio::test]
async fn proxy_quality_check_persists_a_report_and_exposes_exit_geo() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let (_, empty) = request(
        &fixture,
        "/api/admin/proxies/create",
        Some(json!({"name": "Office", "proxyUrl": "http://test-user:private-password@proxy.example:8080"})),
        true,
    )
    .await;
    assert!(empty["data"]["record"]["quality"].is_null());
    let (status, none) = request(
        &fixture,
        "/api/admin/proxies/quality-report?id=proxy_test",
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(none["data"]["report"].is_null());

    let (status, checked) = request(
        &fixture,
        "/api/admin/proxies/quality-check",
        Some(json!({"id": "proxy_test", "revision": 1})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let report = &checked["data"]["report"];
    assert_eq!(report["score"], 100);
    assert_eq!(report["grade"], "A");
    assert_eq!(report["status"], "healthy");
    assert_eq!(report["exitIp"], "203.0.113.2");
    assert_eq!(
        report["exitGeo"],
        json!({"country": "美国", "countryCode": "US", "region": "加州", "city": null})
    );
    assert_eq!(report["items"][0]["target"], "base_connectivity");
    assert_eq!(report["items"][0]["status"], "pass");
    assert_eq!(checked["data"]["record"]["quality"]["grade"], "A");
    assert!(checked["data"]["record"]["quality"].get("items").is_none());

    let (_, saved) = request(
        &fixture,
        "/api/admin/proxies/quality-report?id=proxy_test",
        None,
        true,
    )
    .await;
    assert_eq!(saved["data"]["report"]["summary"], report["summary"]);

    let (_, tested) = request(
        &fixture,
        "/api/admin/proxies/test",
        Some(json!({"id": "proxy_test", "revision": 1})),
        true,
    )
    .await;
    assert_eq!(tested["data"]["lastTest"]["exitGeo"]["countryCode"], "US");
}

#[tokio::test]
async fn proxy_batch_routes_report_skips_without_echoing_credentials() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let (status, created) = request(
        &fixture,
        "/api/admin/proxies/batch-create",
        Some(json!({"items": [
            {"proxyUrl": " http://test-user:private-password@proxy.example:8080 "},
            {"proxyUrl": "http://test-user:private-password@proxy.example:8080"},
            {"proxyUrl": "ftp://test-user:private-password@proxy.example:21"},
        ]})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(created["data"]["created"].as_array().unwrap().len(), 1);
    // 缺省名称取脱敏端点的 host:port。
    assert_eq!(created["data"]["created"][0]["name"], "proxy.example:8080");
    assert_eq!(
        created["data"]["skipped"],
        json!([
            {"reference": "第 3 条", "reason": "代理地址不合法"},
            {"reference": "http://proxy.example:8080/", "reason": "代理地址已存在"},
        ])
    );
    assert_eq!(created["data"]["configRevision"], 1);

    let (status, deleted) = request(
        &fixture,
        "/api/admin/proxies/batch-delete",
        Some(json!({"items": [{"id": "proxy_test", "revision": 1}, {"id": "proxy_gone", "revision": 1}]})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["data"]["deletedIds"], json!(["proxy_test"]));
    assert_eq!(deleted["data"]["skipped"][0]["reference"], "proxy_gone");

    for (path, body) in [
        ("/api/admin/proxies/batch-create", json!({"items": []})),
        ("/api/admin/proxies/batch-delete", json!({"items": []})),
        (
            "/api/admin/proxies/batch-create",
            json!({"items": [{"proxyUrl": "http://a:1", "extra": 1}]}),
        ),
        (
            "/api/admin/proxies/quality-check",
            json!({"id": "proxy_test"}),
        ),
    ] {
        let status = request(&fixture, path, Some(body), true).await.0;
        assert!(status.is_client_error(), "{path}: {status}");
    }
    for path in [
        "/api/admin/proxies/quality-check",
        "/api/admin/proxies/batch-create",
        "/api/admin/proxies/batch-delete",
    ] {
        assert_eq!(
            request(&fixture, path, Some(json!({})), false).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        request(
            &fixture,
            "/api/admin/proxies/quality-report?id=proxy_test",
            None,
            false
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}
