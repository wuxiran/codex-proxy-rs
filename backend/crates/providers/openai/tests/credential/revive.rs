use std::sync::Arc;
use std::time::SystemTime;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use gateway_core::account::{
    AccountErrorReason, AccountStateChange, CredentialState, ProviderAccountId,
    ProviderAccountStore,
};
use provider_openai::config::CodexReviveSettings;
use provider_openai::credential::{
    CodexAccountProfile, CodexCredentialCodec, CodexOAuthSecret, CodexReviveService,
    ImportCodexOAuthCredential,
};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::MemoryAccountStore;

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

fn signed_pool(_user_id: &str, access_token: &str) -> serde_json::Value {
    json!({
        "exported_at": "2026-09-17T00:00:00Z",
        "signature": "test-ed25519-signature",
        "accounts": [{
            "name": "pool-a",
            "platform": "openai",
            "type": "oauth",
            "credentials": {
                "access_token": access_token,
                "refresh_token": "rt-old"
            }
        }]
    })
}

#[test]
fn unsigned_documents_are_not_archived() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemoryAccountStore::default();
    let repository = Arc::new(store).repository();
    let service = CodexReviveService::new(
        dir.path().to_path_buf(),
        CodexReviveSettings::default(),
        repository,
    )
    .unwrap();
    service
        .record_signed_document(&json!({"accounts": [{"credentials": {"access_token": "x"}}]}))
        .unwrap();
    assert!(
        dir.path()
            .join("exports")
            .read_dir()
            .unwrap()
            .next()
            .is_none()
    );
}

#[tokio::test]
async fn expired_signed_accounts_are_revived_and_tokens_are_rotated() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryAccountStore::default());
    let old_access = test_jwt("user-a");
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_revive_1".to_owned(),
            name: "pool-a".to_owned(),
            secret: CodexOAuthSecret {
                access_token: secrecy::SecretString::from(old_access.clone()),
                refresh_token: Some(secrecy::SecretString::from("rt-old")),
                id_token: None,
            },
            verified_account: CodexAccountProfile {
                email: Some("a@example.com".to_owned()),
                oauth_subject: "user-a".to_owned(),
                poid: None,
                chatgpt_account_id: "chatgpt-a".to_owned(),
                chatgpt_user_id: "user-a".to_owned(),
                plan_type: Some("plus".to_owned()),
                access_token_expires_at: None,
            },
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    let account_id = ProviderAccountId::new("acct_revive_1").unwrap();
    let account = store.get_account(&account_id).await.unwrap().unwrap();
    store
        .apply_state_change(AccountStateChange {
            account_id: account_id.clone(),
            expected_revision: account.revision(),
            credential_state: CredentialState::Expired,
            observed_at: SystemTime::now(),
            error_reason: Some(AccountErrorReason::CredentialExpired),
            message: Some("invalid_grant".to_owned()),
        })
        .await
        .unwrap();

    let server = MockServer::start().await;
    let recovered_access = test_jwt("user-a");
    Mock::given(method("POST"))
        .and(path("/verify/start"))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({
            "ok": true,
            "job": {
                "job_id": "11111111-1111-1111-1111-111111111111",
                "task_token": "secret-token",
                "status": "queued"
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/verify/11111111-1111-1111-1111-111111111111"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "job": {
                "job_id": "11111111-1111-1111-1111-111111111111",
                "status": "completed",
                "preflight_id": "pf-1",
                "unauthorized_count": 1
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/tasks"))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({
            "ok": true,
            "task": {
                "task_id": "22222222-2222-2222-2222-222222222222",
                "status": "running"
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/tasks/22222222-2222-2222-2222-222222222222"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "task": {
                "task_id": "22222222-2222-2222-2222-222222222222",
                "status": "recovered",
                "download_ready": true
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/tasks/22222222-2222-2222-2222-222222222222/download"))
        .and(header("X-Revive-Task-Token", "secret-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "accounts": [{
                "credentials": {
                    "access_token": recovered_access,
                    "refresh_token": "rt-new"
                }
            }]
        })))
        .mount(&server)
        .await;

    let settings = CodexReviveSettings {
        enabled: true,
        base_url: server.uri(),
        verify_workers: 50,
        task_workers: 10,
    };
    let service =
        CodexReviveService::for_test(dir.path().to_path_buf(), settings, store.repository())
            .unwrap();
    service
        .record_signed_document(&signed_pool("user-a", &old_access))
        .unwrap();
    let summary = service.run_cycle().await.unwrap();
    assert_eq!(summary.applied, 1);
    let refreshed = store.get_account(&account_id).await.unwrap().unwrap();
    assert_eq!(refreshed.credential_state(), CredentialState::Ready);
    let loaded = store.load_current_credential(&account_id).await.unwrap();
    let data = CodexCredentialCodec::decode_complete(&loaded.credential).unwrap();
    let oauth = data.oauth().unwrap();
    assert_eq!(oauth.refresh_token.as_deref(), Some("rt-new"));
    assert!(!format!("{summary:?}").contains("secret-token"));
    assert!(!format!("{oauth:?}").contains("rt-new"));
}
