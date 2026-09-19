use std::sync::{Arc, Mutex};

use chrono::Utc;
use gateway_admin::{
    AdminServices,
    model::{
        AdminErrorKind,
        proxies::{ProxyRecord, ProxyTestResult},
        public_import::{PublicImportItemStatus, UpdatePublicImportConfig},
    },
};
use gateway_core::account::OutboundProxy;
use serde_json::{Map, Value, json};

use super::{
    account_groups::{FakeGroupStore, group_id},
    accounts::{FakeAccountStore, FakeProviderAdmin, context, events, recorded, revision},
    proxies::TestProxies,
};

struct Fixture {
    services: AdminServices,
    provider: Arc<FakeProviderAdmin>,
    store: Arc<FakeAccountStore>,
    reserved: Arc<Mutex<Vec<String>>>,
    events: super::accounts::EventLog,
}

async fn fixture(pool: Vec<ProxyRecord>) -> Fixture {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    // 假 store 只认识 acct_test；导入返回同一 ID，后续的 state 绑定才能读到账号。
    provider.set_import_account_ids(&["acct_test"]);
    let store = FakeAccountStore::new("openai", events.clone());
    let reserved = Arc::new(Mutex::new(Vec::new()));
    let services = super::AdminHarness::new()
        .provider(provider.clone())
        .accounts(store.clone())
        .account_groups(Arc::new(FakeGroupStore::default()))
        .proxies(Arc::new(TestProxies {
            events: Some(events.clone()),
            records: Some(Arc::new(Mutex::new(pool))),
            reserved: Some(reserved.clone()),
            ..Default::default()
        }))
        .build()
        .await;
    Fixture {
        services,
        provider,
        store,
        reserved,
        events,
    }
}

fn proxy(id: &str, tested: Option<bool>) -> ProxyRecord {
    let now = Utc::now();
    ProxyRecord {
        location: None,
        id: id.to_owned(),
        name: format!("出口 {id}"),
        proxy: OutboundProxy::parse("http://127.0.0.1:8080").expect("proxy"),
        revision: revision(1),
        account_count: 0,
        last_test_at: tested.map(|_| now),
        last_test: tested.map(|success| ProxyTestResult {
            success,
            latency_ms: 10,
            exit_ip: None,
            message: String::new(),
        }),
        created_at: now,
        updated_at: now,
    }
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().expect("object")
}

async fn enable(fixture: &Fixture, pin_turn_state: bool) -> String {
    fixture
        .services
        .public_import()
        .update(
            &context("enable-entry"),
            UpdatePublicImportConfig {
                enabled: true,
                group_ids: vec![group_id()],
                pin_turn_state,
                expires_at: None,
            },
        )
        .await
        .expect("enable entry")
        .token
}

#[tokio::test]
async fn entry_should_stay_closed_until_an_administrator_enables_it() {
    let fixture = fixture(vec![proxy("proxy_ok", Some(true))]).await;
    let service = fixture.services.public_import();

    assert_eq!(service.entry("imp-anything").await.expect("entry"), None);
    let config = service.config().await.expect("config");
    assert!(!config.enabled);
    assert!(config.token.starts_with("imp-") && config.token.len() == 68);
    // 令牌正确但入口未开启时同样不可用。
    assert_eq!(service.entry(&config.token).await.expect("entry"), None);
    assert!(
        service
            .import(
                &config.token,
                "req",
                object(json!({ "refresh_token": "rt" }))
            )
            .await
            .expect("import")
            .is_none()
    );
    assert!(recorded(&fixture.events).is_empty());
}

#[tokio::test]
async fn update_should_require_existing_groups_before_enabling() {
    let fixture = fixture(Vec::new()).await;
    let service = fixture.services.public_import();

    let error = service
        .update(
            &context("enable-empty"),
            UpdatePublicImportConfig {
                enabled: true,
                group_ids: Vec::new(),
                pin_turn_state: true,
                expires_at: None,
            },
        )
        .await
        .expect_err("empty groups");
    assert_eq!(error.kind(), AdminErrorKind::Invalid);

    let error = service
        .update(
            &context("enable-missing"),
            UpdatePublicImportConfig {
                enabled: true,
                group_ids: vec![
                    gateway_core::routing::AccountGroupId::new(
                        "grp_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    )
                    .expect("group ID"),
                ],
                pin_turn_state: true,
                expires_at: None,
            },
        )
        .await
        .expect_err("missing group");
    assert_eq!(error.kind(), AdminErrorKind::Invalid);
}

#[tokio::test]
async fn import_should_split_accounts_bind_tested_proxy_groups_and_pin_state() {
    let fixture = fixture(vec![
        proxy("proxy_untested", None),
        proxy("proxy_failed", Some(false)),
        proxy("proxy_ok", Some(true)),
    ])
    .await;
    let token = enable(&fixture, true).await;
    let service = fixture.services.public_import();

    let entry = service.entry(&token).await.expect("entry").expect("open");
    assert_eq!(entry.group_names, ["Primary"]);
    assert!(entry.pin_turn_state);

    let result = service
        .import(
            &token,
            "public-import-request",
            object(json!({
                "code": 0,
                "data": {
                    "proxies": [{ "proxy_key": "k", "protocol": "socks5", "host": "evil.example", "port": 1 }],
                    "accounts": [
                        { "name": "first", "proxy_key": "k", "credentials": { "refresh_token": "rt-1" } },
                        { "credentials": { "email": "second@example.com" }, "outboundProxyUrl": "socks5://evil.example:1" },
                    ],
                },
            })),
        )
        .await
        .expect("import")
        .expect("authorized");

    assert_eq!(result.items.len(), 2);
    assert!(result.items.iter().all(|item| {
        item.status == PublicImportItemStatus::Imported
            && item.imported_accounts == 1
            && item.state_pinned
            && item.proxy_name.as_deref() == Some("出口 proxy_ok")
    }));
    assert_eq!(result.items[0].name.as_deref(), Some("first"));
    assert_eq!(result.items[1].name.as_deref(), Some("second@example.com"));

    // 只有通过测试的代理进入随机池，文档自带的代理引用被剥离。
    assert_eq!(*fixture.reserved.lock().unwrap(), ["proxy_ok", "proxy_ok"]);
    let documents = fixture.provider.import_documents();
    assert_eq!(documents.len(), 2);
    for document in &documents {
        assert!(document.get("proxies").is_none());
        let accounts = document["accounts"].as_array().expect("accounts");
        assert_eq!(accounts.len(), 1);
        for field in ["proxy_key", "outboundProxyUrl", "outbound_proxy_url"] {
            assert!(accounts[0].get(field).is_none());
        }
    }

    let settings = fixture.store.import_settings();
    assert_eq!(settings.len(), 2);
    for settings in settings
        .into_iter()
        .map(|settings| settings.expect("settings"))
    {
        assert!(settings.enabled);
        assert_eq!(settings.group_ids, [group_id()]);
    }
    let events = recorded(&fixture.events);
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == "store.commit_rotation")
            .count(),
        2
    );
    assert!(
        fixture
            .store
            .audit_requests()
            .iter()
            .all(|request| request == "public-import-request")
    );
}

#[tokio::test]
async fn import_should_skip_state_pin_when_disabled_and_report_item_failures() {
    let fixture = fixture(vec![proxy("proxy_ok", Some(true))]).await;
    let token = enable(&fixture, false).await;
    fixture
        .provider
        .fail_next(gateway_admin::ports::provider::ProviderAdminErrorKind::Invalid);

    let result = fixture
        .services
        .public_import()
        .import(
            &token,
            "req",
            object(json!({ "accounts": [{ "name": "bad" }, { "name": "good" }] })),
        )
        .await
        .expect("import")
        .expect("authorized");

    assert_eq!(result.items[0].status, PublicImportItemStatus::Failed);
    assert!(result.items[0].message.is_some());
    assert_eq!(result.items[1].status, PublicImportItemStatus::Imported);
    assert!(!result.items[1].state_pinned);
    assert!(!recorded(&fixture.events).contains(&"store.commit_rotation"));
}

#[tokio::test]
async fn import_should_reject_wrong_token_rotated_token_and_unsupported_documents() {
    let fixture = fixture(vec![proxy("proxy_ok", Some(true))]).await;
    let token = enable(&fixture, true).await;
    let service = fixture.services.public_import();
    let document = || object(json!({ "refresh_token": "rt" }));

    assert!(
        service
            .import("imp-wrong", "req", document())
            .await
            .expect("import")
            .is_none()
    );
    for bad in [
        json!({ "cdks": ["CDK-1"] }),
        json!({ "accounts": [] }),
        json!({ "accounts": "nope" }),
        json!({ "accounts": vec![json!({ "name": "n" }); 201] }),
    ] {
        let error = service
            .import(&token, "req", object(bad))
            .await
            .expect_err("unsupported document");
        assert_eq!(error.kind(), AdminErrorKind::Invalid);
    }

    let rotated = service
        .rotate_token(&context("rotate"))
        .await
        .expect("rotate");
    assert_ne!(rotated.token, token);
    assert!(rotated.enabled);
    assert!(
        service
            .import(&token, "req", document())
            .await
            .expect("import")
            .is_none()
    );
    assert!(recorded(&fixture.events).is_empty());
}

#[tokio::test]
async fn import_should_fail_closed_without_a_tested_proxy() {
    let fixture = fixture(vec![proxy("proxy_untested", None)]).await;
    let token = enable(&fixture, true).await;

    let error = fixture
        .services
        .public_import()
        .import(&token, "req", object(json!({ "refresh_token": "rt" })))
        .await
        .expect_err("no proxy");
    assert_eq!(error.kind(), AdminErrorKind::Conflict);
    assert!(recorded(&fixture.events).is_empty());
}

#[tokio::test]
async fn link_should_stop_working_once_the_configured_expiry_passes() {
    let fixture = fixture(vec![proxy("proxy_ok", Some(true))]).await;
    let service = fixture.services.public_import();
    let update = |expires_at| UpdatePublicImportConfig {
        enabled: true,
        group_ids: vec![group_id()],
        pin_turn_state: true,
        expires_at,
    };

    // 开启时不接受已经过去的有效期。
    let error = service
        .update(
            &context("expired"),
            update(Some(Utc::now() - chrono::Duration::seconds(1))),
        )
        .await
        .expect_err("past expiry");
    assert_eq!(error.kind(), AdminErrorKind::Invalid);

    let expires_at = Utc::now() + chrono::Duration::milliseconds(300);
    let config = service
        .update(&context("short-lived"), update(Some(expires_at)))
        .await
        .expect("short-lived link");
    assert_eq!(config.expires_at, Some(expires_at));
    let entry = service
        .entry(&config.token)
        .await
        .expect("entry")
        .expect("still valid");
    assert_eq!(entry.expires_at, Some(expires_at));

    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert_eq!(service.entry(&config.token).await.expect("entry"), None);
    assert!(
        service
            .import(
                &config.token,
                "req",
                object(json!({ "refresh_token": "rt" }))
            )
            .await
            .expect("import")
            .is_none()
    );
    // 更换链接不重置有效期：过期后必须由管理员重新设定。
    let rotated = service
        .rotate_token(&context("rotate"))
        .await
        .expect("rotate");
    assert_eq!(rotated.expires_at, Some(expires_at));
    assert_eq!(service.entry(&rotated.token).await.expect("entry"), None);

    let renewed = service
        .update(&context("renew"), update(None))
        .await
        .expect("renew");
    assert!(
        service
            .entry(&renewed.token)
            .await
            .expect("entry")
            .is_some()
    );
    assert!(recorded(&fixture.events).is_empty());
}
