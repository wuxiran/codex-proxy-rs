// 直接测试生产缓存的时钟边界，不为测试扩大 Provider 的公开 API。
#[path = "../src/turn_state_pin.rs"]
mod implementation;

use implementation::{MAX_PIN_AGE, TurnStatePins, credential_binding};
use std::time::{Duration, SystemTime};

#[test]
fn pins_require_success_are_immutable_and_expire_without_sliding() {
    let pins = TurnStatePins::default();
    let now = SystemTime::now();
    let binding = credential_binding("generation", "access-token");
    let mut failed = pins.attempt("account", binding.clone(), "model", "client", now);
    failed.observe(Some(&"f".repeat(292)));
    drop(failed);
    assert!(pins.status("account", &binding, now).is_empty());
    let mut first = pins.attempt("account", binding.clone(), "model", "client", now);
    first.observe(Some(&"b".repeat(312)));
    first.completed(now);
    assert!(pins.status("account", &binding, now).is_empty());
    first.observe(Some(&"a".repeat(292)));
    first.completed(now);
    let later = now + Duration::from_secs(3599);
    let mut second = pins.attempt("account", binding.clone(), "model", "client", later);
    assert_eq!(second.value(), Some("a".repeat(292).as_str()));
    second.observe(Some(&"c".repeat(292)));
    second.completed(later);
    let status = pins.status("account", &binding, later);
    assert_eq!(status[0].model, "model");
    assert_eq!(status[0].captured_at, now);
    assert_eq!(status[0].hits, 1);
    assert!(
        pins.attempt("account", binding, "model", "client", now + MAX_PIN_AGE)
            .value()
            .is_none()
    );
}

#[test]
fn pins_separate_account_model_client_and_credential_and_can_be_cleared() {
    let pins = TurnStatePins::default();
    let now = SystemTime::now();
    let binding = credential_binding("first", "token");
    let mut first = pins.attempt("account", binding.clone(), "model", "client", now);
    first.observe(Some(&"a".repeat(292)));
    first.completed(now);
    for (account, epoch, model, client) in [
        ("other", binding.clone(), "model", "client"),
        ("account", binding.clone(), "other", "client"),
        ("account", binding.clone(), "model", "other"),
        (
            "account",
            credential_binding("second", "token"),
            "model",
            "client",
        ),
        (
            "account",
            credential_binding("first", "new-token"),
            "model",
            "client",
        ),
    ] {
        assert!(
            pins.attempt(account, epoch, model, client, now)
                .value()
                .is_none()
        );
    }
    pins.clear("other");
    assert_eq!(pins.status("account", &binding, now).len(), 1);
    pins.clear("account");
    assert!(pins.status("account", &binding, now).is_empty());
}

#[test]
fn concurrent_successes_choose_one_candidate_and_long_requests_cannot_renew_expired_state() {
    let pins = TurnStatePins::default();
    let now = SystemTime::now();
    let mut first = pins.attempt("account", "binding".to_owned(), "model", "client", now);
    let mut second = pins.attempt("account", "binding".to_owned(), "model", "client", now);
    first.observe(Some(&"a".repeat(292)));
    second.observe(Some(&"b".repeat(292)));
    second.completed(now);
    first.completed(now);
    assert_eq!(
        pins.attempt("account", "binding".to_owned(), "model", "client", now)
            .value(),
        Some("b".repeat(292).as_str())
    );
    let mut late = pins.attempt("account", "binding".to_owned(), "model", "client", now);
    late.observe(Some(&"c".repeat(292)));
    late.completed(now + MAX_PIN_AGE);
    assert!(
        pins.status("account", "binding", now + MAX_PIN_AGE)
            .is_empty()
    );
}
