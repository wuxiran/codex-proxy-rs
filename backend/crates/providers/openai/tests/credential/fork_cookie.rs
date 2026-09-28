//! fork：本机联调回环上游的 cookie 回放范围。

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
