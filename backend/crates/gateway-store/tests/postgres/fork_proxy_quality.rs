//! fork：代理质量检测与出口地理（outbound_proxy_quality，fork 迁移 9002）。

use gateway_admin::{model::proxies::*, ports::proxy::ProxyStore};
use gateway_core::account::OutboundProxy;
use gateway_store::postgres::PgProxyRepository;

use super::{
    TestDatabase,
    proxies::{context, success},
};

fn geo() -> ProxyExitGeo {
    ProxyExitGeo {
        country: "美国".to_owned(),
        country_code: "US".to_owned(),
        region: Some("加州".to_owned()),
        city: None,
        timezone: Some("America/Los_Angeles".to_owned()),
    }
}

#[tokio::test]
async fn quality_reports_round_trip_refresh_connectivity_and_reset_with_the_address() {
    let Some(database) = TestDatabase::create("proxy_quality").await else {
        return;
    };
    let store = PgProxyRepository::new(database.pool.clone());
    let context = context();
    let created = store
        .create(
            NewProxy {
                auto_location: false,
                test: None,
                location: None,
                name: "Quality".to_owned(),
                proxy: OutboundProxy::parse("http://user:secret@127.0.0.1:8080").unwrap(),
            },
            &context,
        )
        .await
        .unwrap()
        .record;
    assert!(created.quality.is_none());
    assert!(store.quality_report(&created.id).await.unwrap().is_none());
    assert!(store.quality_report("proxy_missing").await.is_err());

    // 地区随成功的连通性结果保存；失败结果即使带了地区也不落库。
    let located = store
        .record_test(
            &created.id,
            created.revision,
            ProxyTestResult {
                exit_geo: Some(geo()),
                ..success()
            },
            &context,
        )
        .await
        .unwrap()
        .record;
    assert_eq!(located.last_test.unwrap().exit_geo, Some(geo()));
    let failed = store
        .record_test(
            &created.id,
            created.revision,
            ProxyTestResult {
                success: false,
                exit_ip: None,
                exit_geo: Some(geo()),
                ..success()
            },
            &context,
        )
        .await
        .unwrap()
        .record;
    assert!(failed.last_test.unwrap().exit_geo.is_none());

    let report = ProxyQualityReport::finalize(
        ProxyQualityProbe {
            base: ProxyTestResult {
                latency_ms: 321,
                exit_geo: Some(geo()),
                ..success()
            },
            items: vec![
                ProxyQualityItem {
                    target: "chatgpt".to_owned(),
                    status: ProxyQualityItemStatus::Challenge,
                    http_status: Some(403),
                    latency_ms: Some(88),
                    message: "命中 Cloudflare challenge".to_owned(),
                    cf_ray: Some("ray-1".to_owned()),
                },
                ProxyQualityItem {
                    target: "xai".to_owned(),
                    status: ProxyQualityItemStatus::Fail,
                    http_status: None,
                    latency_ms: None,
                    message: "请求超时".to_owned(),
                    cf_ray: None,
                },
            ],
        },
        "2026-09-21T01:02:03Z".parse().unwrap(),
    );
    assert!(
        store
            .record_quality(
                &created.id,
                gateway_admin::model::Revision::new(created.revision.get() + 1).unwrap(),
                report.clone(),
                &context
            )
            .await
            .is_err()
    );
    let checked = store
        .record_quality(&created.id, created.revision, report.clone(), &context)
        .await
        .unwrap();
    assert_eq!(checked.quality, Some(report.snapshot.clone()));
    // 质量检测的第一项就是一次连通性测试，二者在同一事务里前进。
    let last_test = checked.last_test.unwrap();
    assert!(last_test.success);
    assert_eq!(last_test.latency_ms, 321);
    assert_eq!(last_test.exit_geo, Some(geo()));
    assert_eq!(
        store.quality_report(&created.id).await.unwrap(),
        Some(report)
    );
    let audited: i64 = sqlx::query_scalar(
        "select count(*) from admin_audit_events where action = 'quality_check' and entity_ref = $1",
    )
    .bind(&created.id)
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(audited, 1);

    // 普通测试不清除质量结论；连接地址变化后旧结论全部失效。
    let retested = store
        .record_test(&created.id, created.revision, success(), &context)
        .await
        .unwrap()
        .record;
    assert!(retested.quality.is_some());
    let renamed = store
        .update(
            UpdateProxy {
                auto_location: None,
                test: None,
                location: None,
                id: created.id.clone(),
                revision: created.revision,
                name: "Renamed".to_owned(),
                proxy: None,
            },
            &context,
        )
        .await
        .unwrap()
        .record;
    assert!(renamed.quality.is_some());
    let moved = store
        .update(
            UpdateProxy {
                auto_location: None,
                test: None,
                location: None,
                id: created.id.clone(),
                revision: renamed.revision,
                name: "Renamed".to_owned(),
                proxy: Some(OutboundProxy::parse("http://user:secret@127.0.0.1:8081").unwrap()),
            },
            &context,
        )
        .await
        .unwrap()
        .record;
    assert!(moved.quality.is_none());
    assert!(moved.last_test.is_none());
    assert!(store.quality_report(&created.id).await.unwrap().is_none());
    database.close().await;
}
