use std::time::{Duration, SystemTime, UNIX_EPOCH};

use turn_state::IssuedAtSource;
use turn_state::fernet::{FutureStamped, issued_at, resolve_issued_at};

use crate::{fernet_token, plain_token};

#[test]
fn reads_the_embedded_issue_time() {
    let token = fernet_token(1_700_000_000, 292);
    assert_eq!(
        issued_at(&token),
        Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
    );
    // 带 padding 也能读。
    assert_eq!(issued_at(&format!("{token}==")), issued_at(&token));
}

#[test]
fn non_fernet_values_fall_back_to_capture_time() {
    assert_eq!(issued_at(&plain_token(292)), None);
    let captured = UNIX_EPOCH + Duration::from_secs(1_000);
    let now = captured + Duration::from_secs(5);
    let resolved = resolve_issued_at(&plain_token(292), captured, now, Duration::from_secs(60))
        .expect("fallback");
    assert_eq!(resolved.at, captured);
    assert_eq!(resolved.source, IssuedAtSource::Captured);
}

#[test]
fn future_stamped_tokens_are_rejected_but_skew_is_tolerated() {
    let now = UNIX_EPOCH + Duration::from_secs(2_000_000_000);
    let far = fernet_token(2_000_000_000 + 600, 292);
    assert_eq!(
        resolve_issued_at(&far, now, now, Duration::from_secs(120)).err(),
        Some(FutureStamped)
    );
    let near = fernet_token(2_000_000_000 + 60, 292);
    let resolved = resolve_issued_at(&near, now, now, Duration::from_secs(120)).expect("skew");
    assert_eq!(resolved.source, IssuedAtSource::Fernet);
}

#[test]
fn garbage_is_not_a_token() {
    assert_eq!(issued_at(""), None);
    assert_eq!(issued_at("gAAA"), None);
    assert_eq!(issued_at("!!!!"), None);
    let _ = SystemTime::now();
}
