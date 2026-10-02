use async_trait::async_trait;
use gateway_admin::{
    model::{MutationContext, Revision, proxies::*},
    ports::{
        proxy::{ProxyProbe, ProxyStore},
        store::AdminStoreResult,
    },
};
use gateway_core::account::{OutboundProxy, ProviderAccountId};

#[derive(Default)]
pub(super) struct TestProxies {
    pub events: Option<super::accounts::EventLog>,
    pub accounts: Option<Vec<ProxyAccountRef>>,
    /// 设置后 `list`/`get` 返回这些记录；可在测试途中改写，模拟代理被编辑。
    pub records: Option<std::sync::Arc<std::sync::Mutex<Vec<ProxyRecord>>>>,
    /// 记录每次导入预留的代理 ID。
    pub reserved: Option<std::sync::Arc<std::sync::Mutex<Vec<String>>>>,
    /// 上游用例的单记录 double：`records` 未设时 `get` 回退到它。
    pub record: Option<ProxyRecord>,
}

struct ImportGuard(super::accounts::EventLog);

impl gateway_admin::ports::proxy::ProxyImportGuard for ImportGuard {}

impl Drop for ImportGuard {
    fn drop(&mut self) {
        self.0.lock().unwrap().push("proxy.release");
    }
}

#[async_trait]
impl ProxyStore for TestProxies {
    async fn remove_account(
        &self,
        _: &str,
        _: &ProviderAccountId,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(super::unavailable("proxy"))
    }

    async fn reserve_import(
        &self,
        id: &str,
    ) -> AdminStoreResult<gateway_admin::ports::proxy::ProxyImportReservation> {
        let events = self
            .events
            .as_ref()
            .ok_or_else(|| super::unavailable("proxy"))?;
        events.lock().unwrap().push("proxy.reserve");
        if let Some(reserved) = &self.reserved {
            reserved.lock().unwrap().push(id.to_owned());
        }
        Ok(gateway_admin::ports::proxy::ProxyImportReservation {
            binding: ImportProxyBinding {
                id: id.to_owned(),
                proxy: OutboundProxy::parse("http://127.0.0.1:8080").unwrap(),
            },
            guard: Box::new(ImportGuard(events.clone())),
        })
    }
    async fn list(&self, query: ProxyListQuery) -> AdminStoreResult<ProxyPage> {
        let items = self
            .records
            .as_ref()
            .map(|records| records.lock().unwrap().clone())
            .ok_or_else(|| super::unavailable("proxy"))?;
        Ok(ProxyPage {
            total: items.len() as u64,
            items: if query.page == 1 { items } else { Vec::new() },
            page: query.page,
            page_size: query.page_size.get(),
        })
    }
    async fn list_accounts(
        &self,
        query: ProxyAccountListQuery,
    ) -> AdminStoreResult<ProxyAccountPage> {
        let items = self
            .accounts
            .clone()
            .ok_or_else(|| super::unavailable("proxy"))?;
        Ok(ProxyAccountPage {
            total: items.len() as u64,
            items,
            page: query.page,
            page_size: query.page_size.get(),
        })
    }
    async fn get(&self, id: &str) -> AdminStoreResult<ProxyRecord> {
        self.records
            .as_ref()
            .and_then(|records| {
                records
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|record| record.id == id)
                    .cloned()
            })
            .or_else(|| self.record.clone().filter(|record| record.id == id))
            .ok_or_else(|| super::unavailable("proxy"))
    }
    async fn create(&self, _: NewProxy, _: &MutationContext) -> AdminStoreResult<ProxyMutation> {
        Err(super::unavailable("proxy"))
    }
    async fn update(&self, _: UpdateProxy, _: &MutationContext) -> AdminStoreResult<ProxyMutation> {
        Err(super::unavailable("proxy"))
    }
    async fn delete(
        &self,
        _: &str,
        _: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(super::unavailable("proxy"))
    }
    async fn record_test(
        &self,
        _: &str,
        _: Revision,
        _: ProxyTestResult,
        _: &MutationContext,
    ) -> AdminStoreResult<ProxyRecord> {
        Err(super::unavailable("proxy"))
    }
    async fn record_quality(
        &self,
        _: &str,
        _: Revision,
        _: ProxyQualityReport,
        _: &MutationContext,
    ) -> AdminStoreResult<ProxyRecord> {
        Err(super::unavailable("proxy"))
    }
    async fn quality_report(&self, _: &str) -> AdminStoreResult<Option<ProxyQualityReport>> {
        Err(super::unavailable("proxy"))
    }
}

#[async_trait]
impl ProxyProbe for TestProxies {
    async fn test(&self, _: &OutboundProxy) -> ProxyTestResult {
        panic!("unexpected proxy probe")
    }
}

#[tokio::test]
async fn authorization_uses_selected_proxy_regardless_of_probe_status() {
    use super::accounts::{FakeAccountStore, FakeProviderAdmin, context, events, recorded};
    use gateway_admin::model::provider_credentials::StartAuthorization;
    use std::sync::Arc;

    for kind in ["openai", "xai"] {
        for probe_success in [None, Some(false), Some(true)] {
            let events = events();
            let now = chrono::Utc::now();
            let services = super::AdminHarness::new()
                .provider(FakeProviderAdmin::new(kind, events.clone()))
                .accounts(FakeAccountStore::new(kind, events.clone()))
                .proxies(Arc::new(TestProxies {
                    record: Some(ProxyRecord {
                        location: None,
                        id: "proxy_oauth".to_owned(),
                        name: "授权出口".to_owned(),
                        proxy: OutboundProxy::parse("http://127.0.0.1:8080").unwrap(),
                        revision: Revision::new(1).unwrap(),
                        account_count: 0,
                        last_test_at: probe_success.map(|_| now),
                        last_test: probe_success.map(|success| ProxyTestResult {
                            success,
                            latency_ms: 10,
                            exit_ip: None,
                            exit_geo: None,
                            exit_ipv4: None,
                            exit_ipv6: None,
                            message: "出口探测结果".to_owned(),
                        }),
                        quality: None,
                        created_at: now,
                        updated_at: now,
                    }),
                    ..Default::default()
                }))
                .build()
                .await;
            let command = StartAuthorization {
                outbound_proxy: Some(AccountProxySelection::Saved("proxy_oauth".to_owned())),
                context: context("oauth-proxy-status"),
                name: "授权账号".to_owned(),
                reauthorization: None,
            };
            let result = if kind == "openai" {
                services.openai().start_authorization(command).await
            } else {
                services.xai().start_authorization(command).await
            };
            assert!(result.is_ok(), "{kind}, {probe_success:?}: {result:?}");
            assert_eq!(recorded(&events), ["provider.start_authorization"]);
        }
    }
}

#[tokio::test]
async fn linked_accounts_share_plan_resolution_and_only_read_cached_quota() {
    use super::accounts::{FakeProviderAdmin, events};
    use gateway_admin::model::{PageSize, provider_credentials::ProviderQuota};
    use std::sync::Arc;

    for (stored, cached, expected) in [
        (None, Some("free"), Some("free")),
        (Some("unknown"), Some("free"), Some("free")),
        (Some("plus"), Some("free"), Some("plus")),
        (None, None, None),
    ] {
        let provider = FakeProviderAdmin::new("openai", events());
        provider.set_quota(ProviderQuota {
            plan_type: cached.map(str::to_owned),
            observed_at: None,
            refresh_token_expires_at: None,
            windows: vec![],
            limit_reached: false,
            provider_data: None,
        });
        let services = super::AdminHarness::new()
            .provider(provider.clone())
            .proxies(Arc::new(TestProxies {
                accounts: Some(vec![ProxyAccountRef {
                    id: "acct_plan".to_owned(),
                    name: "套餐测试".to_owned(),
                    email: None,
                    provider_kind: "openai".to_owned(),
                    authentication_kind: "oauth".to_owned(),
                    plan_type: stored.map(str::to_owned),
                    plan_type_display: None,
                    groups: vec![],
                    enabled: true,
                }]),
                ..Default::default()
            }))
            .build()
            .await;
        let result = services
            .proxies()
            .list_accounts(ProxyAccountListQuery {
                proxy_id: "proxy_plan".to_owned(),
                page: 1,
                page_size: PageSize::new(20).unwrap(),
                search: String::new(),
            })
            .await
            .unwrap();
        assert_eq!(result.items[0].plan_type.as_deref(), expected);
        assert_eq!(
            result.items[0].plan_type_display.as_deref(),
            match expected {
                Some("free") => Some("OpenaiDisplayFree"),
                Some("plus") => Some("OpenaiDisplayPlus"),
                _ => None,
            }
        );
        let requests = provider.quota_requests();
        assert_eq!(requests.len(), usize::from(stored != Some("plus")));
        assert!(requests.iter().all(|request| !request.refresh));
    }
}

#[tokio::test]
async fn credential_import_keeps_proxy_reserved_until_commit_and_releases_on_errors() {
    use super::accounts::{
        FakeAccountStore, FakeProviderAdmin, context, document, events, recorded,
    };
    use gateway_admin::model::provider_credentials::ImportCredentials;
    use gateway_admin::ports::provider::ProviderAdminErrorKind;
    use std::sync::Arc;

    for kind in ["openai", "xai"] {
        for failure in [None, Some("prepare"), Some("commit")] {
            let events = events();
            let provider = FakeProviderAdmin::new(kind, events.clone());
            let store = FakeAccountStore::new(kind, events.clone());
            if failure == Some("prepare") {
                provider.fail_next(ProviderAdminErrorKind::Unavailable);
            }
            if failure == Some("commit") {
                store.fail_next_commit();
            }
            let services = super::AdminHarness::new()
                .provider(provider)
                .accounts(store)
                .proxies(Arc::new(TestProxies {
                    events: Some(events.clone()),
                    ..Default::default()
                }))
                .build()
                .await;
            let command = ImportCredentials {
                outbound_proxy_id: Some("proxy_import".to_owned()),
                settings: None,
                context: context("reserved-import"),
                document: document(),
            };
            let result = if kind == "openai" {
                services.openai().import_document(command).await
            } else {
                services.xai().import_document(command).await
            };
            assert_eq!(result.is_err(), failure.is_some());
            let events = recorded(&events);
            let expected = if failure == Some("prepare") {
                vec!["proxy.reserve", "provider.prepare_import", "proxy.release"]
            } else {
                vec![
                    "proxy.reserve",
                    "provider.prepare_import",
                    "store.commit_import",
                    "proxy.release",
                ]
            };
            assert_eq!(&events[..expected.len()], expected);
        }
    }
}

/// 内存代理目录：URL 去重、在用代理拒绝删除、按修订号记录质量结果。
#[derive(Default)]
struct MemoryProxies {
    records: std::sync::Mutex<Vec<ProxyRecord>>,
    in_use: Vec<String>,
    probes: std::sync::atomic::AtomicUsize,
    block_probe: bool,
}

fn memory_record(id: &str, url: &str) -> ProxyRecord {
    ProxyRecord {
        location: None,
        id: id.to_owned(),
        name: id.to_owned(),
        proxy: OutboundProxy::parse(url).unwrap(),
        revision: Revision::new(1).unwrap(),
        account_count: 0,
        last_test_at: None,
        last_test: None,
        quality: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn store_error(
    kind: gateway_admin::ports::store::AdminStoreErrorKind,
) -> gateway_admin::ports::store::AdminStoreError {
    gateway_admin::ports::store::AdminStoreError::new(kind, "proxy", "memory proxy store")
}

#[async_trait]
impl ProxyStore for MemoryProxies {
    async fn reserve_import(
        &self,
        _: &str,
    ) -> AdminStoreResult<gateway_admin::ports::proxy::ProxyImportReservation> {
        Err(super::unavailable("proxy"))
    }
    async fn list(&self, _: ProxyListQuery) -> AdminStoreResult<ProxyPage> {
        Err(super::unavailable("proxy"))
    }
    async fn list_accounts(&self, _: ProxyAccountListQuery) -> AdminStoreResult<ProxyAccountPage> {
        Err(super::unavailable("proxy"))
    }
    async fn get(&self, id: &str) -> AdminStoreResult<ProxyRecord> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .find(|record| record.id == id)
            .cloned()
            .ok_or_else(|| store_error(gateway_admin::ports::store::AdminStoreErrorKind::NotFound))
    }
    async fn remove_account(
        &self,
        _: &str,
        _: &ProviderAccountId,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(super::unavailable("proxy"))
    }
    async fn create(
        &self,
        command: NewProxy,
        _: &MutationContext,
    ) -> AdminStoreResult<ProxyMutation> {
        let mut records = self.records.lock().unwrap();
        if records.iter().any(|record| record.proxy == command.proxy) {
            return Err(store_error(
                gateway_admin::ports::store::AdminStoreErrorKind::Conflict,
            ));
        }
        let mut record = memory_record(
            &format!("proxy_{}", records.len() + 1),
            command.proxy.expose_url(),
        );
        record.name = command.name;
        records.push(record.clone());
        Ok(ProxyMutation {
            config_revision: Revision::new(records.len() as u64 + 10).unwrap(),
            record,
        })
    }
    async fn update(&self, _: UpdateProxy, _: &MutationContext) -> AdminStoreResult<ProxyMutation> {
        Err(super::unavailable("proxy"))
    }
    async fn delete(
        &self,
        id: &str,
        revision: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        let mut records = self.records.lock().unwrap();
        let index = records
            .iter()
            .position(|record| record.id == id)
            .ok_or_else(|| {
                store_error(gateway_admin::ports::store::AdminStoreErrorKind::NotFound)
            })?;
        if self.in_use.iter().any(|used| used == id) || records[index].revision != revision {
            return Err(store_error(
                gateway_admin::ports::store::AdminStoreErrorKind::Conflict,
            ));
        }
        records.remove(index);
        Ok(Revision::new(100 - records.len() as u64).unwrap())
    }
    async fn record_test(
        &self,
        _: &str,
        _: Revision,
        _: ProxyTestResult,
        _: &MutationContext,
    ) -> AdminStoreResult<ProxyRecord> {
        Err(super::unavailable("proxy"))
    }
    async fn record_quality(
        &self,
        id: &str,
        _: Revision,
        report: ProxyQualityReport,
        _: &MutationContext,
    ) -> AdminStoreResult<ProxyRecord> {
        let mut records = self.records.lock().unwrap();
        let record = records
            .iter_mut()
            .find(|record| record.id == id)
            .ok_or_else(|| {
                store_error(gateway_admin::ports::store::AdminStoreErrorKind::NotFound)
            })?;
        record.quality = Some(report.snapshot);
        Ok(record.clone())
    }
    async fn quality_report(&self, _: &str) -> AdminStoreResult<Option<ProxyQualityReport>> {
        Ok(None)
    }
}

#[async_trait]
impl ProxyProbe for MemoryProxies {
    async fn test(&self, _: &OutboundProxy) -> ProxyTestResult {
        self.probes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.block_probe {
            std::future::pending::<()>().await;
        }
        ProxyTestResult {
            success: true,
            latency_ms: 120,
            exit_ip: Some("203.0.113.9".parse().unwrap()),
            exit_geo: None,
            exit_ipv4: None,
            exit_ipv6: None,
            message: "连接成功".to_owned(),
        }
    }

    async fn quality(&self, proxy: &OutboundProxy) -> ProxyQualityProbe {
        ProxyQualityProbe {
            base: self.test(proxy).await,
            items: vec![ProxyQualityItem {
                target: "chatgpt".to_owned(),
                status: ProxyQualityItemStatus::Challenge,
                http_status: Some(403),
                latency_ms: Some(80),
                message: "命中 Cloudflare challenge".to_owned(),
                cf_ray: Some("ray-1".to_owned()),
            }],
        }
    }
}

async fn memory_services(proxies: std::sync::Arc<MemoryProxies>) -> gateway_admin::AdminServices {
    super::AdminHarness::new()
        .proxies(proxies.clone())
        .proxy_probe(proxies)
        .build()
        .await
}

#[tokio::test]
async fn quality_check_scores_the_probe_and_rejects_stale_revisions_before_probing() {
    use super::accounts::context;
    use gateway_admin::model::AdminErrorKind;
    use std::sync::{Arc, atomic::Ordering};

    let proxies = Arc::new(MemoryProxies::default());
    proxies
        .records
        .lock()
        .unwrap()
        .push(memory_record("proxy_a", "http://127.0.0.1:8080"));
    let services = memory_services(proxies.clone()).await;

    let stale = services
        .proxies()
        .quality_check("proxy_a", Revision::new(2).unwrap(), &context("stale"))
        .await
        .unwrap_err();
    assert_eq!(stale.kind(), AdminErrorKind::Conflict);
    assert_eq!(proxies.probes.load(Ordering::SeqCst), 0);

    let outcome = services
        .proxies()
        .quality_check("proxy_a", Revision::new(1).unwrap(), &context("check"))
        .await
        .unwrap();
    assert_eq!(outcome.report.snapshot.score, 70);
    assert_eq!(outcome.report.snapshot.grade, 'C');
    assert_eq!(
        outcome.report.snapshot.status,
        ProxyQualityStatus::Challenge
    );
    assert_eq!(outcome.report.items[0].target, PROXY_QUALITY_BASE_TARGET);
    assert_eq!(outcome.record.quality, Some(outcome.report.snapshot));
}

#[tokio::test]
async fn batch_create_and_delete_skip_individual_conflicts_without_failing_the_batch() {
    use super::accounts::context;
    use std::sync::Arc;

    let proxies = Arc::new(MemoryProxies {
        in_use: vec!["proxy_1".to_owned()],
        ..Default::default()
    });
    let services = memory_services(proxies.clone()).await;
    let new_proxy = |name: &str, url: &str| NewProxy {
        location: None,
        name: name.to_owned(),
        proxy: OutboundProxy::parse(url).unwrap(),
    };
    let created = services
        .proxies()
        .create_batch(
            vec![
                new_proxy("a", "http://user:secret@10.0.0.1:8080"),
                new_proxy("dup", "http://user:secret@10.0.0.1:8080"),
                new_proxy(" ", "socks5://10.0.0.2:1080"),
                new_proxy("b", "socks5h://10.0.0.3:1080"),
            ],
            &context("batch-create"),
        )
        .await
        .unwrap();
    assert_eq!(created.created.len(), 2);
    assert_eq!(created.config_revision, Some(Revision::new(12).unwrap()));
    assert_eq!(created.skipped.len(), 2);
    assert_eq!(created.skipped[0].reason, "代理地址已存在");
    // 跳过项只回显脱敏端点，凭据不随结果返回。
    assert!(
        created
            .skipped
            .iter()
            .all(|skip| !skip.reference.contains("secret"))
    );

    let item = |id: &str, revision| ProxyBatchDeleteItem {
        id: id.to_owned(),
        revision: Revision::new(revision).unwrap(),
    };
    let deleted = services
        .proxies()
        .delete_batch(
            vec![
                item("proxy_1", 1),
                item("proxy_2", 1),
                item("proxy_missing", 1),
            ],
            &context("batch-delete"),
        )
        .await
        .unwrap();
    assert_eq!(deleted.deleted_ids, ["proxy_2"]);
    assert_eq!(deleted.skipped.len(), 2);
    assert!(deleted.config_revision.is_some());

    assert!(
        services
            .proxies()
            .create_batch(vec![], &context("empty"))
            .await
            .is_err()
    );
}

#[tokio::test(start_paused = true)]
async fn busy_test_slots_queue_briefly_then_report_rate_limited() {
    use gateway_admin::model::AdminErrorKind;
    use std::sync::{Arc, atomic::Ordering};

    let proxies = Arc::new(MemoryProxies {
        block_probe: true,
        ..Default::default()
    });
    let services = Arc::new(memory_services(proxies.clone()).await);
    let proxy = OutboundProxy::parse("http://127.0.0.1:8080").unwrap();
    let mut running = Vec::new();
    for _ in 0..4 {
        let services = services.clone();
        let proxy = proxy.clone();
        running.push(tokio::spawn(async move {
            services.proxies().probe(&proxy).await
        }));
    }
    while proxies.probes.load(Ordering::SeqCst) < 4 {
        tokio::task::yield_now().await;
    }
    // 四个槽位都被占用：第五个请求排队到超时才被拒绝，而不是立刻 429。
    let started = tokio::time::Instant::now();
    let error = services.proxies().probe(&proxy).await.unwrap_err();
    assert_eq!(error.kind(), AdminErrorKind::RateLimited);
    assert!(started.elapsed() >= std::time::Duration::from_secs(20));
    assert_eq!(proxies.probes.load(Ordering::SeqCst), 4);
    for task in running {
        task.abort();
    }
}
