//! fork：从未过滤的 Set-Cookie 头读 `__cf_bm` 的签发 TTL（短 = 降智节点特征）。

use provider_openai::cfbm_issued_ttl_seconds;

#[test]
fn parses_max_age_short_and_long() {
    let short =
        vec!["__cf_bm=abc.def; Path=/; Max-Age=120; Domain=.chatgpt.com; HttpOnly".to_owned()];
    assert_eq!(cfbm_issued_ttl_seconds(&short), Some(120));
    let long = vec!["__cf_bm=xyz; Max-Age=1800; Path=/".to_owned()];
    assert_eq!(cfbm_issued_ttl_seconds(&long), Some(1800));
}

#[test]
fn ignores_non_cfbm_and_missing() {
    let other = vec![
        "_cfuvid=zzz; Max-Age=600".to_owned(),
        "cf_clearance=q; Max-Age=999".to_owned(),
    ];
    assert_eq!(cfbm_issued_ttl_seconds(&other), None);
    assert_eq!(cfbm_issued_ttl_seconds(&[]), None);
    // 有 __cf_bm 但既无 Max-Age 也无 Expires → None（不瞎猜）。
    let no_ttl = vec!["__cf_bm=abc; Path=/; HttpOnly".to_owned()];
    assert_eq!(cfbm_issued_ttl_seconds(&no_ttl), None);
}
