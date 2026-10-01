use std::time::{Duration, SystemTime};

use turn_state::{
    AccountWidePin, Decision, InjectMode, PinRejected, RequestFacts, Settings, Source,
    TurnStateService, credential_binding,
};

use crate::{fernet_token, plain_token};

const EGRESS: &str = "egress-a";

fn facts<'a>(
    binding: &str,
    client: &'a str,
    carried: Option<&'a str>,
    now: SystemTime,
) -> RequestFacts<'a> {
    RequestFacts {
        account: "acct",
        binding: binding.to_owned(),
        model: "gpt-6-astra",
        client,
        egress: EGRESS,
        carried,
        now,
    }
}

fn pin<'a>(binding: &str, value: &'a str, now: SystemTime) -> AccountWidePin<'a> {
    AccountWidePin {
        account: "acct",
        binding: binding.to_owned(),
        model: "gpt-6-astra",
        egress: EGRESS.to_owned(),
        value,
        captured_at: now,
        now,
        source: Source::Hunt,
        ttl: None,
        gateway: None,
    }
}

#[test]
fn passive_capture_requires_completion_and_is_reused_by_the_same_client() {
    let service = TurnStateService::in_memory();
    let now = SystemTime::now();
    let binding = credential_binding("gen", "token");
    let value = plain_token(292);

    let mut failed = service.begin_request(facts(&binding, "cli", None, now));
    assert_eq!(failed.decision(), Decision::Pass);
    failed.observe(Some(&value));
    drop(failed);
    assert!(service.status("acct", &binding, now).is_empty());

    let mut first = service.begin_request(facts(&binding, "cli", None, now));
    first.observe(Some(&value));
    first.completed(now);
    let status = service.status("acct", &binding, now);
    assert_eq!(status.len(), 1);
    assert!(!status[0].account_wide);
    assert_eq!(status[0].source, Source::Passive);

    let second = service.begin_request(facts(&binding, "cli", None, now));
    assert_eq!(second.decision(), Decision::Inject);
    assert_eq!(second.value(), Some(value.as_str()));
    let other = service.begin_request(facts(&binding, "other", None, now));
    assert!(other.value().is_none());
    assert_eq!(service.status("acct", &binding, now)[0].hits, 1);
}

#[test]
fn account_wide_pin_persists_expires_from_fernet_time_and_is_egress_bound() {
    let dir = tempfile::tempdir().expect("tempdir");
    let service = TurnStateService::open(dir.path()).expect("open");
    let now = SystemTime::now();
    let issued_secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("epoch")
        .as_secs()
        - 100;
    let token = fernet_token(issued_secs, 292);
    let binding = credential_binding("gen", "token");
    let expires = service
        .pin_account_wide(pin(&binding, &token, now))
        .expect("pinned");
    let expected = SystemTime::UNIX_EPOCH + Duration::from_secs(issued_secs + 240);
    assert_eq!(expires, expected);
    assert_eq!(
        service.account_wide_expires_at("acct", &binding, "gpt-6-astra", EGRESS, now),
        Some(expected)
    );
    assert_eq!(
        service.account_wide_expires_at("acct", &binding, "gpt-6-astra", "elsewhere", now),
        None
    );

    let reopened = TurnStateService::open(dir.path()).expect("reopen");
    let attempt = reopened.begin_request(facts(&binding, "any", None, now));
    assert_eq!(attempt.value(), Some(token.as_str()));
    let buckets = reopened.buckets(now);
    assert_eq!(buckets.len(), 1);
    assert_eq!(buckets[0].scope, "account");
    assert_eq!(buckets[0].source, Source::Hunt);
}

#[test]
fn pin_rejections_cover_length_expiry_and_future_stamps() {
    let service = TurnStateService::in_memory();
    let now = SystemTime::now();
    assert_eq!(
        service
            .pin_account_wide(pin("b", &plain_token(100), now))
            .err(),
        Some(PinRejected::Length)
    );
    let stale = AccountWidePin {
        captured_at: now - Duration::from_secs(3600),
        ..pin("b", "", now)
    };
    let stale_value = plain_token(292);
    let stale = AccountWidePin {
        value: &stale_value,
        ..stale
    };
    assert_eq!(
        service.pin_account_wide(stale).err(),
        Some(PinRejected::Expired)
    );
    let future_secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("epoch")
        .as_secs()
        + 3600;
    let future = fernet_token(future_secs, 292);
    assert_eq!(
        service.pin_account_wide(pin("b", &future, now)).err(),
        Some(PinRejected::FutureStamped)
    );
}

#[test]
fn replace_only_and_dry_run_change_the_verdict_but_not_the_capture() {
    let service = TurnStateService::in_memory();
    let now = SystemTime::now();
    let binding = credential_binding("gen", "token");
    let template = plain_token(292);
    service
        .update_settings(Settings {
            inject_mode: InjectMode::ReplaceOnly,
            template_lengths: vec![292],
            degraded_lengths: vec![312],
            ..Settings::default()
        })
        .expect("settings");
    service
        .pin_account_wide(pin(&binding, &template, now))
        .expect("pinned");

    let degraded = "d".repeat(312);
    let substituted = service.begin_request(facts(&binding, "cli", Some(&degraded), now));
    assert_eq!(substituted.decision(), Decision::Substitute);
    assert_eq!(substituted.value(), Some(template.as_str()));
    let untouched = service.begin_request(facts(&binding, "cli", None, now));
    assert_eq!(untouched.decision(), Decision::Pass);
    assert!(untouched.value().is_none());

    service
        .update_settings(Settings {
            dry_run: true,
            ..Settings::default()
        })
        .expect("dry run");
    let dry = service.begin_request(facts(&binding, "cli", None, now));
    assert_eq!(dry.decision(), Decision::Inject);
    assert!(dry.value().is_none());
    assert_eq!(service.status("acct", &binding, now)[0].hits, 1);
}

#[test]
fn observations_distinguish_injected_silence() {
    let service = TurnStateService::in_memory();
    let now = SystemTime::now();
    let binding = credential_binding("gen", "token");
    service
        .pin_account_wide(pin(&binding, &plain_token(292), now))
        .expect("pinned");
    let mut injected = service.begin_request(facts(&binding, "cli", None, now));
    injected.observe(None);
    drop(injected);
    let mut plain = service.begin_request(facts("other-binding", "cli", None, now));
    plain.observe(Some(&plain_token(780)));
    drop(plain);
    let snapshot = service.observations(now);
    let tally = &snapshot.buckets["acct/gpt-6-astra"];
    assert_eq!(tally.injected_silent, 1);
    assert_eq!(tally.normal, 1);
    assert_eq!(tally.injected_total, 1);
}

#[test]
fn clear_forgets_the_account() {
    let service = TurnStateService::in_memory();
    let now = SystemTime::now();
    let binding = credential_binding("gen", "token");
    service
        .pin_account_wide(pin(&binding, &plain_token(292), now))
        .expect("pinned");
    service.clear("acct");
    assert!(service.status("acct", &binding, now).is_empty());
    assert!(service.buckets(now).is_empty());
}

#[test]
fn mint_pins_use_their_own_ttl_and_carry_the_gateway() {
    let service = TurnStateService::in_memory();
    let now = SystemTime::now();
    let binding = credential_binding("gen", "token");
    let value = plain_token(780);
    let expires = service
        .pin_minted_account_wide(
            AccountWidePin {
                account: "acct",
                binding: binding.clone(),
                model: "gpt-6-astra",
                egress: EGRESS.to_owned(),
                value: &value,
                captured_at: now,
                now,
                source: Source::Mint,
                ttl: Some(Duration::from_secs(240)),
                gateway: Some("unified-95".to_owned()),
            },
            "route-a",
        )
        .expect("pinned");
    assert_eq!(expires.expires_at, now + Duration::from_secs(240));
    let status = service.status("acct", &binding, now);
    assert_eq!(status[0].source, Source::Mint);
    assert_eq!(status[0].gateway.as_deref(), Some("unified-95"));
    let attempt =
        service.begin_request_on_route(facts(&binding, "cli", None, now), Some("route-a"));
    assert!(!attempt.needs_template());
    let missing = service.begin_request(facts("other", "cli", None, now));
    assert!(missing.needs_template());
}

#[test]
fn cloud_mint_settings_validate_and_redact() {
    let mut settings = Settings::default();
    settings.cloud_mint.enabled = true;
    assert_eq!(settings.validate(), Ok(()), "native mode needs no relay");
    settings.cloud_mint.mode = ::turn_state::MintMode::Relay;
    assert!(settings.validate().is_err());
    settings.cloud_mint.relay_url = "http://127.0.0.1:9000".to_owned();
    settings.cloud_mint.relay_key = "k".to_owned();
    assert_eq!(settings.validate(), Ok(()));
    let shown = settings.redacted();
    assert_eq!(shown.cloud_mint.relay_key, "<set>");
    let merged = shown.merge_secret_placeholders(&settings);
    assert_eq!(merged.cloud_mint.relay_key, "k");
    let json = serde_json::to_string(&Settings::default()).expect("json");
    assert!(json.contains("\"cloudMint\""));
    assert!(json.contains("\"ticketTtlSeconds\":240"));
}

#[test]
fn warm_pool_settings_default_on_and_bounds() {
    let defaults = Settings::default();
    assert!(defaults.warm_pool.enabled, "warm pool ships default-on");
    assert_eq!(
        defaults.warm_pool.reprobe_seconds, 300,
        "low-frequency canary"
    );
    assert_eq!(defaults.validate(), Ok(()), "default warm pool validates");

    let json = serde_json::to_string(&Settings::default()).expect("json");
    assert!(json.contains("\"warmPool\""));
    assert!(json.contains("\"maxAgeSeconds\":3000"));

    // A disabled warm pool skips all bound checks.
    let mut off = Settings::default();
    off.warm_pool.enabled = false;
    off.warm_pool.connections_per_account = 0;
    assert_eq!(off.validate(), Ok(()), "disabled warm pool ignores bounds");

    let mut settings = Settings::default();
    assert_eq!(
        settings.validate(),
        Ok(()),
        "sane defaults validate when enabled"
    );

    settings.warm_pool.connections_per_account = 0;
    assert!(settings.validate().is_err(), "zero connections rejected");
    settings.warm_pool.connections_per_account = 2;

    settings.warm_pool.max_age_seconds = 4000;
    assert!(
        settings.validate().is_err(),
        "max age above upstream cap rejected"
    );
    settings.warm_pool.max_age_seconds = 3000;

    settings.warm_pool.probe_effort = "insane".to_owned();
    assert!(settings.validate().is_err(), "unknown effort rejected");
}

mod mint_routes {
    use std::time::{Duration, SystemTime};

    use turn_state::{AccountWidePin, RequestFacts, Source, TurnStateService};

    fn pin<'a>(model: &'a str, value: &'a str, now: SystemTime) -> AccountWidePin<'a> {
        AccountWidePin {
            account: "acct",
            binding: "binding".to_owned(),
            model,
            egress: "egress".to_owned(),
            value,
            captured_at: now,
            now,
            source: Source::Mint,
            ttl: Some(Duration::from_secs(240)),
            gateway: None,
        }
    }

    fn request<'a>(model: &'a str, now: SystemTime) -> RequestFacts<'a> {
        RequestFacts {
            account: "acct",
            binding: "binding".to_owned(),
            model,
            client: "cli",
            egress: "egress",
            carried: None,
            now,
        }
    }

    #[test]
    fn minted_ticket_requires_its_route_even_after_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let service = TurnStateService::open(dir.path()).unwrap();
        let now = SystemTime::now();
        let value = "a".repeat(780);
        service
            .pin_minted_account_wide(pin("model-a", &value, now), "route-a")
            .unwrap();
        for reader in [service, TurnStateService::open(dir.path()).unwrap()] {
            let valid = reader.begin_request_on_route(request("model-a", now), Some("route-a"));
            assert_eq!(valid.value(), Some(value.as_str()));
            assert_eq!(valid.injected_route(), Some("route-a"));
            for route in [None, Some("route-b")] {
                let absent = reader.begin_request_on_route(request("model-a", now), route);
                assert!(absent.value().is_none());
                assert!(absent.needs_template());
            }
            assert_eq!(reader.status("acct", "binding", now).len(), 1);
        }
    }

    #[test]
    fn same_second_mint_on_a_new_route_replaces_the_old_route_ticket() {
        let dir = tempfile::tempdir().unwrap();
        let service = TurnStateService::open(dir.path()).unwrap();
        let now = SystemTime::now();
        let old = "a".repeat(780);
        let new = "b".repeat(780);
        for model in ["model-a", "model-b"] {
            service
                .pin_minted_account_wide(pin(model, &old, now), "route-a")
                .unwrap();
        }
        service
            .pin_minted_account_wide(pin("model-a", &new, now), "route-b")
            .unwrap();
        assert_eq!(
            service
                .begin_request_on_route(request("model-a", now), Some("route-b"))
                .value(),
            Some(new.as_str())
        );
        assert!(
            service
                .begin_request_on_route(request("model-a", now), Some("route-a"))
                .value()
                .is_none()
        );
        // 账号路由更新后，另一个模型留下的旧路由票也不能搭配新路由发送。
        assert!(
            service
                .begin_request_on_route(request("model-b", now), Some("route-b"))
                .value()
                .is_none()
        );
    }

    #[test]
    fn older_mint_on_the_same_route_reports_the_retained_ticket() {
        let dir = tempfile::tempdir().unwrap();
        let service = TurnStateService::open(dir.path()).unwrap();
        let now = SystemTime::UNIX_EPOCH
            + Duration::from_secs(
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            );
        let existing = "a".repeat(780);
        let incoming = "b".repeat(312);
        let initial = service
            .pin_minted_account_wide(pin("model", &existing, now), "route")
            .unwrap();
        let mut stale = pin("model", &incoming, now);
        stale.captured_at = now - Duration::from_secs(1);
        let actual = service.pin_minted_account_wide(stale, "route").unwrap();
        assert_eq!(actual.length, existing.len());
        assert_eq!(actual.expires_at, initial.expires_at);
        assert_eq!(
            service
                .begin_request_on_route(request("model", now), Some("route"))
                .value(),
            Some(existing.as_str())
        );
    }

    #[test]
    fn legacy_unbound_mint_is_not_injected_but_hunt_still_is() {
        let service = TurnStateService::in_memory();
        let now = SystemTime::now();
        let value = "a".repeat(780);
        service.pin_account_wide(pin("model", &value, now)).unwrap();
        for route in [None, Some("route")] {
            assert!(
                service
                    .begin_request_on_route(request("model", now), route)
                    .value()
                    .is_none()
            );
        }
        let mut hunt = pin("model", &value, now);
        hunt.source = Source::Hunt;
        service.pin_account_wide(hunt).unwrap();
        assert_eq!(
            service.begin_request(request("model", now)).value(),
            Some(value.as_str())
        );
    }
}
