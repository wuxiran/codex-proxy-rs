use std::sync::Arc;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use provider_openai::config::CodexCdkSettings;
use provider_openai::credential::token_client::{RefreshFailure, TokenPair, TokenRefresher};
use provider_openai::credential::{
    CodexCdkClient, CodexCredentialAdminService, extract_cdk_codes, is_cdk_code,
};
use serde_json::json;
use wiremock::matchers::{header, method, path};
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

#[tokio::test]
async fn cdk_document_redeems_and_imports_signed_export() {
    let server = MockServer::start().await;
    let redemption_id = "11111111-1111-1111-1111-111111111111";
    Mock::given(method("POST"))
        .and(path("/api/cdk/redeem"))
        .and(header("x-cdk-client-version", "20260907-receipt-capacity"))
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
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/cdk/redemptions/{redemption_id}/download"
        )))
        .and(header("x-cdk-download-token", "test-download-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(signed_export("user-cdk")))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let cdk = Arc::new(
        CodexCdkClient::new(
            dir.path().to_path_buf(),
            CodexCdkSettings {
                enabled: true,
                base_url: server.uri(),
                client_id: "r1-testclient".to_owned(),
                client_version: "20260907-receipt-capacity".to_owned(),
            },
        )
        .unwrap(),
    );
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    )
    .with_cdk_client(cdk);
    let prepared = service
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
