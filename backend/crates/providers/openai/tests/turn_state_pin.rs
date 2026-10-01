// 直接测试生产缓存的时钟边界，不为测试扩大 Provider 的公开 API。
// 生产壳里给 lib.rs/admin.rs 用的构造与访问器在这里用不到。
#[path = "../src/turn_state_pin.rs"]
#[allow(dead_code, unused_imports)]
mod implementation;

use implementation::{CaptureRule, PinRejected, TurnStatePins, credential_binding};
use std::time::{Duration, SystemTime};

/// 多数用例不关心出口；账号级 state 与请求走同一个出口。
const EGRESS: &str = "egress-a";
/// 默认模板寿命（运行设置未改时）。
const MAX_PIN_AGE: Duration = turn_state::DEFAULT_TTL;

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
        None,
        now,
        None,
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
        None,
        now,
        None,
    );
    // 长度门已废弃：太短(<MIN)的票据仍不予捕获。
    first.observe(Some(&"b".repeat(100)));
    first.completed(now);
    assert!(pins.status("account", &binding, now).is_empty());
    first.observe(Some(&"a".repeat(292)));
    first.completed(now);
    let later = now + (MAX_PIN_AGE - Duration::from_secs(1));
    let mut second = pins.attempt(
        "account",
        binding.clone(),
        "model",
        "client",
        292,
        EGRESS,
        None,
        later,
        None,
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
            None,
            now + MAX_PIN_AGE,
            None
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
        None,
        now,
        None,
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
            pins.attempt(account, epoch, model, client, 292, EGRESS, None, now, None)
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
        None,
        now,
        None,
    );
    let mut second = pins.attempt(
        "account",
        "binding".to_owned(),
        "model",
        "client",
        292,
        EGRESS,
        None,
        now,
        None,
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
            None,
            now,
            None
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
        None,
        now,
        None,
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
            None,
            now,
            None,
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
                None,
                now,
                None
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
            None,
            now,
            None
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
            None,
            now,
            None
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
    // 换凭据后账号级 state 不可见；长度规则已废弃，不同长度参数落在同一作用域。
    assert!(
        pins.attempt(
            "account",
            credential_binding("generation", "new"),
            "astra",
            "c",
            332,
            EGRESS,
            None,
            now,
            None
        )
        .value()
        .is_none()
    );
    assert!(
        pins.attempt(
            "account", binding, "astra", "c", 356, EGRESS, None, now, None
        )
        .value()
        .is_some()
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
        pins.account_wide_expires_at("account", "binding", "astra", EGRESS, now),
        Some(now + MAX_PIN_AGE)
    );
    assert!(
        pins.account_wide_expires_at("account", "binding", "terra", EGRESS, now)
            .is_none()
    );
    // 到期以模板自身为准：捕获时刻 + 默认 TTL。
    assert_eq!(
        pins.account_wide_expires_at("account", "binding", "astra", EGRESS, now),
        Some(now + MAX_PIN_AGE)
    );
    let later = now + (MAX_PIN_AGE - Duration::from_secs(1));
    let mut reuse = pins.attempt(
        "account",
        "binding".into(),
        "astra",
        "client",
        332,
        EGRESS,
        None,
        later,
        None,
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
            None,
            now + MAX_PIN_AGE,
            None
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
        None,
        now,
        None,
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
            None,
            now,
            None
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
            None,
            now,
            None
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
            None,
            now,
            None
        )
        .value()
        .is_none()
    );
    assert!(
        pins.account_wide_expires_at("account", "binding", "astra", "egress-b", now)
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
        None,
        now,
        None,
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
            None,
            now,
            None
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
            None,
            now,
            None,
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

fn repeat(n: usize) -> String {
    "a".repeat(n)
}

#[test]
fn accepts_780_ticket_and_reads_back_on_same_egress() {
    let pins = TurnStatePins::default();
    let now = SystemTime::now();
    let value = repeat(780);
    assert!(
        pins.pin_account_wide(
            "acct",
            "bind".into(),
            "gpt-6-astra",
            0,
            "egr".into(),
            &value,
            now,
            now,
        )
        .is_ok()
    );
    let attempt = pins.attempt(
        "acct",
        "bind".into(),
        "gpt-6-astra",
        "cli",
        0,
        "egr",
        None,
        now,
        None,
    );
    assert_eq!(attempt.value(), Some(value.as_str()));
}

#[test]
fn accepts_legacy_lengths() {
    let pins = TurnStatePins::default();
    let now = SystemTime::now();
    for len in [292usize, 332, 356] {
        assert!(
            pins.pin_account_wide("a", "b".into(), "m", 0, "e".into(), &repeat(len), now, now)
                .is_ok(),
            "len {len} should pin"
        );
    }
}

#[test]
fn rejects_too_short_and_non_ascii() {
    let pins = TurnStatePins::default();
    let now = SystemTime::now();
    assert!(matches!(
        pins.pin_account_wide("a", "b".into(), "m", 0, "e".into(), &repeat(100), now, now),
        Err(PinRejected::Length)
    ));
    let non_ascii = "\u{00e9}".repeat(300);
    assert!(matches!(
        pins.pin_account_wide("a", "b".into(), "m", 0, "e".into(), &non_ascii, now, now),
        Err(PinRejected::Length)
    ));
}

/// 客户端带着受限档 state 而桶里有模板：`always` 模式直接换掉；被动捕获不因此改变。
#[test]
fn carried_state_is_replaced_by_the_account_wide_pin() {
    let pins = TurnStatePins::default();
    pins.service()
        .update_settings(turn_state::Settings {
            inject_mode: turn_state::InjectMode::Always,
            ..turn_state::Settings::default()
        })
        .unwrap();
    let now = SystemTime::now();
    let hunted = "h".repeat(332);
    pins.pin_account_wide(
        "account",
        "binding".into(),
        "astra",
        0,
        EGRESS.into(),
        &hunted,
        now,
        now,
    )
    .unwrap();
    let carried = "c".repeat(312);
    let attempt = pins.attempt(
        "account",
        "binding".into(),
        "astra",
        "client",
        0,
        EGRESS,
        Some(&carried),
        now,
        None,
    );
    assert_eq!(attempt.value(), Some(hunted.as_str()));
    let same = pins.attempt(
        "account",
        "binding".into(),
        "astra",
        "client",
        0,
        EGRESS,
        Some(&hunted),
        now,
        None,
    );
    assert!(same.value().is_none());
}

/// fork：固定自身 state 与自动续期的管理端契约（原在 tests/admin.rs 末尾）。
mod admin_rotation {
    use std::sync::Arc;
    use std::time::SystemTime;

    use gateway_admin::model::provider_credentials::{PrepareCredentialRotation, ProviderDocument};
    use gateway_core::account::OpaqueProviderData;
    use provider_openai::credential::ImportCodexOAuthCredential;
    use serde_json::json;

    use crate::admin::{TestOAuthPending, account_record, provider_ports_with, valid_config};
    use crate::support::{MemoryAccountStore, profile, secret};

    /// 续期参数绝不进凭据：现网旧版的凭据 schema 是 `deny_unknown_fields`，多一个字段就会让
    /// 回滚后的实例（以及发版排空期间的旧槽位）读不了这个账号。
    #[tokio::test]
    async fn turn_state_auto_hunt_lives_outside_the_credential_and_requires_the_pin_switch() {
        let store = Arc::new(MemoryAccountStore::default());
        store
            .seed_oauth_credential(ImportCodexOAuthCredential {
                account_id: "acct_auto_hunt".to_owned(),
                name: "auto".to_owned(),
                secret: secret("test-auto-hunt-token"),
                verified_account: profile("chatgpt-auto-hunt"),
                next_refresh_at: None,
                enabled: true,
            })
            .await;
        let account = store.account("acct_auto_hunt").unwrap();
        // 配置持有运行数据目录的 TempDir；必须活到测试结束，续期参数就写在里面。
        let config = valid_config();
        let bundle = provider_openai::initialize(
            config.config.clone(),
            provider_ports_with(Arc::clone(&store), Arc::new(TestOAuthPending::default())),
        )
        .await
        .unwrap();
        let admin = bundle.admin_provider();
        let rotate = |material: serde_json::Value| {
            admin.prepare_rotation(PrepareCredentialRotation {
                account: account_record(&account),
                provider_material: ProviderDocument::new(OpaqueProviderData::new(
                    material.as_object().unwrap().clone(),
                )),
            })
        };
        let auto = json!({"enabled":true,"model":"gpt-6-astra","attempts":5,"include_direct":true});

        // 固定没开时不能开续期：否则后台会对一个不使用 state 的账号持续发请求。
        assert!(rotate(json!({"turn_state_auto_hunt":auto})).await.is_err());
        for invalid in [
            json!({"enabled":true,"model":"gpt-6-astra","attempts":0}),
            json!({"enabled":true,"model":"gpt-6-astra","attempts":21}),
            json!({"enabled":true,"model":"","attempts":5}),
        ] {
            assert!(
                rotate(json!({"pin_turn_state":true,"turn_state_auto_hunt":invalid}))
                    .await
                    .is_err()
            );
        }

        let prepared = rotate(json!({"pin_turn_state":true,"turn_state_auto_hunt":auto}))
            .await
            .unwrap();
        let data = prepared
            .facts()
            .provider_material
            .expose_to_provider()
            .expose_to_provider();
        // 凭据里只有旧版本认识的字段。
        assert!(data.contains_key("turn_state_pin"));
        assert!(!data.contains_key("turn_state_auto_hunt"));
        assert!(prepared.facts().preserve_credential_state);

        // 准备阶段不落盘：凭据提交若失败，接口报错的同时后台不能已经按新参数开始发请求。
        let settings_file = config
            ._runtime
            .path()
            .join("deploy")
            .join("turn_state")
            .join("auto_hunt.json");
        assert!(!settings_file.exists());
        // 只有提交成功后 Admin 才调用 finish，参数此时才生效。
        prepared.into_parts().1.finish();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&settings_file).unwrap()).unwrap();
        assert_eq!(
            saved,
            json!({"acct_auto_hunt":{"model":"gpt-6-astra","attempts":5,"include_direct":true}})
        );
        // 文件损坏不能被当成「没人开启」：否则下一次保存会抹掉其它账号的设置。
        std::fs::write(&settings_file, b"{ truncated").unwrap();
        rotate(json!({"turn_state_auto_hunt":{"enabled":false}}))
            .await
            .unwrap()
            .into_parts()
            .1
            .finish();
        assert_eq!(std::fs::read(&settings_file).unwrap(), b"{ truncated");

        // 关掉固定会一并清掉续期参数；此时凭据未提交固定开关，续期任务没有可做的事。
        rotate(json!({"pin_turn_state":false})).await.unwrap();
        assert!(
            admin
                .turn_state_hunt_renewals(SystemTime::now(), std::time::Duration::from_secs(300))
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn turn_state_pin_admin_preserves_credentials_and_exposes_only_safe_status() {
        let store = Arc::new(MemoryAccountStore::default());
        store
            .seed_oauth_credential(ImportCodexOAuthCredential {
                account_id: "acct_pin_admin".to_owned(),
                name: "pin".to_owned(),
                secret: secret("test-pin-token"),
                verified_account: profile("chatgpt-pin-admin"),
                next_refresh_at: None,
                enabled: true,
            })
            .await;
        let account = store.account("acct_pin_admin").unwrap();
        let config = valid_config();
        let bundle = provider_openai::initialize(
            config.config.clone(),
            provider_ports_with(Arc::clone(&store), Arc::new(TestOAuthPending::default())),
        )
        .await
        .unwrap();
        let admin = bundle.admin_provider();
        let view = admin
            .account_configuration(account.id())
            .await
            .unwrap()
            .unwrap();
        let view = view.expose_to_provider().expose_to_provider();
        assert_eq!(view.get("pinTurnState"), Some(&json!(false)));
        assert_eq!(view.get("guanlanReviveAvailable"), Some(&json!(false)));
        assert_eq!(view.get("turnStatePins"), Some(&json!([])));
        assert_eq!(
            view.get("turnStateCaptureRule"),
            Some(&json!({"defaultLength":0,"modelLengths":{}}))
        );
        assert!(
            !serde_json::to_string(view)
                .unwrap()
                .contains("test-pin-token")
        );
        let mut generations = Vec::new();
        for enabled in [true, true, false] {
            let prepared = admin
                .prepare_rotation(PrepareCredentialRotation {
                    account: account_record(&account),
                    provider_material: ProviderDocument::new(OpaqueProviderData::new(
                        json!({"pin_turn_state":enabled})
                            .as_object()
                            .unwrap()
                            .clone(),
                    )),
                })
                .await
                .unwrap();
            let data = prepared
                .facts()
                .provider_material
                .expose_to_provider()
                .expose_to_provider();
            assert_eq!(data.get("access_token"), Some(&json!("test-pin-token")));
            assert_eq!(prepared.facts().account_id, *account.id());
            assert!(prepared.facts().preserve_profile);
            assert!(prepared.facts().preserve_credential_state);
            if enabled {
                generations.push(data["turn_state_pin"].as_str().unwrap().to_owned());
            } else {
                assert!(!data.contains_key("turn_state_pin"));
            }
        }
        assert_ne!(generations[0], generations[1]);
        for material in [
            json!({"guanlan_revive":true}),
            json!({"guanlan_revive":false}),
            json!({"guanlan_revive":true, "pin_turn_state":true}),
            json!({"guanlan_revive":true, "access_token":"injected"}),
        ] {
            let result = admin
                .prepare_rotation(PrepareCredentialRotation {
                    account: account_record(&account),
                    provider_material: ProviderDocument::new(OpaqueProviderData::new(
                        material.as_object().unwrap().clone(),
                    )),
                })
                .await;
            assert!(result.is_err());
        }
        let mixed = admin
            .prepare_rotation(PrepareCredentialRotation {
                account: account_record(&account),
                provider_material: ProviderDocument::new(OpaqueProviderData::new(
                    json!({"pin_turn_state":true, "access_token":"injected"})
                        .as_object()
                        .unwrap()
                        .clone(),
                )),
            })
            .await;
        assert!(mixed.is_err());
    }

    #[tokio::test]
    async fn turn_state_pin_team_admin_exposes_model_rules_without_enabling_capture() {
        for plan in [
            "team",
            "business",
            "self_serve_business_prolite",
            "self_serve_business_usage_based",
        ] {
            let store = Arc::new(MemoryAccountStore::default());
            let mut account_profile = profile("chatgpt-team-pin");
            account_profile.plan_type = Some(plan.to_owned());
            store
                .seed_oauth_credential(ImportCodexOAuthCredential {
                    account_id: "acct_team_pin".to_owned(),
                    name: "team-pin".to_owned(),
                    secret: secret("team-pin-test-token"),
                    verified_account: account_profile,
                    next_refresh_at: None,
                    enabled: true,
                })
                .await;
            let account = store.account("acct_team_pin").unwrap();
            let config = valid_config();
            let bundle = provider_openai::initialize(
                config.config.clone(),
                provider_ports_with(Arc::clone(&store), Arc::new(TestOAuthPending::default())),
            )
            .await
            .unwrap();
            let view = bundle
                .admin_provider()
                .account_configuration(account.id())
                .await
                .unwrap()
                .unwrap();
            let view = view.expose_to_provider().expose_to_provider();
            assert_eq!(view.get("pinTurnState"), Some(&json!(false)));
            // 长度门已废弃：所有套餐/模型统一同一作用域长度常量，不再逐模型区分。
            assert_eq!(
                view.get("turnStateCaptureRule"),
                Some(&json!({"defaultLength":0,"modelLengths":{}}))
            );
        }
    }
}
