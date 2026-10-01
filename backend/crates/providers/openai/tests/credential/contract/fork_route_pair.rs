//! fork：换模型后按指纹条件删除路由 cookie 对。

use sha2::{Digest as _, Sha256};

use super::*;

fn fingerprint(cflb: &str, oailb: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(cflb.as_bytes());
    digest.update([0]);
    digest.update(oailb.as_bytes());
    hex::encode(digest.finalize())
}

fn pair_headers(cflb: &str, oailb: &str) -> Vec<String> {
    vec![
        format!("__cflb={cflb}; Path=/; Domain=chatgpt.com; Secure; Max-Age=3600"),
        format!("__oailb={oailb}; Path=/; Domain=chatgpt.com; Secure; Max-Age=3600"),
    ]
}

#[test]
fn a_stale_request_cannot_drop_a_pair_that_was_replaced_meanwhile() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let request_url =
        Url::parse("https://chatgpt.com/backend-api/codex/responses").expect("request URL");
    let names = |store: &Arc<MemoryAccountStore>| {
        let account = store.account("acct_primary").expect("account");
        let data =
            block_on(store.repository().load_complete_data(&account)).expect("credential data");
        let mut names: Vec<String> = data
            .cookies()
            .iter()
            .map(|cookie| format!("{}={}", cookie.name, cookie.value))
            .collect();
        names.sort();
        names
    };
    let capture = |headers: Vec<String>| {
        let account = store.account("acct_primary").expect("account");
        block_on(selector.capture_response_cookies(&account, &request_url, &headers))
            .expect("capture cookies");
    };

    capture(vec![
        "cf_clearance=keep; Path=/; Domain=chatgpt.com; Secure; Max-Age=3600".to_owned(),
    ]);
    capture(pair_headers("cflb-a", "oailb-a"));
    // 慢请求拿着的是这个版本的账号，用的是 pair A。
    let slow_request_account = store.account("acct_primary").expect("account");

    // 另一条请求已经把 pair 换成 B。
    capture(pair_headers("cflb-b", "oailb-b"));
    assert!(!block_on(selector.drop_route_pair(
        &slow_request_account,
        &fingerprint("cflb-a", "oailb-a")
    )));
    assert_eq!(
        names(&store),
        ["__cflb=cflb-b", "__oailb=oailb-b", "cf_clearance=keep"]
    );

    // 用过 pair B 的请求发现换模型：只删这一对，其余 cookie 不动；账号版本落后也能删。
    assert!(block_on(selector.drop_route_pair(
        &slow_request_account,
        &fingerprint("cflb-b", "oailb-b")
    )));
    assert_eq!(names(&store), ["cf_clearance=keep"]);
    // 已经没有 pair 了，重复调用是空操作。
    assert!(!block_on(selector.drop_route_pair(
        &slow_request_account,
        &fingerprint("cflb-b", "oailb-b")
    )));
}

#[test]
fn a_captured_oailb_lives_until_its_own_jwt_expiry() {
    use base64::Engine as _;

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let request_url =
        Url::parse("https://chatgpt.com/backend-api/codex/responses").expect("request URL");
    let exp = chrono::Utc::now().timestamp() + 3900;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(
        "{{\"host\":\"chat.gateway.unified-88.api.openai.com\",\"exp\":{exp}}}"
    ));
    let oailb = format!("eyJhbGciOiJub25lIn0.{payload}.sig");
    let account = store.account("acct_primary").expect("account");
    block_on(selector.capture_response_cookies(
        &account,
        &request_url,
        &pair_headers("cflb-a", &oailb),
    ))
    .expect("capture cookies");

    let account = store.account("acct_primary").expect("account");
    let data = block_on(store.repository().load_complete_data(&account)).expect("credential data");
    let expiry = |name: &str| {
        data.cookies()
            .iter()
            .find(|cookie| cookie.name == name)
            .and_then(|cookie| cookie.expires_at)
            .expect("expiry")
            .timestamp()
    };
    // Set-Cookie 只声明了 3600 秒；__oailb 按 JWT 的 exp，__cflb 仍按自己的声明。
    assert_eq!(expiry("__oailb"), exp);
    assert!(expiry("__cflb") < exp - 200);
}
