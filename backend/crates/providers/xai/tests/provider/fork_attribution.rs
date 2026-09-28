//! fork：归因记录实际出口（metadata 在任何推理前投影所选代理，且不泄露凭据）。

use std::sync::Arc;
use std::sync::atomic::Ordering;

use gateway_core::engine::provider::Provider as _;
use gateway_core::lifecycle::CancellationToken;

use super::contract::{StubInferenceTransport, StubSelector, context, provider, provider_request};

#[tokio::test]
async fn attribution_metadata_projects_selected_proxy_before_any_inference() {
    for url in [
        None,
        Some("http://synthetic-user:synthetic-secret@proxy.example:8080"),
    ] {
        let mut selector = StubSelector::success();
        Arc::get_mut(&mut selector).unwrap().proxy =
            url.map(|url| gateway_core::account::OutboundProxy::parse(url).unwrap());
        let transport = StubInferenceTransport::success();
        let provider = provider(selector, transport.clone()).await;
        let stream = provider
            .execute(
                provider_request("xai"),
                context(CancellationToken::new(), None),
            )
            .await
            .unwrap();
        assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            stream.metadata().outbound_proxy_endpoint(),
            Some(if url.is_some() {
                "http://proxy.example:8080/"
            } else {
                "direct"
            })
        );
        assert!(!format!("{:?}", stream.metadata()).contains("synthetic"));
    }
}
