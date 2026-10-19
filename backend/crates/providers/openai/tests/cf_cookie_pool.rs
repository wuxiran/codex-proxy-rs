//! fork：按出口共享的 `__cf_bm` 池只收 CF 名、按出口隔离、过期不借、后采集覆盖先采集。

use chrono::{DateTime, Utc};
use provider_openai::CfCookiePool;
use secrecy::{ExposeSecret as _, SecretString};

fn harvest(
    pool: &CfCookiePool,
    egress: &str,
    name: &str,
    value: &str,
    expires: Option<DateTime<Utc>>,
) {
    pool.harvest(
        egress,
        name,
        SecretString::from(value.to_owned()),
        "chatgpt.com".to_owned(),
        "/".to_owned(),
        true,
        true,
        expires,
    );
}

#[test]
fn pools_cf_bm_per_egress_and_ignores_other_names() {
    let pool = CfCookiePool::default();
    harvest(&pool, "egr-a", "__cf_bm", "cf-a", None);
    harvest(
        &pool,
        "egr-a",
        "__Secure-next-auth.session-token",
        "auth",
        None,
    );
    let got = pool.borrow("egr-a").unwrap();
    assert_eq!(got.name, "__cf_bm");
    assert_eq!(got.value.expose_secret(), "cf-a");
    // 非同出口拿不到。
    assert!(pool.borrow("egr-b").is_none());
}

#[test]
fn expired_cookie_is_not_borrowable() {
    let pool = CfCookiePool::default();
    harvest(
        &pool,
        "egr",
        "__cf_bm",
        "old",
        Some(Utc::now() - chrono::Duration::seconds(5)),
    );
    assert!(pool.borrow("egr").is_none());
}

#[test]
fn latest_harvest_wins() {
    let pool = CfCookiePool::default();
    harvest(&pool, "egr", "__cf_bm", "v1", None);
    harvest(&pool, "egr", "__cf_bm", "v2", None);
    assert_eq!(pool.borrow("egr").unwrap().value.expose_secret(), "v2");
}
