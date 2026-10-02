use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::{StreamExt, stream};
use gateway_core::{
    account::{OutboundProxy, ProviderAccountId},
    routing::ProviderKind,
    runtime::SnapshotControl,
};
use tokio::sync::{Semaphore, SemaphorePermit};

use super::{map_store_error, publish_committed};
use crate::{
    model::{
        AdminError, AdminErrorKind, MutationContext, Revision,
        provider_credentials::{ProviderQuotaRequest, explicit_plan_type},
        proxies::*,
    },
    ports::{
        provider::ProviderAdminRegistry,
        proxy::{ProxyProbe, ProxyStore},
        store::AdminStoreErrorKind,
    },
};

#[async_trait]
pub trait ProxiesService: Send + Sync {
    async fn list(&self, query: ProxyListQuery) -> Result<ProxyPage, AdminError>;
    async fn list_accounts(
        &self,
        query: ProxyAccountListQuery,
    ) -> Result<ProxyAccountPage, AdminError>;
    async fn remove_account(
        &self,
        proxy_id: &str,
        account_id: &str,
        context: &MutationContext,
    ) -> Result<Revision, AdminError>;
    async fn create(
        &self,
        command: NewProxy,
        context: &MutationContext,
    ) -> Result<ProxyMutation, AdminError>;
    async fn update(
        &self,
        command: UpdateProxy,
        context: &MutationContext,
    ) -> Result<ProxyMutation, AdminError>;
    async fn delete(
        &self,
        id: &str,
        revision: Revision,
        context: &MutationContext,
    ) -> Result<Revision, AdminError>;
    async fn test(
        &self,
        id: &str,
        revision: Revision,
        context: &MutationContext,
    ) -> Result<ProxyRecord, AdminError>;
    /// 探测未保存的连接地址，不写入代理记录或修改账号绑定。
    async fn probe(&self, proxy: &OutboundProxy) -> Result<ProxyTestResult, AdminError>;
    /// 经代理探测各上游目标并评分；同时刷新连通性结果。
    async fn quality_check(
        &self,
        id: &str,
        revision: Revision,
        context: &MutationContext,
    ) -> Result<ProxyQualityOutcome, AdminError>;
    async fn quality_report(&self, id: &str) -> Result<Option<ProxyQualityReport>, AdminError>;
    /// 逐条创建；重复或不合法的条目跳过，不让整批失败。
    async fn create_batch(
        &self,
        commands: Vec<NewProxy>,
        context: &MutationContext,
    ) -> Result<ProxyBatchCreate, AdminError>;
    /// 逐条删除；仍被账号使用或已被修改的条目跳过。
    async fn delete_batch(
        &self,
        items: Vec<ProxyBatchDeleteItem>,
        context: &MutationContext,
    ) -> Result<ProxyBatchDelete, AdminError>;
}

/// 批量操作由前端并发发起；短暂排队比立即拒绝更符合预期，仍以超时兜底防止堆积。
const TEST_SLOT_WAIT: Duration = Duration::from_secs(20);

pub(crate) struct DefaultProxiesService {
    store: Arc<dyn ProxyStore>,
    probe: Arc<dyn ProxyProbe>,
    snapshot: Arc<dyn SnapshotControl>,
    providers: ProviderAdminRegistry,
    test_slots: Semaphore,
}

impl DefaultProxiesService {
    pub(crate) fn new(
        store: Arc<dyn ProxyStore>,
        probe: Arc<dyn ProxyProbe>,
        snapshot: Arc<dyn SnapshotControl>,
        providers: ProviderAdminRegistry,
    ) -> Self {
        Self {
            store,
            probe,
            snapshot,
            providers,
            test_slots: Semaphore::new(4),
        }
    }

    async fn test_slot(&self) -> Result<SemaphorePermit<'_>, AdminError> {
        match tokio::time::timeout(TEST_SLOT_WAIT, self.test_slots.acquire()).await {
            Ok(Ok(permit)) => Ok(permit),
            _ => Err(AdminError::new(
                AdminErrorKind::RateLimited,
                "代理测试繁忙，请稍后重试",
            )),
        }
    }

    /// 已提交的条目必须发布，即使本批随后中断。
    async fn publish_batch(&self, revision: Option<Revision>) -> Result<(), AdminError> {
        match revision {
            Some(revision) => publish_committed(self.snapshot.as_ref(), revision).await,
            None => Ok(()),
        }
    }
}

#[async_trait]
impl ProxiesService for DefaultProxiesService {
    async fn remove_account(
        &self,
        proxy_id: &str,
        account_id: &str,
        context: &MutationContext,
    ) -> Result<Revision, AdminError> {
        if proxy_id.is_empty() || proxy_id.len() > 128 || proxy_id.chars().any(char::is_control) {
            return Err(AdminError::invalid("代理 ID 不合法"));
        }
        let account_id = ProviderAccountId::new(account_id.to_owned())
            .map_err(|_| AdminError::invalid("账号 ID 不合法"))?;
        let revision = self
            .store
            .remove_account(proxy_id, &account_id, context)
            .await
            .map_err(|error| {
                if error.kind() == AdminStoreErrorKind::Conflict {
                    AdminError::conflict("账号的代理绑定已变化，请刷新后重试")
                } else {
                    map_store_error(error, "proxy account")
                }
            })?;
        publish_committed(self.snapshot.as_ref(), revision).await?;
        Ok(revision)
    }

    async fn list_accounts(
        &self,
        query: ProxyAccountListQuery,
    ) -> Result<ProxyAccountPage, AdminError> {
        if query.proxy_id.is_empty()
            || query.proxy_id.len() > 128
            || query.proxy_id.chars().any(char::is_control)
            || query.page == 0
            || query.search.len() > 256
            || query.search.chars().any(char::is_control)
        {
            return Err(AdminError::invalid("关联账号查询参数不合法"));
        }
        let mut page = self
            .store
            .list_accounts(query)
            .await
            .map_err(|error| map_store_error(error, "proxy"))?;
        page.items = stream::iter(page.items)
            .map(|mut account| async {
                // 与账号目录一致，缺失套餐时读取已有额度快照；限制并发且不触发上游刷新。
                let mut cached_quota = None;
                if explicit_plan_type(account.plan_type.as_deref()).is_none()
                    && let Ok(kind) = ProviderKind::new(account.provider_kind.clone())
                    && let Ok(provider) = self.providers.require(&kind)
                    && let Ok(account_id) = ProviderAccountId::new(account.id.clone())
                    && let Ok(quota) = provider
                        .quota(ProviderQuotaRequest {
                            account_id,
                            refresh: false,
                            rolling_usage: None,
                        })
                        .await
                {
                    cached_quota = Some(quota);
                }
                account.plan_type_display = self.providers.resolve_account_plan(
                    &account.provider_kind,
                    &mut account.plan_type,
                    cached_quota.as_ref(),
                );
                account
            })
            .buffered(8)
            .collect()
            .await;
        Ok(page)
    }

    async fn list(&self, query: ProxyListQuery) -> Result<ProxyPage, AdminError> {
        if query.page == 0 || query.search.len() > 256 || query.search.chars().any(char::is_control)
        {
            return Err(AdminError::invalid("代理查询参数不合法"));
        }
        self.store
            .list(query)
            .await
            .map_err(|error| map_store_error(error, "proxy"))
    }

    async fn create(
        &self,
        mut command: NewProxy,
        context: &MutationContext,
    ) -> Result<ProxyMutation, AdminError> {
        command.name = validate_name(&command.name)?;
        command.location = command
            .location
            .map(|location| location.normalized())
            .transpose()
            .map_err(|_| AdminError::invalid("代理位置不合法"))?;
        let result = self
            .store
            .create(command, context)
            .await
            .map_err(|error| map_store_error(error, "proxy"))?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }

    async fn update(
        &self,
        mut command: UpdateProxy,
        context: &MutationContext,
    ) -> Result<ProxyMutation, AdminError> {
        command.name = validate_name(&command.name)?;
        command.location = command
            .location
            .map(|location| location.map(|value| value.normalized()).transpose())
            .transpose()
            .map_err(|_| AdminError::invalid("代理位置不合法"))?;
        let result = self
            .store
            .update(command, context)
            .await
            .map_err(|error| map_store_error(error, "proxy"))?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }

    async fn delete(
        &self,
        id: &str,
        revision: Revision,
        context: &MutationContext,
    ) -> Result<Revision, AdminError> {
        let result = self
            .store
            .delete(id, revision, context)
            .await
            .map_err(|error| map_store_error(error, "proxy"))?;
        publish_committed(self.snapshot.as_ref(), result).await?;
        Ok(result)
    }

    async fn probe(&self, proxy: &OutboundProxy) -> Result<ProxyTestResult, AdminError> {
        let _permit = self.test_slot().await?;
        Ok(self.probe.test(proxy).await)
    }

    async fn quality_check(
        &self,
        id: &str,
        revision: Revision,
        context: &MutationContext,
    ) -> Result<ProxyQualityOutcome, AdminError> {
        let _permit = self.test_slot().await?;
        let record = self
            .store
            .get(id)
            .await
            .map_err(|error| map_store_error(error, "proxy"))?;
        if record.revision != revision {
            return Err(AdminError::conflict("代理已被修改，请刷新后重新检测"));
        }
        let probe = self.probe.quality(&record.proxy).await;
        let report = ProxyQualityReport::finalize(probe, chrono::Utc::now());
        let record = self
            .store
            .record_quality(id, revision, report.clone(), context)
            .await
            .map_err(|error| map_store_error(error, "proxy"))?;
        Ok(ProxyQualityOutcome { record, report })
    }

    async fn quality_report(&self, id: &str) -> Result<Option<ProxyQualityReport>, AdminError> {
        validate_id(id)?;
        self.store
            .quality_report(id)
            .await
            .map_err(|error| map_store_error(error, "proxy"))
    }

    async fn create_batch(
        &self,
        commands: Vec<NewProxy>,
        context: &MutationContext,
    ) -> Result<ProxyBatchCreate, AdminError> {
        if commands.is_empty() || commands.len() > MAX_PROXY_BATCH_ITEMS {
            return Err(AdminError::invalid("批量添加需要 1 至 200 条代理"));
        }
        let mut result = ProxyBatchCreate {
            config_revision: None,
            created: Vec::new(),
            skipped: Vec::new(),
        };
        for mut command in commands {
            let reference = command.proxy.endpoint();
            let Ok(name) = validate_name(&command.name) else {
                result.skipped.push(ProxyBatchSkip {
                    reference,
                    reason: "代理名称需要 1 至 100 个字符".to_owned(),
                });
                continue;
            };
            command.name = name;
            match self.store.create(command, context).await {
                Ok(mutation) => {
                    result.config_revision = Some(mutation.config_revision);
                    result.created.push(mutation.record);
                }
                Err(error) if error.kind() == AdminStoreErrorKind::Conflict => {
                    result.skipped.push(ProxyBatchSkip {
                        reference,
                        reason: "代理地址已存在".to_owned(),
                    });
                }
                Err(error) => {
                    self.publish_batch(result.config_revision).await?;
                    return Err(map_store_error(error, "proxy"));
                }
            }
        }
        self.publish_batch(result.config_revision).await?;
        Ok(result)
    }

    async fn delete_batch(
        &self,
        items: Vec<ProxyBatchDeleteItem>,
        context: &MutationContext,
    ) -> Result<ProxyBatchDelete, AdminError> {
        if items.is_empty() || items.len() > MAX_PROXY_BATCH_ITEMS {
            return Err(AdminError::invalid("批量删除需要 1 至 200 条代理"));
        }
        for item in &items {
            validate_id(&item.id)?;
        }
        let mut result = ProxyBatchDelete {
            config_revision: None,
            deleted_ids: Vec::new(),
            skipped: Vec::new(),
        };
        for item in items {
            match self.store.delete(&item.id, item.revision, context).await {
                Ok(revision) => {
                    result.config_revision = Some(revision);
                    result.deleted_ids.push(item.id);
                }
                Err(error) if error.kind() == AdminStoreErrorKind::Conflict => {
                    result.skipped.push(ProxyBatchSkip {
                        reference: item.id,
                        reason: "代理仍有账号使用，或已被修改".to_owned(),
                    });
                }
                Err(error) if error.kind() == AdminStoreErrorKind::NotFound => {
                    result.skipped.push(ProxyBatchSkip {
                        reference: item.id,
                        reason: "代理不存在或已被删除".to_owned(),
                    });
                }
                Err(error) => {
                    self.publish_batch(result.config_revision).await?;
                    return Err(map_store_error(error, "proxy"));
                }
            }
        }
        self.publish_batch(result.config_revision).await?;
        Ok(result)
    }

    async fn test(
        &self,
        id: &str,
        revision: Revision,
        context: &MutationContext,
    ) -> Result<ProxyRecord, AdminError> {
        let _permit = self.test_slot().await?;
        let record = self
            .store
            .get(id)
            .await
            .map_err(|error| map_store_error(error, "proxy"))?;
        if record.revision != revision {
            return Err(AdminError::conflict("代理已被修改，请刷新后重新测试"));
        }
        let result = self.probe.test(&record.proxy).await;
        self.store
            .record_test(id, revision, result, context)
            .await
            .map_err(|error| map_store_error(error, "proxy"))
    }
}

fn validate_id(id: &str) -> Result<(), AdminError> {
    if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
        return Err(AdminError::invalid("代理 ID 不合法"));
    }
    Ok(())
}

fn validate_name(value: &str) -> Result<String, AdminError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 100 || value.chars().any(char::is_control) {
        return Err(AdminError::invalid("代理名称需要 1 至 100 个字符"));
    }
    Ok(value.to_owned())
}
