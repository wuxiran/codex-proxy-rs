use std::sync::Arc;
use std::time::SystemTime;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use gateway_core::account::{
    AccountErrorReason, AccountStateChange, CredentialState, ProviderAccountId,
    ProviderAccountStore,
};
use provider_openai::config::CodexReviveSettings;
use provider_openai::credential::{
    CodexAccountProfile, CodexCredentialAdmin, CodexCredentialCodec, CodexOAuthSecret,
    CodexReviveService, ImportCodexOAuthCredential,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::MemoryAccountStore;

fn test_jwt(user_id: &str) -> String {
    format!(
        "unverified-header.{}.unverified-signature",
        URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "https://api.openai.com/auth": { "chatgpt_user_id": user_id, "chatgpt_account_id": "chatgpt-a" }
            }))
            .expect("jwt payload")
        )
    )
}

pub(crate) fn signed_fixture(mut document: Value) -> Value {
    fn sorted(v: &Value) -> Value {
        match v {
            Value::Object(o) => {
                let mut keys: Vec<_> = o.keys().collect();
                keys.sort();
                let mut map = serde_json::Map::new();
                for k in keys {
                    map.insert(k.clone(), sorted(&o[k]));
                }
                Value::Object(map)
            }
            Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
            x => x.clone(),
        }
    }
    let records: Vec<_> = document["accounts"].as_array().unwrap().iter().enumerate().map(|(i, a)| {
        json!({"index":i,"payload_sha256":hex::encode(Sha256::digest(serde_json::to_vec(&sorted(a)).unwrap()))})
    }).collect();
    document["x_revive_manifest"] = json!({"version":1,"signature":"test-signature-not-real","scope":["inspect","reauth","download"],"records":records});
    document
}

fn signed_pool(_user_id: &str, access_token: &str) -> Value {
    signed_fixture(
        json!({"accounts":[{"name":"pool-a","platform":"openai","type":"oauth","rate_multiplier":1.0,"credentials":{"access_token":access_token,"refresh_token":"rt-old"}}]}),
    )
}

async fn seed(store: &MemoryAccountStore, opt_in: bool) -> ProviderAccountId {
    let mut prepared = CodexCredentialAdmin
        .prepare_import(ImportCodexOAuthCredential {
            account_id: "acct_revive_1".to_owned(),
            name: "pool-a".to_owned(),
            secret: CodexOAuthSecret {
                access_token: test_jwt("user-a").into(),
                refresh_token: Some("rt-old".into()),
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
        .unwrap();
    let mut data = CodexCredentialCodec::decode_complete(&prepared.credential).unwrap();
    data.oauth_mut().unwrap().guanlan_auto_revive = opt_in;
    prepared.credential = CodexCredentialCodec::encode_complete(data).unwrap();
    store.create_account(prepared).await.unwrap();
    let id = ProviderAccountId::new("acct_revive_1").unwrap();
    let account = store.get_account(&id).await.unwrap().unwrap();
    store
        .apply_state_change(AccountStateChange {
            account_id: id.clone(),
            expected_revision: account.revision(),
            credential_state: CredentialState::Expired,
            observed_at: SystemTime::now(),
            error_reason: Some(AccountErrorReason::CredentialExpired),
            message: Some("invalid_grant".to_owned()),
        })
        .await
        .unwrap();
    id
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

#[test]
fn signed_original_keeps_decimal_bytes_and_rejects_modified_payload() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryAccountStore::default());
    let service = CodexReviveService::new(
        dir.path().to_path_buf(),
        CodexReviveSettings::default(),
        store.repository(),
    )
    .unwrap();
    let original = signed_pool("user-a", &test_jwt("user-a"));
    let raw = serde_json::to_vec_pretty(&original).unwrap();
    service.record_raw_document(&raw).unwrap();
    let archive = dir
        .path()
        .join("exports")
        .read_dir()
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(std::fs::read(&archive).unwrap(), raw);
    let mut altered = original;
    altered["accounts"][0]["name"] = json!("changed-by-client");
    assert!(service.record_signed_document(&altered).is_err());
    assert_eq!(
        std::fs::read_dir(dir.path().join("exports"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn legacy_integer_representation_is_repaired_only_with_matching_signed_hash() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryAccountStore::default());
    let service = CodexReviveService::new(
        dir.path().to_path_buf(),
        CodexReviveSettings::default(),
        store.repository(),
    )
    .unwrap();
    let original = signed_pool("user-a", &test_jwt("user-a"));
    let mut old = original.clone();
    old["accounts"][0]["rate_multiplier"] = json!(1);
    service.record_signed_document(&old).unwrap();
    let path = dir
        .path()
        .join("exports")
        .read_dir()
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(path).unwrap()).unwrap(),
        original
    );
    old["accounts"][0]["rate_multiplier"] = json!(2);
    assert!(service.record_signed_document(&old).is_err());
}

#[tokio::test]
async fn unsigned_unselected_disabled_or_global_disabled_accounts_never_submit() {
    for (opt_in, archive, account_enabled, service_enabled) in [
        (false, true, true, true),
        (true, false, true, true),
        (true, true, false, true),
        (true, true, true, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryAccountStore::default());
        let id = seed(&store, opt_in).await;
        store.set_enabled(&id, account_enabled).await.unwrap();
        let server = MockServer::start().await;
        let service = CodexReviveService::for_test(
            dir.path().to_path_buf(),
            CodexReviveSettings {
                enabled: service_enabled,
                base_url: server.uri(),
                ..Default::default()
            },
            store.repository(),
        )
        .unwrap();
        if archive {
            service
                .record_signed_document(&signed_pool("user-a", &test_jwt("user-a")))
                .unwrap();
        }
        assert_eq!(service.run_cycle().await.unwrap().applied, 0);
        assert!(server.received_requests().await.unwrap().is_empty());
        assert_eq!(
            store
                .get_account(&id)
                .await
                .unwrap()
                .unwrap()
                .credential_state(),
            CredentialState::Expired
        );
    }
}

#[tokio::test]
async fn queued_verification_is_polled_with_summary_and_failure_is_cooled_down() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryAccountStore::default());
    let id = seed(&store, true).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/verify/start"))
        .respond_with(ResponseTemplate::new(202).set_body_json(
            json!({"job":{"job_id":"verify-1","task_token":"test-only","status":"queued"}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let calls = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    Mock::given(method("GET"))
        .and(path("/verify/verify-1"))
        .and(query_param("summary", "1"))
        .respond_with(move |_: &wiremock::Request| {
            let status = if count.fetch_add(1, Ordering::SeqCst) == 0 {
                "running"
            } else {
                "failed"
            };
            ResponseTemplate::new(200)
                .set_body_json(json!({"job":{"job_id":"verify-1","status":status}}))
        })
        .mount(&server)
        .await;
    let service = CodexReviveService::for_test(
        dir.path().to_path_buf(),
        CodexReviveSettings {
            base_url: server.uri(),
            ..Default::default()
        },
        store.repository(),
    )
    .unwrap();
    service
        .record_signed_document(&signed_pool("user-a", &test_jwt("user-a")))
        .unwrap();
    assert_eq!(service.run_cycle().await.unwrap().failed, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(service.run_cycle().await.unwrap().cooled_down, 1);
    let status = service.account_status(&store.get_account(&id).await.unwrap().unwrap(), true);
    assert_eq!(status.status, "failed");
    assert_eq!(status.reason.as_deref(), Some("rejected"));
    assert!(status.next_attempt_at.is_some());
}

#[tokio::test]
async fn expired_signed_accounts_are_revived_and_tokens_are_rotated() {
    recovery_scenario(false, false).await;
}

#[tokio::test]
async fn canceling_opt_in_during_recovery_does_not_write_back() {
    recovery_scenario(true, false).await;
}

#[tokio::test]
async fn recovery_for_another_workspace_is_rejected_before_writeback() {
    recovery_scenario(false, true).await;
}

async fn recovery_scenario(disable_during_recovery: bool, wrong_workspace: bool) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryAccountStore::default());
    let old_access = test_jwt("user-a");
    let account_id = seed(&store, true).await;
    let server = MockServer::start().await;
    let recovered_access = if wrong_workspace {
        let claims = json!({"https://api.openai.com/auth":{"chatgpt_user_id":"user-a","chatgpt_account_id":"different-workspace"}});
        format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        )
    } else {
        test_jwt("user-a")
    };
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
        .and(query_param("summary", "1"))
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
    // 真实接口的 result 视图没有计数；不能把缺失 unauthorized_count 当作零。
    Mock::given(method("GET")).and(path("/verify/11111111-1111-1111-1111-111111111111"))
        .and(query_param("result","1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"job":{"job_id":"11111111-1111-1111-1111-111111111111","status":"completed","preflight_id":"pf-1"}})))
        .mount(&server).await;
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
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(100))
                .set_body_json(json!({
                    "accounts": [{
                        "credentials": {
                            "access_token": recovered_access,
                            "refresh_token": "rt-new"
                        }
                    }]
                })),
        )
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
    let stop = async {
        if !disable_during_recovery {
            return;
        }
        for _ in 0..100 {
            if server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|r| r.url.path().ends_with("/download"))
            {
                let loaded = store.load_current_credential(&account_id).await.unwrap();
                let mut data = CodexCredentialCodec::decode_complete(&loaded.credential).unwrap();
                data.oauth_mut().unwrap().guanlan_auto_revive = false;
                let update = gateway_core::account::CredentialCasUpdate::new(
                    account_id.clone(),
                    loaded.account.revision(),
                    gateway_core::account::ProviderAccountUpdate {
                        account_id: account_id.clone(),
                        name: loaded.account.name().to_owned(),
                        email: loaded.account.email().map(str::to_owned),
                        plan_type: loaded.account.plan_type().map(str::to_owned),
                    },
                    CodexCredentialCodec::encode_complete(data).unwrap(),
                    loaded.account.has_refresh_token(),
                    loaded.account.access_token_expires_at(),
                    loaded.account.next_refresh_at(),
                )
                .unwrap()
                .preserving_profile();
                store.compare_and_swap_credential(update).await.unwrap();
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("recovery never reached download");
    };
    let (summary, ()) = tokio::join!(service.run_cycle(), stop);
    let summary = summary.unwrap();
    if disable_during_recovery || wrong_workspace {
        assert_eq!(summary.applied, 0);
        assert_eq!(summary.failed, 1);
        let loaded = store.load_current_credential(&account_id).await.unwrap();
        assert_eq!(loaded.account.credential_state(), CredentialState::Expired);
        assert_eq!(
            CodexCredentialCodec::decode_complete(&loaded.credential)
                .unwrap()
                .oauth()
                .unwrap()
                .refresh_token
                .as_deref(),
            Some("rt-old")
        );
        return;
    }
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
