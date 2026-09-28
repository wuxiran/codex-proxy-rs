use std::sync::Arc;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use provider_openai::config::CodexCdkSettings;
use provider_openai::credential::token_client::{RefreshFailure, TokenPair, TokenRefresher};
use provider_openai::credential::{
    CodexCdkClient, CodexCredentialAdminService, extract_cdk_codes, is_cdk_code,
};
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{TestLeaseCoordinator, runtime_policy};

struct UnusedRefresher;

#[async_trait]
impl TokenRefresher for UnusedRefresher {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        Err(RefreshFailure::RetryableTransport {
            message: "unused".to_owned(),
        })
    }
}

fn test_jwt(user_id: &str) -> String {
    format!(
        "unverified-header.{}.unverified-signature",
        URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "https://api.openai.com/auth": { "chatgpt_user_id": user_id }
            }))
            .expect("jwt payload")
        )
    )
}

fn signed_export(user_id: &str) -> serde_json::Value {
    json!({
        "exported_at": "2026-09-17T00:00:00Z",
        "proxies": [],
        "x_revive_manifest": {
            "signature": "test-signature",
            "source": "cdk_redeem"
        },
        "accounts": [{
            "name": "cdk-user@example.invalid",
            "platform": "openai",
            "type": "oauth",
            "credentials": {
                "access_token": test_jwt(user_id),
                "refresh_token": "rt-cdk",
                "email": "cdk-user@example.invalid",
                "plan_type": "self_serve_business_prolite"
            }
        }]
    })
}

#[test]
fn cdk_code_format_and_document_extraction() {
    assert!(is_cdk_code("CDK-6RUG-KBVZ-OUTP-APZ6-73RQ-ECF5-7GZJ-ARFP"));
    assert!(is_cdk_code("cdk-aaaa-bbbb-cccc-dddd-eeee-ffff-gggg-hhhh"));
    assert!(!is_cdk_code("CDK-AAAA-BBBB-CCCC-DDDD"));
    assert!(
        extract_cdk_codes(&json!({"accounts": [{"name": "a"}]}))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        extract_cdk_codes(&json!({
            "cdks": ["CDK-AAAA-BBBB-CCCC-DDDD-EEEE-FFFF-GGGG-HHHH"]
        }))
        .unwrap(),
        Some(vec![
            "CDK-AAAA-BBBB-CCCC-DDDD-EEEE-FFFF-GGGG-HHHH".to_owned()
        ])
    );
}

const CLIENT_VERSION: &str = "20260922-credentials";

fn cdk_service(base_url: String) -> CodexCredentialAdminService {
    let dir = tempfile::tempdir().unwrap();
    let cdk = Arc::new(
        CodexCdkClient::new(
            dir.path().to_path_buf(),
            CodexCdkSettings {
                enabled: true,
                base_url,
                client_id: "r1-testclient".to_owned(),
                ..CodexCdkSettings::default()
            },
        )
        .unwrap(),
    );
    CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    )
    .with_cdk_client(cdk)
}

async fn mount_download(server: &MockServer, redemption_id: &str, token: &str, user_id: &str) {
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/cdk/redemptions/{redemption_id}/download"
        )))
        .and(query_param("format", "sub2api"))
        .and(header("x-cdk-download-token", token))
        .and(header("x-cdk-client-version", CLIENT_VERSION))
        .respond_with(ResponseTemplate::new(200).set_body_json(signed_export(user_id)))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_receipt(server: &MockServer, redemption_id: &str) {
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/cdk/redemptions/{redemption_id}/received"
        )))
        .and(header("x-cdk-client", "r1-testclient"))
        .and(header("x-cdk-client-version", CLIENT_VERSION))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "received_at": "2026-09-28T00:00:00Z"
        })))
        .expect(1)
        .mount(server)
        .await;
}

#[test]
fn default_cdk_client_version_tracks_guanlan_redeem_page() {
    assert_eq!(CodexCdkSettings::default().client_version, CLIENT_VERSION);
}

#[tokio::test]
async fn cdk_document_redeems_and_imports_signed_export() {
    let server = MockServer::start().await;
    let redemption_id = "11111111-1111-1111-1111-111111111111";
    Mock::given(method("POST"))
        .and(path("/api/cdk/redeem"))
        .and(header("x-cdk-client-version", CLIENT_VERSION))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "status": "redeemed",
            "redemption_id": redemption_id,
            "download_token": "test-download-token",
            "filename": "cdk-accounts-1.json",
            "count": 1
        })))
        .mount(&server)
        .await;
    mount_download(&server, redemption_id, "test-download-token", "user-cdk").await;
    mount_receipt(&server, redemption_id).await;

    let prepared = cdk_service(server.uri())
        .prepare_import_document(json!({
            "cdks": ["CDK-AAAA-BBBB-CCCC-DDDD-EEEE-FFFF-GGGG-HHHH"]
        }))
        .await
        .unwrap();
    assert_eq!(prepared.accounts().len(), 1);
    assert_eq!(
        prepared.accounts()[0].account.name(),
        "cdk-user@example.invalid"
    );
}

#[tokio::test]
async fn cdk_redeem_across_workspaces_imports_every_signed_file_and_acknowledges_each() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/cdk/redeem"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "status": "redeemed",
            "count": 2,
            "workspace_count": 2,
            "downloads": [
                { "redemption_id": "red-a", "download_token": "token-a", "workspace_id": "ws-a", "count": 1 },
                { "redemption_id": "red-b", "download_token": "token-b", "workspace_id": "ws-b", "count": 1 }
            ],
            "details": [
                { "code": "CDK-AAAA-BBBB-CCCC-DDDD-EEEE-FFFF-GGGG-HHHH", "status": "redeemed" },
                { "code": "CDK-IIII-JJJJ-KKKK-LLLL-MMMM-NNNN-OOOO-PPPP", "status": "redeemed" }
            ]
        })))
        .mount(&server)
        .await;
    mount_download(&server, "red-a", "token-a", "user-a").await;
    mount_download(&server, "red-b", "token-b", "user-b").await;
    mount_receipt(&server, "red-a").await;
    mount_receipt(&server, "red-b").await;

    let prepared = cdk_service(server.uri())
        .prepare_import_document(json!({
            "cdks": [
                "CDK-AAAA-BBBB-CCCC-DDDD-EEEE-FFFF-GGGG-HHHH",
                "CDK-IIII-JJJJ-KKKK-LLLL-MMMM-NNNN-OOOO-PPPP"
            ]
        }))
        .await
        .unwrap();
    // 修复前只下载第一个空间，第二个空间已消耗的 CDK 会被静默丢掉。
    assert_eq!(prepared.accounts().len(), 2);

    // 回执与兑换请求共用同一个 Idempotency-Key。
    let requests = server.received_requests().await.unwrap();
    let key_of = |request: &wiremock::Request| {
        request
            .headers
            .get("idempotency-key")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let redeem_key = requests
        .iter()
        .find(|request| request.url.path() == "/api/cdk/redeem")
        .and_then(key_of)
        .expect("redeem idempotency key");
    let receipt_keys: Vec<_> = requests
        .iter()
        .filter(|request| request.url.path().ends_with("/received"))
        .filter_map(key_of)
        .collect();
    assert_eq!(receipt_keys, vec![redeem_key.clone(), redeem_key]);
}

#[tokio::test]
async fn cdk_receipt_failure_does_not_block_import() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/cdk/redeem"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "status": "recovered",
            "downloads": [{ "redemption_id": "red-a", "download_token": "token-a" }]
        })))
        .mount(&server)
        .await;
    mount_download(&server, "red-a", "token-a", "user-a").await;
    Mock::given(method("POST"))
        .and(path("/api/cdk/redemptions/red-a/received"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let prepared = cdk_service(server.uri())
        .prepare_import_document(json!({
            "cdks": ["CDK-AAAA-BBBB-CCCC-DDDD-EEEE-FFFF-GGGG-HHHH"]
        }))
        .await
        .unwrap();
    assert_eq!(prepared.accounts().len(), 1);
}

#[tokio::test]
async fn cdk_client_update_required_names_the_version_guanlan_expects() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/cdk/redeem"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "ok": false,
            "error_code": "client_update_required",
            "error": "兑换页面已更新，请刷新页面后重试；本次未执行兑换或增加找回次数。",
            "cdk_client_version": "20261001-next"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = cdk_service(server.uri())
        .prepare_import_document(json!({
            "cdks": ["CDK-AAAA-BBBB-CCCC-DDDD-EEEE-FFFF-GGGG-HHHH"]
        }))
        .await
        .unwrap_err();
    let message = format!("{error:?}");
    assert!(message.contains("20261001-next"), "{message}");
    assert!(
        message.contains("openai.auth.cdk.client_version"),
        "{message}"
    );
}

#[tokio::test]
async fn cdk_response_without_complete_download_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/cdk/redeem"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "status": "redeemed",
            // 非法 ID 不会被拼进 URL
            "downloads": [{ "redemption_id": "../admin", "download_token": "token-a" }]
        })))
        .mount(&server)
        .await;

    let error = cdk_service(server.uri())
        .prepare_import_document(json!({
            "cdks": ["CDK-AAAA-BBBB-CCCC-DDDD-EEEE-FFFF-GGGG-HHHH"]
        }))
        .await
        .unwrap_err();
    assert!(format!("{error:?}").contains("无法识别"), "{error:?}");
}

#[tokio::test]
async fn invalid_cdk_format_is_rejected_before_import() {
    let dir = tempfile::tempdir().unwrap();
    let cdk = Arc::new(
        CodexCdkClient::new(dir.path().to_path_buf(), CodexCdkSettings::default()).unwrap(),
    );
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    )
    .with_cdk_client(cdk);
    let error = service
        .prepare_import_document(json!({ "cdks": ["NOT-A-CDK"] }))
        .await
        .unwrap_err();
    assert!(format!("{error}").contains("CDK redeem failed"));
}
