//! fork：账号用例测试替身的 fork 状态与钩子，以及 fork 增补的账号用例测试。
//!
//! 上游替身只多一个 `fork` 字段和几行 `// fork:` 钩子调用；fork 专用的状态、断言辅助与
//! 测试都放在这里，合并上游时 `accounts.rs` 可直接取上游再补回标记行。

use std::sync::Mutex;

use chrono::{TimeDelta, Utc};
use gateway_admin::model::{
    accounts::{AccountListQuery, AccountRecord, AccountUsageWindowResult, BatchUpdateAccounts},
    provider_credentials::{PrepareCredentialImport, PreparedCredentialCreate},
    proxies::AccountProxySelection,
};
use gateway_core::account::{OutboundProxy, ProviderAccountId};

use super::accounts::{
    FakeAccountStore, FakeProviderAdmin, account_record, accounts_service, events,
    quota_local_usage,
};

/// `FakeProviderAdmin` 的 fork 状态：免登录导入与遍历代理找 state。
pub(super) struct FakeProviderAdminFork {
    import_authentication_kind: Mutex<String>,
    import_documents: Mutex<Vec<serde_json::Value>>,
    /// `Some` 时支持遍历代理找 state；值是当前凭据绑定，测试可中途改写模拟凭据刷新。
    pub(super) hunt_binding: Mutex<Option<String>>,
    /// 续期任务每个周期看到的到期账号。
    pub(super) hunt_renewals: Mutex<Vec<gateway_admin::ports::provider::TurnStateRenewal>>,
    /// 最近一次钉住 state 时传入的出口（外层 `None` = 还没钉过；内层 `None` = 直连）。
    pub(super) hunt_pinned_egress: Mutex<Option<Option<String>>>,
}

impl Default for FakeProviderAdminFork {
    fn default() -> Self {
        Self {
            import_authentication_kind: Mutex::new("oauth".to_owned()),
            import_documents: Mutex::new(Vec::new()),
            hunt_binding: Mutex::new(None),
            hunt_renewals: Mutex::new(Vec::new()),
            hunt_pinned_egress: Mutex::new(None),
        }
    }
}

impl FakeProviderAdminFork {
    /// 记录 Provider 收到的导入文档，供断言入口改写后的内容。
    pub(super) fn record_import_document(&self, command: &PrepareCredentialImport) {
        self.import_documents
            .lock()
            .expect("provider import documents")
            .push(serde_json::Value::Object(
                command
                    .document
                    .expose_to_provider()
                    .expose_to_provider()
                    .clone(),
            ));
    }

    /// 按测试设定的认证类型准备导入结果（免登录入口只收 OAuth）。
    pub(super) fn with_import_authentication_kind(
        &self,
        create: PreparedCredentialCreate,
    ) -> PreparedCredentialCreate {
        PreparedCredentialCreate {
            authentication_kind: self
                .import_authentication_kind
                .lock()
                .expect("import authentication kind")
                .clone(),
            ..create
        }
    }
}

impl FakeProviderAdmin {
    /// 依次返回 Provider 收到的导入文档，供断言入口改写后的内容。
    pub(super) fn import_documents(&self) -> Vec<serde_json::Value> {
        self.fork
            .import_documents
            .lock()
            .expect("provider import documents")
            .clone()
    }

    pub(super) fn set_import_authentication_kind(&self, kind: &str) {
        *self
            .fork
            .import_authentication_kind
            .lock()
            .expect("import authentication kind") = kind.to_owned();
    }
}

/// `FakeAccountStore` 的 fork 状态：批量改绑出口时解析已保存代理。
#[derive(Default)]
pub(super) struct FakeAccountStoreFork {
    /// 已保存代理 ID 到地址的解析表；批量更新选中其中之一时账号的出口随之改变。
    pub(super) saved_proxies: Mutex<std::collections::BTreeMap<String, OutboundProxy>>,
}

impl FakeAccountStoreFork {
    /// 批量更新带出口选择时同步改写账号出口。
    pub(super) fn apply_outbound_proxy(
        &self,
        command: &BatchUpdateAccounts,
        accounts: &Mutex<Vec<AccountRecord>>,
    ) {
        let Some(selection) = &command.outbound_proxy else {
            return;
        };
        let proxy = match selection {
            AccountProxySelection::Direct => None,
            AccountProxySelection::Url(proxy) => Some(proxy.clone()),
            AccountProxySelection::Saved(id) => {
                self.saved_proxies.lock().expect("proxies").get(id).cloned()
            }
        };
        // 解析不到的已保存代理保持原绑定：模拟「提交看似成功、账号却没换到该出口」。
        if !matches!(selection, AccountProxySelection::Saved(_)) || proxy.is_some() {
            for account in accounts.lock().expect("accounts").iter_mut() {
                if command.account_ids.contains(&account.id) {
                    account.outbound_proxy = proxy.clone();
                }
            }
        }
    }
}

#[tokio::test]
async fn unobserved_quota_should_preserve_local_cost_in_list_detail_and_refresh() {
    let mut account = account_record("openai");
    account.created_at = Utc::now() - TimeDelta::days(60);
    let added_at = account.created_at;
    let store = FakeAccountStore::with_account(account, events());
    let account_id = ProviderAccountId::new("acct_test").unwrap();

    // 金额未知、已知零和已有消费都不依赖上游额度；统计范围也不能缩为最近 24 小时。
    // 列表的用量投影按 service 实例做短 TTL 缓存，每种金额用新实例，避免读到上一轮。
    for amount in [None, Some("0"), Some("12.34")] {
        let provider = FakeProviderAdmin::new("openai", events());
        let services = accounts_service(provider, store.clone()).await;
        let mut expected = quota_local_usage("acct_test", 4_330_000);
        expected.costs = amount
            .map(|value| gateway_admin::model::accounts::AccountCost {
                currency: "USD".to_owned(),
                amount: value.parse().unwrap(),
            })
            .into_iter()
            .collect();
        store.set_quota_window_usage(vec![AccountUsageWindowResult {
            account_id: account_id.to_string(),
            key: "account-lifetime".to_owned(),
            usage: expected.clone(),
        }]);
        let page = services
            .accounts()
            .list(AccountListQuery {
                page: 1,
                page_size: gateway_admin::model::PageSize::new(20).unwrap(),
                provider_kind: None,
                group_filter: None,
                search: None,
                status: None,
                sort: None,
            })
            .await
            .unwrap();
        let detail = services.accounts().quota(&account_id, false).await.unwrap();
        let refreshed = services.accounts().quota(&account_id, true).await.unwrap();
        for item in [&page.items[0], &detail, &refreshed] {
            assert_eq!(item.usage.as_ref(), Some(&expected));
            assert!(item.quota.windows.is_empty());
        }
        let queries = store.quota_window_queries();
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].range.start, added_at);
        assert!(queries[0].range.end > added_at + TimeDelta::days(59));
    }
}
