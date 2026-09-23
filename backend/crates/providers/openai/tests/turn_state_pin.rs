// 直接测试生产缓存的时钟边界，不为测试扩大 Provider 的公开 API。
#[path = "../src/turn_state_pin.rs"]
mod implementation;

use implementation::{CaptureRule, MAX_PIN_AGE, PinRejected, TurnStatePins, credential_binding};
use std::time::{Duration, SystemTime};

/// 多数用例不关心出口；账号级 state 与请求走同一个出口。
const EGRESS: &str = "egress-a";

#[test]
fn pins_require_success_are_immutable_and_expire_without_sliding() {
    let pins = TurnStatePins::default();
    let now = SystemTime::now();
    let binding = credential_binding("generation", "access-token");
    let mut failed = pins.attempt(
        "account",
        binding.clone(),
        "model",
        "client",
        292,
        EGRESS,
        now,
    );
    failed.observe(Some(&"f".repeat(292)));
    drop(failed);
    assert!(pins.status("account", &binding, now).is_empty());
    let mut first = pins.attempt(
        "account",
        binding.clone(),
        "model",
        "client",
        292,
        EGRESS,
        now,
    );
    // 长度门已废弃：太短(<MIN)的票据仍不予捕获。
    first.observe(Some(&"b".repeat(100)));
    first.completed(now);
    assert!(pins.status("account", &binding, now).is_empty());
    first.observe(Some(&"a".repeat(292)));
    first.completed(now);
    let later = now + Duration::from_secs(3599);
    let mut second = pins.attempt(
        "account",
        binding.clone(),
        "model",
        "client",
        292,
        EGRESS,
        later,
    );
    assert_eq!(second.value(), Some("a".repeat(292).as_str()));
    second.observe(Some(&"c".repeat(292)));
    second.completed(later);
    let status = pins.status("account", &binding, later);
    assert_eq!(status[0].model, "model");
    assert_eq!(status[0].length, 292);
    assert_eq!(status[0].captured_at, now);
    assert_eq!(status[0].hits, 1);
    assert!(
        pins.attempt(
            "account",
            binding,
            "model",
            "client",
            292,
            EGRESS,
            now + MAX_PIN_AGE
        )
        .value()
        .is_none()
    );
}

#[test]
fn pins_separate_account_model_client_and_credential_and_can_be_cleared() {
    let pins = TurnStatePins::default();
    let now = SystemTime::now();
    let binding = credential_binding("first", "token");
    let mut first = pins.attempt(
        "account",
        binding.clone(),
        "model",
        "client",
        292,
        EGRESS,
        now,
    );
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
            pins.attempt(account, epoch, model, client, 292, EGRESS, now)
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
    let mut first = pins.attempt(
        "account",
        "binding".to_owned(),
        "model",
        "client",
        292,
        EGRESS,
        now,
    );
    let mut second = pins.attempt(
        "account",
        "binding".to_owned(),
        "model",
        "client",
        292,
        EGRESS,
        now,
    );
    first.observe(Some(&"a".repeat(292)));
    second.observe(Some(&"b".repeat(292)));
    second.completed(now);
    first.completed(now);
    assert_eq!(
        pins.attempt(
            "account",
            "binding".to_owned(),
            "model",
            "client",
            292,
            EGRESS,
            now
        )
        .value(),
        Some("b".repeat(292).as_str())
    );
    let mut late = pins.attempt(
        "account",
        "binding".to_owned(),
        "model",
        "client",
        292,
        EGRESS,
        now,
    );
    late.observe(Some(&"c".repeat(292)));
    late.completed(now + MAX_PIN_AGE);
    assert!(
        pins.status("account", "binding", now + MAX_PIN_AGE)
            .is_empty()
    );
}

#[test]
fn account_wide_pin_serves_every_client_and_replaces_only_that_model() {
    let pins = TurnStatePins::default();
    let now = SystemTime::now();
    let binding = credential_binding("generation", "token");
    for model in ["astra", "terra"] {
        let mut own = pins.attempt(
            "account",
            binding.clone(),
            model,
            "client-a",
            332,
            EGRESS,
            now,
        );
        own.observe(Some(&"o".repeat(332)));
        own.completed(now);
    }
    let hunted = "h".repeat(332);
    pins.pin_account_wide(
        "account",
        binding.clone(),
        "astra",
        332,
        EGRESS.into(),
        &hunted,
        now,
        now,
    )
    .unwrap();
    // 旧出口上捕获的客户端级 state 被替换，其它模型不受影响。
    for client in ["client-a", "client-b"] {
        assert_eq!(
            pins.attempt(
                "account",
                binding.clone(),
                "astra",
                client,
                332,
                EGRESS,
                now
            )
            .value(),
            Some(hunted.as_str())
        );
    }
    assert_eq!(
        pins.attempt(
            "account",
            binding.clone(),
            "terra",
            "client-a",
            332,
            EGRESS,
            now
        )
        .value(),
        Some("o".repeat(332).as_str())
    );
    assert!(
        pins.attempt(
            "account",
            binding.clone(),
            "terra",
            "client-b",
            332,
            EGRESS,
            now
        )
        .value()
        .is_none()
    );
    let status = pins.status("account", &binding, now);
    let astra: Vec<_> = status.iter().filter(|pin| pin.model == "astra").collect();
    assert_eq!(astra.len(), 1);
    assert!(astra[0].account_wide);
    assert_eq!(astra[0].hits, 2);
    assert!(
        status
            .iter()
            .any(|pin| pin.model == "terra" && !pin.account_wide)
    );
    // 换凭据或换长度规则后账号级 state 同样不可见。
    assert!(
        pins.attempt(
            "account",
            credential_binding("generation", "new"),
            "astra",
            "c",
            332,
            EGRESS,
            now
        )
        .value()
        .is_none()
    );
    assert!(
        pins.attempt("account", binding, "astra", "c", 356, EGRESS, now)
            .value()
            .is_none()
    );
}

#[test]
fn account_wide_fallback_never_creates_client_pins_and_keeps_fixed_lifetime() {
    let pins = TurnStatePins::default();
    let now = SystemTime::now();
    let hunted = "h".repeat(332);
    pins.pin_account_wide(
        "account",
        "binding".into(),
        "astra",
        332,
        EGRESS.into(),
        &hunted,
        now,
        now,
    )
    .unwrap();
    assert_eq!(
        pins.account_wide_captured_at("account", "binding", "astra", 332, EGRESS, now),
        Some(now)
    );
    assert!(
        pins.account_wide_captured_at("account", "binding", "terra", 332, EGRESS, now)
            .is_none()
    );
    // 长度规则变了的旧 state 对续期来说等于没有。
    assert!(
        pins.account_wide_captured_at("account", "binding", "astra", 356, EGRESS, now)
            .is_none()
    );
    let later = now + Duration::from_secs(3599);
    let mut reuse = pins.attempt(
        "account",
        "binding".into(),
        "astra",
        "client",
        332,
        EGRESS,
        later,
    );
    assert_eq!(reuse.value(), Some(hunted.as_str()));
    reuse.observe(Some(&"n".repeat(332)));
    reuse.completed(later);
    assert_eq!(pins.status("account", "binding", later).len(), 1);
    // 命中不续期：寿命从捕获时刻起算。
    assert!(
        pins.attempt(
            "account",
            "binding".into(),
            "astra",
            "client",
            332,
            EGRESS,
            now + MAX_PIN_AGE
        )
        .value()
        .is_none()
    );
}

/// 在途请求开始时还没有 state；它完成前管理员钉了账号级 state。
/// 它完成时不能再写客户端级 state —— 查找优先客户端级，旧出口的值会盖过刚钉的。
#[test]
fn request_in_flight_during_a_hunt_cannot_shadow_the_account_wide_pin() {
    let pins = TurnStatePins::default();
    let now = SystemTime::now();
    let mut in_flight = pins.attempt(
        "account",
        "binding".into(),
        "astra",
        "client",
        332,
        EGRESS,
        now,
    );
    assert!(in_flight.value().is_none());
    let hunted = "h".repeat(332);
    pins.pin_account_wide(
        "account",
        "binding".into(),
        "astra",
        332,
        EGRESS.into(),
        &hunted,
        now,
        now,
    )
    .unwrap();
    in_flight.observe(Some(&"o".repeat(332)));
    in_flight.completed(now);
    assert_eq!(pins.status("account", "binding", now).len(), 1);
    assert_eq!(
        pins.attempt(
            "account",
            "binding".into(),
            "astra",
            "client",
            332,
            EGRESS,
            now
        )
        .value(),
        Some(hunted.as_str())
    );
}

/// 账号级 state 只属于探测到它的出口：账号被改绑到别处后不再使用，续期也视其为缺失。
#[test]
fn account_wide_pin_is_only_used_on_the_egress_it_was_observed_on() {
    let pins = TurnStatePins::default();
    let now = SystemTime::now();
    let hunted = "h".repeat(332);
    pins.pin_account_wide(
        "account",
        "binding".into(),
        "astra",
        332,
        EGRESS.into(),
        &hunted,
        now,
        now,
    )
    .unwrap();
    assert_eq!(
        pins.attempt(
            "account",
            "binding".into(),
            "astra",
            "client",
            332,
            EGRESS,
            now
        )
        .value(),
        Some(hunted.as_str())
    );
    assert!(
        pins.attempt(
            "account",
            "binding".into(),
            "astra",
            "client",
            332,
            "egress-b",
            now
        )
        .value()
        .is_none()
    );
    assert!(
        pins.account_wide_captured_at("account", "binding", "astra", 332, "egress-b", now)
            .is_none()
    );
    assert_ne!(
        implementation::egress_fingerprint(None),
        implementation::egress_fingerprint(Some("http://127.0.0.1:1"))
    );
    // 旧出口的账号级 state 不生效，也不能挡住新出口上的被动捕获。
    let mut on_new_egress = pins.attempt(
        "account",
        "binding".into(),
        "astra",
        "client",
        332,
        "egress-b",
        now,
    );
    on_new_egress.observe(Some(&"n".repeat(332)));
    on_new_egress.completed(now);
    assert_eq!(
        pins.attempt(
            "account",
            "binding".into(),
            "astra",
            "client",
            332,
            "egress-b",
            now
        )
        .value(),
        Some("n".repeat(332).as_str())
    );
}

#[test]
fn account_wide_pin_rejects_wrong_length_non_ascii_and_stale_captures() {
    let pins = TurnStatePins::default();
    let now = SystemTime::now();
    // 长度门已废弃：太短(<MIN)与含非 ASCII 可见字符仍拒。
    for value in ["s".repeat(199), format!("{} ", "s".repeat(331))] {
        assert_eq!(
            pins.pin_account_wide(
                "account",
                "binding".into(),
                "astra",
                332,
                EGRESS.into(),
                &value,
                now,
                now
            ),
            Err(PinRejected::Length)
        );
    }
    assert_eq!(
        pins.pin_account_wide(
            "account",
            "binding".into(),
            "astra",
            332,
            EGRESS.into(),
            &"s".repeat(332),
            now,
            now + MAX_PIN_AGE
        ),
        Err(PinRejected::Expired)
    );
    assert!(pins.status("account", "binding", now).is_empty());
}

#[test]
fn capture_rule_is_uniform_and_length_gate_is_a_floor() {
    // 长度门已废弃：expected_length 对所有套餐/模型统一，票据只按下限+ASCII 判合法。
    let now = SystemTime::now();
    for plan in ["team", "business", "self_serve_business_prolite", "pro"] {
        let rule = CaptureRule::for_plan(Some(plan));
        for model in [
            "gpt-5.5",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-6-astra",
            "gpt-6-sol",
            "unknown-model",
        ] {
            assert_eq!(
                rule.expected_length(model),
                Some(0),
                "plan={plan} model={model}"
            );
        }
    }
    // 任何 >= MIN 的合法票据都能被动捕获并钉住；太短的不捕获。
    let rule = CaptureRule::for_plan(Some("team"));
    let expected = rule.expected_length("gpt-6-astra").unwrap();
    for (length, should_pin) in [(199usize, false), (200, true), (332, true), (780, true)] {
        let pins = TurnStatePins::default();
        let mut attempt = pins.attempt(
            "account",
            "binding".into(),
            "gpt-6-astra",
            "client",
            expected,
            EGRESS,
            now,
        );
        attempt.observe(Some(&"s".repeat(length)));
        // 捕获须等请求成功完成。
        assert!(pins.status("account", "binding", now).is_empty());
        attempt.completed(now);
        let status = pins.status("account", "binding", now);
        if should_pin {
            assert_eq!(status.len(), 1, "len={length}");
            assert_eq!(status[0].length, length);
        } else {
            assert!(status.is_empty(), "len={length} should not pin");
        }
    }
}
