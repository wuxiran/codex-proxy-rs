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
