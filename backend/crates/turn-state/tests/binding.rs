use turn_state::{credential_binding, egress_fingerprint};

#[test]
fn binding_changes_with_generation_or_token_and_never_echoes_them() {
    let a = credential_binding("gen", "token");
    assert_eq!(a, credential_binding("gen", "token"));
    assert_ne!(a, credential_binding("gen2", "token"));
    assert_ne!(a, credential_binding("gen", "token2"));
    assert_eq!(a.len(), 64);
    assert!(!a.contains("token"));
}

#[test]
fn egress_fingerprint_hides_the_proxy_url_and_distinguishes_direct() {
    let direct = egress_fingerprint(None);
    let proxied = egress_fingerprint(Some("socks5h://user:pw@host:1080"));
    assert_ne!(direct, proxied);
    assert!(!proxied.contains("pw"));
    assert_eq!(direct, egress_fingerprint(Some("direct")));
}
