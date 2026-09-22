use gateway_admin::model::proxies::*;

fn base(success: bool) -> ProxyTestResult {
    ProxyTestResult {
        success,
        latency_ms: 150,
        exit_ip: success.then(|| "203.0.113.7".parse().unwrap()),
        exit_geo: success.then(|| ProxyExitGeo {
            country: "美国".to_owned(),
            country_code: "US".to_owned(),
            region: None,
            city: Some("洛杉矶".to_owned()),
        }),
        exit_ipv4: None,
        exit_ipv6: None,
        message: if success {
            "连接成功"
        } else {
            "代理连接超时"
        }
        .to_owned(),
    }
}

fn item(target: &str, status: ProxyQualityItemStatus) -> ProxyQualityItem {
    ProxyQualityItem {
        target: target.to_owned(),
        status,
        http_status: Some(401),
        latency_ms: Some(90),
        message: String::new(),
        cf_ray: None,
    }
}

fn report(success: bool, statuses: &[ProxyQualityItemStatus]) -> ProxyQualityReport {
    ProxyQualityReport::finalize(
        ProxyQualityProbe {
            base: base(success),
            items: statuses
                .iter()
                .map(|status| item("target", *status))
                .collect(),
        },
        "2026-09-21T00:00:00Z".parse().unwrap(),
    )
}

#[test]
fn score_subtracts_fixed_penalties_and_never_goes_below_zero() {
    use ProxyQualityItemStatus::{Challenge, Fail, Pass, Warn};

    let all_pass = report(true, &[Pass, Pass, Pass, Pass]);
    assert_eq!(
        (all_pass.snapshot.score, all_pass.snapshot.grade),
        (100, 'A')
    );
    assert_eq!(all_pass.snapshot.status, ProxyQualityStatus::Healthy);
    assert_eq!(all_pass.passed_count, 5);
    assert_eq!(
        all_pass.snapshot.summary,
        "通过 5 项，告警 0 项，失败 0 项，挑战 0 项"
    );

    // 100 − 10 − 22 − 30。
    let mixed = report(true, &[Pass, Warn, Fail, Challenge]);
    assert_eq!((mixed.snapshot.score, mixed.snapshot.grade), (38, 'F'));
    assert_eq!(mixed.snapshot.status, ProxyQualityStatus::Challenge);

    let floor = report(true, &[Challenge, Challenge, Challenge, Challenge]);
    assert_eq!(floor.snapshot.score, 0);
}

#[test]
fn grade_thresholds_and_overall_status_priority_are_fixed() {
    use ProxyQualityItemStatus::{Fail, Pass, Warn};

    for (score, grade) in [
        (100, 'A'),
        (90, 'A'),
        (89, 'B'),
        (75, 'B'),
        (74, 'C'),
        (60, 'C'),
        (59, 'D'),
        (40, 'D'),
        (39, 'F'),
        (0, 'F'),
    ] {
        assert_eq!(quality_grade(score), grade, "{score}");
    }
    assert_eq!(
        report(true, &[Pass, Warn]).snapshot.status,
        ProxyQualityStatus::Warn
    );
    assert_eq!(
        report(true, &[Warn, Fail]).snapshot.status,
        ProxyQualityStatus::Failed
    );
    assert_eq!(report(true, &[Warn]).snapshot.score, 90);
}

#[test]
fn failed_connectivity_drops_upstream_items_and_keeps_the_probe_message() {
    let failed = report(false, &[ProxyQualityItemStatus::Pass]);
    assert_eq!(failed.items.len(), 1);
    assert_eq!(failed.items[0].target, PROXY_QUALITY_BASE_TARGET);
    assert_eq!(failed.items[0].message, "代理连接超时");
    assert_eq!(failed.snapshot.score, 78);
    assert_eq!(failed.snapshot.status, ProxyQualityStatus::Failed);
    assert!(failed.base_latency_ms.is_none());
    assert!(failed.exit_ip.is_none());
}

#[test]
fn status_names_round_trip_for_storage() {
    for status in ["healthy", "warn", "challenge", "failed"] {
        assert_eq!(ProxyQualityStatus::parse(status).unwrap().as_str(), status);
    }
    for status in ["pass", "warn", "fail", "challenge"] {
        assert_eq!(
            ProxyQualityItemStatus::parse(status).unwrap().as_str(),
            status
        );
    }
    assert!(ProxyQualityStatus::parse("unknown").is_none());
}
