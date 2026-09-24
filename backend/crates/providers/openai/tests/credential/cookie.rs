use url::Url;

use provider_openai::credential::{CodexCookiePolicy, CookiePolicyError};

fn policy() -> CodexCookiePolicy {
    CodexCookiePolicy::new(["session"], ["chatgpt.com"]).expect("valid policy")
}

#[test]
fn capture_should_reject_parent_public_suffix_outside_allowlist() {
    let error = policy()
        .validate_capture(
            &Url::parse("https://chatgpt.com/backend-api").expect("valid URL"),
            Some("com"),
            "session",
            "/",
        )
        .err()
        .expect("public suffix must be rejected");

    assert_eq!(error, CookiePolicyError::InvalidScope);
}

#[test]
fn replay_should_respect_host_only_cookie_scope() {
    let policy = policy();

    assert!(!policy.may_replay(
        &Url::parse("https://api.chatgpt.com/backend-api").expect("valid URL"),
        "chatgpt.com",
        "/",
        true,
        true,
    ));
}

#[test]
fn replay_should_respect_secure_cookie_attribute() {
    let policy = policy();

    assert!(!policy.may_replay(
        &Url::parse("http://chatgpt.com/backend-api").expect("valid URL"),
        "chatgpt.com",
        "/",
        false,
        true,
    ));
}

/// 本机联调的回环上游会被加进允许域（含 http 下非 Secure 的 pair 回放）；生产域不受影响。
#[test]
fn loopback_upstream_is_allowed_only_when_configured() {
    let official = provider_openai::credential::CodexCookiePolicy::official().unwrap();
    let dev = url::Url::parse("http://127.0.0.1:18090/codex/responses").unwrap();
    assert!(!official.may_replay(&dev, "127.0.0.1", "/", false, false));
    let with_dev = provider_openai::credential::CodexCookiePolicy::official()
        .unwrap()
        .with_loopback_upstream("http://127.0.0.1:18090");
    assert!(with_dev.may_replay(&dev, "127.0.0.1", "/", false, false));
    assert!(
        !with_dev.may_replay(&dev, "127.0.0.1", "/", false, true),
        "Secure 不回放到 http"
    );
    let prod = url::Url::parse("https://chatgpt.com/backend-api/codex/responses").unwrap();
    assert!(with_dev.may_replay(&prod, "chatgpt.com", "/", false, true));
    let untouched = provider_openai::credential::CodexCookiePolicy::official()
        .unwrap()
        .with_loopback_upstream("https://chatgpt.com");
    assert!(!untouched.may_replay(&dev, "127.0.0.1", "/", false, false));
}
