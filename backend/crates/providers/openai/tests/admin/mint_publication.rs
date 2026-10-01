use super::*;
use sha2::{Digest as _, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};

const ACCOUNT: &str = "acct_mint_publication";
const MODEL: &str = "gpt-5.4";

async fn fixture(
    relay: &MockServer,
) -> (
    provider_openai::ProviderBundle,
    Arc<MemoryAccountStore>,
    TestOpenAiConfig,
) {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: ACCOUNT.to_owned(),
            name: "mint fixture".to_owned(),
            secret: secret("synthetic-mint-token"),
            verified_account: profile("synthetic-mint-user"),
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    store.set_turn_state_pin(ACCOUNT, true);
    let mut config = valid_config();
    config.config.api.base_url = relay.uri();
    let bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(store.clone(), Arc::new(TestOAuthPending::default())),
    )
    .await
    .unwrap();
    let service = bundle.turn_state_service();
    let mut settings = service.settings();
    settings.cloud_mint.enabled = true;
    settings.cloud_mint.mode = turn_state::MintMode::Relay;
    settings.cloud_mint.relay_url = relay.uri();
    settings.cloud_mint.relay_key = "synthetic-relay-key".to_owned();
    service.update_settings(settings).unwrap();
    (bundle, store, config)
}

fn minted(label: &str, issued_at: DateTime<Utc>) -> Value {
    json!({
        "gateway": "unified-88", "cookies": {"__cflb": format!("cflb-{label}"), "__oailb":format!("oailb-{label}")},
        "expires_at": (issued_at + chrono::Duration::hours(1)).to_rfc3339(), "attempts": 1,
        "tickets": { MODEL: {"turn_state":label.repeat(780),"served_model":MODEL,
            "issued_at":issued_at.to_rfc3339(),"expires_at":(issued_at+chrono::Duration::seconds(240)).to_rfc3339()}}
    })
}

#[tokio::test]
async fn failed_route_write_never_publishes_a_ticket() {
    let relay = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(minted("a", Utc::now())))
        .expect(1)
        .mount(&relay)
        .await;
    let (bundle, store, _config) = fixture(&relay).await;
    store.fail_credential_writes();
    let result = bundle
        .admin_provider()
        .mint_turn_state(
            &ProviderAccountId::new(ACCOUNT).unwrap(),
            vec![MODEL.to_owned()],
        )
        .await;
    assert!(result.is_err());
    assert!(
        bundle
            .turn_state_service()
            .buckets(SystemTime::now())
            .is_empty(),
        "a failed route commit must not leave a consumable ticket"
    );
}

fn route(label: &str) -> String {
    hex::encode(Sha256::digest(
        format!("cflb-{label}\0oailb-{label}").as_bytes(),
    ))
}

async fn binding(store: &MemoryAccountStore) -> String {
    let loaded = store
        .load_current_credential(&ProviderAccountId::new(ACCOUNT).unwrap())
        .await
        .unwrap();
    let data = CodexCredentialCodec::decode_complete(&loaded.credential).unwrap();
    let oauth = data.oauth().unwrap();
    turn_state::credential_binding(
        oauth.turn_state_pin.as_deref().unwrap(),
        &oauth.access_token,
    )
}

#[tokio::test]
async fn committed_mint_is_only_injected_with_its_committed_route() {
    let relay = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(minted("a", Utc::now())))
        .mount(&relay)
        .await;
    let (bundle, store, _config) = fixture(&relay).await;
    let report = bundle
        .admin_provider()
        .mint_turn_state(
            &ProviderAccountId::new(ACCOUNT).unwrap(),
            vec![MODEL.to_owned()],
        )
        .await
        .unwrap();
    assert!(report.pair_written);
    let data = store
        .repository()
        .load_complete_data(&store.account(ACCOUNT).unwrap())
        .await
        .unwrap();
    assert_eq!(
        data.cookies()
            .iter()
            .find(|cookie| cookie.name == "__cflb")
            .unwrap()
            .value,
        "cflb-a"
    );
    let binding = binding(&store).await;
    let egress = turn_state::egress_fingerprint(None);
    for expected in [None, Some(route("b")), Some(route("a"))] {
        let attempt = bundle.turn_state_service().begin_request_on_route(
            turn_state::RequestFacts {
                account: ACCOUNT,
                binding: binding.clone(),
                model: MODEL,
                client: "cli",
                egress: &egress,
                carried: None,
                now: SystemTime::now(),
            },
            expected.as_deref(),
        );
        assert_eq!(attempt.value().is_some(), expected == Some(route("a")));
    }
}

#[tokio::test]
async fn concurrent_route_or_credential_change_rejects_the_stale_mint() {
    for change in ["route", "credential"] {
        let relay = MockServer::start().await;
        let (bundle, store, _config) = fixture(&relay).await;
        let received = Arc::new(AtomicBool::new(false));
        let relay_received = received.clone();
        Mock::given(method("GET"))
            .respond_with(move |_: &wiremock::Request| {
                relay_received.store(true, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_json(minted("a", Utc::now()))
            })
            .mount(&relay)
            .await;
        store.on_credential_load(Arc::new(move |store, id, _| {
            let received = received.clone();
            Box::pin(async move {
                if received.swap(false, Ordering::SeqCst) {
                    let loaded = store.load_current_credential(id).await?;
                    let account = loaded.account;
                    let mut data =
                        CodexCredentialCodec::decode_complete(&loaded.credential).unwrap();
                    let oauth = data.oauth_mut().unwrap();
                    if change == "credential" {
                        oauth.access_token = "synthetic-refreshed-token".to_owned();
                    } else {
                        oauth.cookies = ["__cflb", "__oailb"]
                            .into_iter()
                            .map(|name| provider_openai::credential::CodexCookie {
                                name: name.to_owned(),
                                value: format!("{}-b", name.trim_start_matches('_')),
                                domain: "127.0.0.1".to_owned(),
                                path: "/".to_owned(),
                                host_only: false,
                                secure: false,
                                expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
                            })
                            .collect();
                    }
                    let update = gateway_core::account::CredentialCasUpdate::new(
                        id.clone(),
                        account.revision(),
                        gateway_core::account::ProviderAccountUpdate {
                            account_id: id.clone(),
                            name: account.name().to_owned(),
                            email: account.email().map(str::to_owned),
                            plan_type: account.plan_type().map(str::to_owned),
                        },
                        CodexCredentialCodec::encode_complete(data).unwrap(),
                        account.has_refresh_token(),
                        account.access_token_expires_at(),
                        account.next_refresh_at(),
                    )
                    .unwrap()
                    .preserving_profile();
                    assert!(matches!(
                        store.compare_and_swap_credential(update).await?,
                        gateway_core::account::CredentialCasOutcome::Updated(_)
                    ));
                }
                Ok(())
            })
        }));
        let result = bundle
            .admin_provider()
            .mint_turn_state(
                &ProviderAccountId::new(ACCOUNT).unwrap(),
                vec![MODEL.to_owned()],
            )
            .await;
        assert!(
            result.is_err(),
            "{change} change must reject the stale mint"
        );
        assert!(
            bundle
                .turn_state_service()
                .buckets(SystemTime::now())
                .is_empty()
        );
        let data = store
            .repository()
            .load_complete_data(&store.account(ACCOUNT).unwrap())
            .await
            .unwrap();
        if change == "route" {
            assert_eq!(
                data.cookies()[0].value,
                "cflb-b",
                "mint must not overwrite the concurrent route"
            );
        } else {
            assert_eq!(
                data.oauth().unwrap().access_token,
                "synthetic-refreshed-token"
            );
            assert!(data.cookies().is_empty());
        }
    }
}

#[tokio::test]
async fn relay_ticket_needs_matching_model_and_complete_route() {
    for invalid in ["missing_model", "wrong_model", "missing_route"] {
        let relay = MockServer::start().await;
        let mut response = minted("a", Utc::now());
        match invalid {
            "missing_model" => {
                response["tickets"][MODEL]
                    .as_object_mut()
                    .unwrap()
                    .remove("served_model");
            }
            "wrong_model" => response["tickets"][MODEL]["served_model"] = json!("gpt-other"),
            _ => {
                response["cookies"]
                    .as_object_mut()
                    .unwrap()
                    .remove("__oailb");
            }
        }
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .mount(&relay)
            .await;
        let (bundle, store, _config) = fixture(&relay).await;
        assert!(
            bundle
                .admin_provider()
                .mint_turn_state(
                    &ProviderAccountId::new(ACCOUNT).unwrap(),
                    vec![MODEL.to_owned()]
                )
                .await
                .is_err(),
            "{invalid}"
        );
        assert!(
            bundle
                .turn_state_service()
                .buckets(SystemTime::now())
                .is_empty()
        );
        assert!(
            store
                .repository()
                .load_complete_data(&store.account(ACCOUNT).unwrap())
                .await
                .unwrap()
                .cookies()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn observe_only_mint_does_not_publish_route_or_ticket() {
    let relay = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(minted("a", Utc::now())))
        .mount(&relay)
        .await;
    let (bundle, store, _config) = fixture(&relay).await;
    let service = bundle.turn_state_service();
    let mut settings = service.settings();
    settings.cloud_mint.observe_only = true;
    service.update_settings(settings).unwrap();
    store.fail_credential_writes();
    let report = bundle
        .admin_provider()
        .mint_turn_state(
            &ProviderAccountId::new(ACCOUNT).unwrap(),
            vec![MODEL.to_owned()],
        )
        .await
        .unwrap();
    assert!(report.observe_only);
    assert!(!report.pair_written);
    assert!(service.buckets(SystemTime::now()).is_empty());
    assert!(
        store
            .repository()
            .load_complete_data(&store.account(ACCOUNT).unwrap())
            .await
            .unwrap()
            .cookies()
            .is_empty()
    );
}
