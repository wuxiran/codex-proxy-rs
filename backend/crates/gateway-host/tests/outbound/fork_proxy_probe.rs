//! fork：代理出口地理（尽力而为）与上游目标质量检测。放在 `outbound` 下以满足测试镜像生产模块的约束。

use gateway_admin::model::proxies::ProxyQualityItemStatus;
use gateway_admin::ports::proxy::ProxyProbe;
use gateway_core::account::OutboundProxy;
use gateway_host::proxy_probe::{
    HttpProxyProbe, ProxyQualityTarget, is_cloudflare_challenge, parse_exit_geo,
};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{any, path},
};

async fn proxy_with_exit_ip() -> (MockServer, OutboundProxy) {
    let server = MockServer::start().await;
    Mock::given(path("/ip"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ip": "203.0.113.8"})))
        .mount(&server)
        .await;
    let proxy = OutboundProxy::parse(&format!("http://{}", server.address())).unwrap();
    (server, proxy)
}

#[tokio::test]
async fn exit_geo_is_best_effort_and_never_changes_the_connectivity_result() {
    let (server, proxy) = proxy_with_exit_ip().await;
    Mock::given(path("/geo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success", "country": "美国", "countryCode": "us",
            "regionName": "加州", "city": "洛杉矶"
        })))
        .mount(&server)
        .await;
    let probe = HttpProxyProbe::new("http://unresolvable.invalid/ip");
    let located = probe
        .with_geo_endpoint("http://unresolvable.invalid/geo")
        .test(&proxy, false)
        .await;
    assert!(located.success);
    let geo = located.exit_geo.unwrap();
    assert_eq!(
        (geo.country.as_str(), geo.country_code.as_str()),
        ("美国", "US")
    );
    assert_eq!(geo.city.as_deref(), Some("洛杉矶"));

    // 地区服务出错、被限流或返回垃圾时，测试仍然成功，只是没有地区。
    let broken = HttpProxyProbe::new("http://unresolvable.invalid/ip")
        .with_geo_endpoint("http://unresolvable.invalid/missing")
        .test(&proxy, false)
        .await;
    assert!(broken.success);
    assert!(broken.exit_geo.is_none());
    assert_eq!(broken.exit_ip, located.exit_ip);
}

#[test]
fn exit_geo_parser_rejects_untrusted_shapes() {
    assert!(parse_exit_geo(br#"{"status":"fail","message":"quota"}"#).is_none());
    assert!(parse_exit_geo(br#"{"status":"success","country":"X","countryCode":"USA"}"#).is_none());
    assert!(parse_exit_geo(br#"{"status":"success","country":"","countryCode":"US"}"#).is_none());
    assert!(parse_exit_geo(b"<html>").is_none());
    let long = format!(
        r#"{{"status":"success","country":"US","countryCode":"US","city":"{}"}}"#,
        "x".repeat(200)
    );
    let geo = parse_exit_geo(long.as_bytes()).unwrap();
    assert!(geo.city.is_none());
    assert!(geo.region.is_none());
    assert!(geo.timezone.is_none());

    // 带 IANA 时区时应解析出来，供按出口自动设时区使用。
    let with_tz = parse_exit_geo(
        br#"{"status":"success","country":"US","countryCode":"US","regionName":"Ohio","city":"Piketon","timezone":"America/New_York"}"#,
    )
    .unwrap();
    assert_eq!(with_tz.timezone.as_deref(), Some("America/New_York"));
    let location = gateway_core::account::RequestLocation::from_geo(
        &with_tz.country_code,
        with_tz.region.as_deref(),
        with_tz.city.as_deref(),
        with_tz.timezone.as_deref().unwrap(),
    )
    .unwrap();
    assert_eq!(location.timezone, chrono_tz::America::New_York);
    assert_eq!(location.country, "US");
}

#[tokio::test]
async fn quality_probe_classifies_each_target_independently() {
    let (server, proxy) = proxy_with_exit_ip().await;
    Mock::given(path("/reachable"))
        .respond_with(ResponseTemplate::new(401).set_body_string("{}"))
        .mount(&server)
        .await;
    Mock::given(path("/challenge"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("cf-mitigated", "challenge")
                .insert_header("cf-ray", "8f1c2d3e4a5b6c7d-LAX")
                .set_body_string("<html>Just a moment...</html>"),
        )
        .mount(&server)
        .await;
    Mock::given(path("/limited"))
        .respond_with(ResponseTemplate::new(429).set_body_string("slow down"))
        .mount(&server)
        .await;
    Mock::given(path("/broken"))
        .respond_with(ResponseTemplate::new(502))
        .mount(&server)
        .await;
    Mock::given(path("/forbidden"))
        .respond_with(ResponseTemplate::new(403).set_body_string(r#"{"detail":"forbidden"}"#))
        .mount(&server)
        .await;
    let target = |name: &str, statuses: &[u16]| {
        ProxyQualityTarget::new(
            name,
            &format!("http://unresolvable.invalid/{name}"),
            statuses,
        )
    };
    let result = HttpProxyProbe::new("http://unresolvable.invalid/ip")
        .with_quality_targets(vec![
            target("reachable", &[401]),
            target("challenge", &[401]),
            target("limited", &[401]),
            target("broken", &[401]),
            target("forbidden", &[401]),
        ])
        .quality(&proxy)
        .await;
    assert!(result.base.success);
    let statuses: Vec<_> = result.items.iter().map(|item| item.status).collect();
    assert_eq!(
        statuses,
        [
            ProxyQualityItemStatus::Pass,
            ProxyQualityItemStatus::Challenge,
            ProxyQualityItemStatus::Warn,
            ProxyQualityItemStatus::Fail,
            ProxyQualityItemStatus::Fail,
        ]
    );
    assert_eq!(result.items[0].http_status, Some(401));
    assert_eq!(
        result.items[1].cf_ray.as_deref(),
        Some("8f1c2d3e4a5b6c7d-LAX")
    );
    assert_eq!(result.items[3].message, "非预期状态码: 502");
    assert!(result.items.iter().all(|item| item.latency_ms.is_some()));
}

#[tokio::test]
async fn quality_probe_skips_targets_when_the_proxy_is_unreachable() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let proxy = OutboundProxy::parse(&format!("http://{address}")).unwrap();
    // 目标指向一个真实监听的服务：如果探测绕过代理直连，这里就会收到请求。
    let direct = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(401))
        .expect(0)
        .mount(&direct)
        .await;
    let result = HttpProxyProbe::new(format!("{}/ip", direct.uri()))
        .with_quality_targets(vec![ProxyQualityTarget::new(
            "direct",
            &format!("{}/models", direct.uri()),
            &[401],
        )])
        .quality(&proxy)
        .await;
    assert!(!result.base.success);
    assert!(result.items.is_empty());
}

#[test]
fn cloudflare_challenge_needs_a_blocking_status_and_a_challenge_signal() {
    assert!(is_cloudflare_challenge(403, Some("challenge"), None, ""));
    assert!(is_cloudflare_challenge(
        429,
        None,
        Some("text/html"),
        "<title>Just a moment...</title>"
    ));
    assert!(is_cloudflare_challenge(
        403,
        None,
        None,
        "window._cf_chl_opt = {}"
    ));
    assert!(is_cloudflare_challenge(
        403,
        None,
        Some("text/html; charset=UTF-8"),
        "<html>Cloudflare security challenge</html>"
    ));
    // 普通的 403 / 429 业务响应不是挑战；200 即使带挑战头也不算。
    assert!(!is_cloudflare_challenge(
        403,
        None,
        Some("application/json"),
        r#"{"detail":"forbidden"}"#
    ));
    assert!(!is_cloudflare_challenge(
        429,
        None,
        Some("application/json"),
        "rate limited"
    ));
    assert!(!is_cloudflare_challenge(
        200,
        Some("challenge"),
        None,
        "just a moment"
    ));
}
