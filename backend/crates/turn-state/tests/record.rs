use std::time::{Duration, UNIX_EPOCH};

use turn_state::{BucketRecord, IssuedAtSource, Source};

fn record() -> BucketRecord {
    let issued = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
    BucketRecord {
        account: "acct".into(),
        model: "gpt-6-astra".into(),
        value: "v".repeat(292),
        len: 292,
        issued_at: issued,
        issued_at_source: IssuedAtSource::Fernet,
        captured_at: issued,
        expires_at: issued + Duration::from_secs(3600),
        binding: "b".into(),
        egress: Some("e".into()),
        client: None,
        source: Source::Hunt,
        hits: 3,
        gateway: None,
    }
}

#[test]
fn debug_never_prints_the_value() {
    let rendered = format!("{:?}", record());
    assert!(rendered.contains("<redacted>"));
    assert!(!rendered.contains(&"v".repeat(20)));
}

#[test]
fn round_trips_through_json_with_millisecond_precision() {
    let original = record();
    let json = serde_json::to_string(&original).expect("serialize");
    assert!(json.contains("\"issuedAt\":1700000000123"));
    let back: BucketRecord = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, original);
    assert!(back.matches_path("acct", "gpt-6-astra"));
    assert!(!back.matches_path("other", "gpt-6-astra"));
    assert!(back.consistent());
}

#[test]
fn inconsistent_length_is_detected() {
    let mut broken = record();
    broken.len = 5;
    assert!(!broken.consistent());
}
