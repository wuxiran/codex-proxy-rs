//! fork：免登录导入只收新的 OAuth 账号（`OpenAiService::import_new_accounts`）。

use gateway_admin::model::provider_credentials::ImportCredentials;

use super::super::accounts::{
    FakeAccountStore, FakeProviderAdmin, context, document, events, recorded,
};
use super::openai::service;

#[tokio::test]
async fn openai_new_accounts_import_should_ask_store_to_reject_existing_identities() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    let store = FakeAccountStore::new("openai", events.clone());
    let services = service(provider.clone(), store.clone()).await;

    services
        .openai()
        .import_new_accounts(ImportCredentials {
            outbound_proxy_id: None,
            settings: Some(super::super::accounts::import_settings()),
            context: context("public-import"),
            document: document(),
        })
        .await
        .expect("import new account");

    let recorded = recorded(&events);
    assert!(recorded.contains(&"store.reject_existing"));
}

#[tokio::test]
async fn openai_new_accounts_import_should_reject_non_oauth_credentials_before_commit() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    provider.set_import_authentication_kind("api_key");
    let store = FakeAccountStore::new("openai", events.clone());
    let services = service(provider.clone(), store.clone()).await;

    let error = services
        .openai()
        .import_new_accounts(ImportCredentials {
            outbound_proxy_id: None,
            settings: Some(super::super::accounts::import_settings()),
            context: context("public-import"),
            document: document(),
        })
        .await
        .expect_err("API key accounts must be rejected");

    assert_eq!(error.message(), "此入口只接受 OAuth 账号");
    assert!(!recorded(&events).contains(&"store.commit_import"));
}
