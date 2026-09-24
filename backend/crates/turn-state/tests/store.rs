use std::time::{Duration, SystemTime, UNIX_EPOCH};

use turn_state::store::{PinStore, Scope};
use turn_state::{BucketRecord, IssuedAtSource, Source};

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

fn account_wide(account: &str, model: &str, binding: &str, issued: SystemTime) -> BucketRecord {
    BucketRecord {
        account: account.into(),
        model: model.into(),
        value: "v".repeat(292),
        len: 292,
        issued_at: issued,
        issued_at_source: IssuedAtSource::Captured,
        captured_at: issued,
        expires_at: issued + Duration::from_secs(3600),
        binding: binding.into(),
        egress: Some("egress-a".into()),
        client: None,
        source: Source::Hunt,
        hits: 0,
        gateway: None,
    }
}

fn passive(
    account: &str,
    model: &str,
    binding: &str,
    client: &str,
    issued: SystemTime,
) -> BucketRecord {
    BucketRecord {
        egress: None,
        client: Some(client.into()),
        source: Source::Passive,
        value: "p".repeat(292),
        ..account_wide(account, model, binding, issued)
    }
}

fn scope(client: &str) -> Scope {
    Scope {
        account: "acct".into(),
        binding: "bind".into(),
        model: "gpt-6-astra".into(),
        client: Some(client.into()),
    }
}

#[test]
fn account_wide_pin_survives_a_fresh_open_and_index_is_value_free() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = PinStore::open(dir.path()).expect("open");
    let expires = store
        .pin_account_wide(account_wide("acct", "gpt-6-astra", "bind", now()), now())
        .expect("pinned");
    assert_eq!(expires, now() + Duration::from_secs(3600));
    let file = dir.path().join("buckets/acct/gpt-6-astra.json");
    assert!(file.exists());
    let index = std::fs::read_to_string(dir.path().join("index.json")).expect("index");
    assert!(index.contains("acct/gpt-6-astra"));
    assert!(!index.contains(&"v".repeat(20)));

    let reopened = PinStore::open(dir.path()).expect("reopen");
    let (value, matched) = reopened
        .lookup(&scope("cli"), "egress-a", now())
        .expect("hit from disk");
    assert_eq!(value, "v".repeat(292));
    assert!(matched.client.is_none());
    assert!(reopened.lookup(&scope("cli"), "egress-b", now()).is_none());
}

#[test]
fn file_whose_contents_disagree_with_its_path_is_dropped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = PinStore::open(dir.path()).expect("open");
    let dir_a = dir.path().join("buckets/acct");
    std::fs::create_dir_all(&dir_a).expect("mkdir");
    let foreign = account_wide("other", "gpt-6-astra", "bind", now());
    std::fs::write(
        dir_a.join("gpt-6-astra.json"),
        serde_json::to_vec(&foreign).expect("json"),
    )
    .expect("write");
    assert!(store.lookup(&scope("cli"), "egress-a", now()).is_none());
    assert!(!dir_a.join("gpt-6-astra.json").exists());
}

#[test]
fn binding_mismatch_and_expiry_are_not_served() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = PinStore::open(dir.path()).expect("open");
    store
        .pin_account_wide(account_wide("acct", "gpt-6-astra", "old", now()), now())
        .expect("pinned");
    assert!(store.lookup(&scope("cli"), "egress-a", now()).is_none());
    let mut fresh = scope("cli");
    fresh.binding = "old".into();
    assert!(store.lookup(&fresh, "egress-a", now()).is_some());
    let later = now() + Duration::from_secs(3600);
    assert!(store.lookup(&fresh, "egress-a", later).is_none());
    let just_before = now() + Duration::from_secs(3599);
    assert!(
        PinStore::open(dir.path())
            .expect("reopen")
            .lookup(&fresh, "egress-a", just_before)
            .is_some()
    );
}

#[test]
fn newest_issued_at_wins_across_two_handles_on_the_same_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blue = PinStore::open(dir.path()).expect("blue");
    let green = PinStore::open(dir.path()).expect("green");
    let newer = now() + Duration::from_secs(10);
    let mut record = account_wide("acct", "gpt-6-astra", "bind", newer);
    record.value = "n".repeat(292);
    let first = green.pin_account_wide(record, newer).expect("green pins");
    let older = blue
        .pin_account_wide(account_wide("acct", "gpt-6-astra", "bind", now()), newer)
        .expect("blue yields");
    assert_eq!(older, first);
    let (value, _) = blue
        .lookup(&scope("cli"), "egress-a", newer)
        .expect("blue serves the newer template");
    assert_eq!(value, "n".repeat(292));
}

#[test]
fn passive_pins_stay_in_memory_and_never_override_account_wide_on_same_egress() {
    let store = PinStore::in_memory();
    assert!(store.insert_passive(
        passive("acct", "gpt-6-astra", "bind", "cli", now()),
        "egress-a",
        now()
    ));
    assert!(!store.insert_passive(
        passive("acct", "gpt-6-astra", "bind", "cli", now()),
        "egress-a",
        now()
    ));
    let (value, matched) = store
        .lookup(&scope("cli"), "egress-x", now())
        .expect("client pin");
    assert_eq!(value, "p".repeat(292));
    assert_eq!(matched.client.as_deref(), Some("cli"));
    store
        .pin_account_wide(account_wide("acct", "gpt-6-astra", "bind", now()), now())
        .expect("account wide replaces");
    let (value, _) = store
        .lookup(&scope("cli"), "egress-a", now())
        .expect("account pin");
    assert_eq!(value, "v".repeat(292));
    assert!(!store.insert_passive(
        passive("acct", "gpt-6-astra", "bind", "cli2", now()),
        "egress-a",
        now()
    ));
    assert!(store.insert_passive(
        passive("acct", "gpt-6-astra", "bind", "cli2", now()),
        "egress-b",
        now()
    ));
    assert_eq!(store.status("acct", "bind", now()).len(), 2);
}

#[test]
fn clear_removes_files_and_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = PinStore::open(dir.path()).expect("open");
    store
        .pin_account_wide(account_wide("acct", "gpt-6-astra", "bind", now()), now())
        .expect("pinned");
    store
        .pin_account_wide(account_wide("acct", "gpt-5.5", "bind", now()), now())
        .expect("pinned");
    assert_eq!(store.clear_bucket("acct", Some("gpt-5.5")), 1);
    assert!(!dir.path().join("buckets/acct/gpt-5.5.json").exists());
    assert!(dir.path().join("buckets/acct/gpt-6-astra.json").exists());
    store.clear_account("acct");
    assert!(!dir.path().join("buckets/acct").exists());
    assert!(store.records(now()).is_empty());
}

#[test]
fn unsafe_bucket_keys_are_encoded_not_traversed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = PinStore::open(dir.path()).expect("open");
    store
        .pin_account_wide(account_wide("../escape", "a/b", "bind", now()), now())
        .expect("encoded");
    assert!(dir.path().join("buckets/..%2Fescape/a%2Fb.json").exists());
    assert!(!dir.path().join("escape").exists());
    let records = store.records(now());
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].account, "../escape");
    assert_eq!(records[0].model, "a/b");
}
