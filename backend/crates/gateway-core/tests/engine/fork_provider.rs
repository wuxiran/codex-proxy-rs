//! fork：Provider 调用元数据的出口归属投影（凭据脱敏、直连与未知区分）。

use gateway_core::account::{OutboundProxy, ProviderAccountId};
use gateway_core::engine::provider::ProviderCallMetadata;
use gateway_core::routing::{ProviderKind, UpstreamModelId};
use gateway_core::upstream::UpstreamTransport;

#[test]
fn provider_call_metadata_redacts_proxy_credentials_and_distinguishes_direct_from_unknown() {
    let metadata = ProviderCallMetadata::new(
        ProviderKind::new("openai").unwrap(),
        UpstreamModelId::new("gpt-5.6-luna").unwrap(),
        ProviderAccountId::new("acct_proxy").unwrap(),
        UpstreamTransport::new("http_sse").unwrap(),
    );
    assert!(metadata.outbound_proxy_endpoint().is_none());
    assert_eq!(
        metadata
            .clone()
            .with_outbound_proxy(None)
            .outbound_proxy_endpoint(),
        Some("direct")
    );
    for scheme in ["http", "https", "socks5", "socks5h"] {
        let proxy = OutboundProxy::parse(&format!(
            "{scheme}://synthetic-user:synthetic%40secret@[::1]:1080"
        ))
        .unwrap();
        let projected = metadata.clone().with_outbound_proxy(Some(&proxy));
        assert_eq!(
            projected.outbound_proxy_endpoint(),
            Some(
                format!(
                    "{scheme}://[::1]:1080{}",
                    if scheme.starts_with("http") { "/" } else { "" }
                )
                .as_str()
            )
        );
        let debug = format!("{projected:?}");
        assert!(!debug.contains("synthetic"));
        assert!(!debug.contains("%40secret"));
    }
}
