//! 统一账号目录与跨 Provider 动态分派。

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use futures::StreamExt as _;
use gateway_core::{
    account::ProviderAccountId,
    engine::probe::{AccountProbe, AccountProbeRequest},
    routing::{ProviderKind, UpstreamModelId},
    runtime::SnapshotControl,
};

use crate::{
    model::{
        AdminError, MutationContext,
        accounts::{
            AccountConnectionTestEvent, AccountConnectionTestEventStream, AccountListQuery,
            AccountPageItem, AccountUpdateResult, AccountUsage, AccountUsageWindowQuery,
            AccountsUpdateResult, BatchUpdateAccounts, TurnStateHuntCommand,
            TurnStateHuntEventStream, UpdateAccount,
        },
        observability::TimeRange,
        provider_credentials::{
            AccountConcurrency, AccountDirectoryItem, AccountDirectoryPage, AccountExportBundle,
            AccountPersonalInfo, AccountRecentErrors, AccountRefreshResult,
            ConsumeProviderResetCredit, PrepareCredentialRefresh, ProviderModels,
            ProviderProfileAvatar, ProviderQuota, ProviderQuotaRequest, ProviderQuotaWindow,
            ProviderResetCreditResult, ProviderResetCredits, QuotaLocalUsageAttribution,
        },
        quota_forecast::{AccountQuotaForecastReport, account_quota_forecasts},
        quota_forecast_sampling::{QuotaForecastPoint, select_forecast_sample},
    },
    ports::{
        provider::ProviderAdminRegistry,
        proxy::ProxyStore,
        store::{AccountRuntimeStore, AccountStore, SettingsStore},
    },
};

use super::usage_projection_cache::{self, UsageProjection};
use super::{
    commit_credential_refresh, map_provider_error, map_store_error, publish_committed,
    validate_prepared_rotation,
};

pub(super) const CONNECTION_TEST_INPUT: &str = "Reply with exactly OK.";

/// 统一账号页消费的服务。
#[async_trait]
pub trait AccountsService: Send + Sync {
    async fn list(&self, query: AccountListQuery) -> Result<AccountDirectoryPage, AdminError>;

    async fn export(
        &self,
        context: &MutationContext,
        account_ids: Vec<ProviderAccountId>,
    ) -> Result<AccountExportBundle, AdminError>;

    async fn refresh(
        &self,
        context: &MutationContext,
        account_id: ProviderAccountId,
    ) -> Result<AccountRefreshResult, AdminError>;

    async fn recover(
        &self,
        context: &MutationContext,
        account_id: ProviderAccountId,
    ) -> Result<AccountRefreshResult, AdminError>;

    async fn update(
        &self,
        context: &MutationContext,
        command: UpdateAccount,
    ) -> Result<AccountUpdateResult, AdminError>;

    async fn lower_concurrency_limit(
        &self,
        context: &MutationContext,
        account_id: ProviderAccountId,
        limit: gateway_core::account::AccountConcurrencyLimit,
    ) -> Result<Option<AccountUpdateResult>, AdminError>;

    async fn batch_update(
        &self,
        context: &MutationContext,
        command: BatchUpdateAccounts,
    ) -> Result<AccountsUpdateResult, AdminError>;

    async fn account_configuration(
        &self,
        _account_id: &ProviderAccountId,
    ) -> Result<Option<crate::model::provider_credentials::ProviderDocument>, AdminError> {
        Ok(None)
    }

    async fn quota(
        &self,
        account_id: &ProviderAccountId,
        refresh: bool,
    ) -> Result<AccountDirectoryItem, AdminError>;

    async fn quota_forecast(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<AccountQuotaForecastReport, AdminError>;

    async fn personal_info(
        &self,
        _account_id: &ProviderAccountId,
    ) -> Result<AccountPersonalInfo, AdminError> {
        Err(AdminError::invalid("当前 Provider 不支持个人信息"))
    }

    async fn profile_avatar(
        &self,
        _account_id: &ProviderAccountId,
    ) -> Result<ProviderProfileAvatar, AdminError> {
        Err(AdminError::invalid("当前 Provider 不支持账号头像"))
    }

    async fn reset_credits(
        &self,
        _context: &MutationContext,
        _account_id: ProviderAccountId,
    ) -> Result<ProviderResetCredits, AdminError> {
        Err(AdminError::invalid("当前 Provider 不支持重置额度"))
    }

    async fn consume_reset_credit(
        &self,
        _context: &MutationContext,
        _command: ConsumeProviderResetCredit,
    ) -> Result<ProviderResetCreditResult, AdminError> {
        Err(AdminError::invalid("当前 Provider 不支持重置额度"))
    }

    async fn models(
        &self,
        account_id: &ProviderAccountId,
        refresh: bool,
    ) -> Result<ProviderModels, AdminError>;

    async fn test_connection(
        &self,
        account_id: ProviderAccountId,
        upstream_model: UpstreamModelId,
    ) -> Result<AccountConnectionTestEventStream, AdminError>;

    /// 测智台：对指定账号发一条自定义 prompt（可选思考强度）的测试请求，流式回输出。
    /// 走 probe 路径钉住该账号；钉票随账号自动带（有则带、无则按无票客户跑）。
    async fn run_test_bench(
        &self,
        account_id: ProviderAccountId,
        upstream_model: UpstreamModelId,
        prompt: String,
        reasoning_effort: Option<String>,
    ) -> Result<AccountConnectionTestEventStream, AdminError>;

    /// 账号级 state 缺失或将在 `margin` 内到期、需要自动续期的账号。
    async fn turn_state_renewals(
        &self,
        _now: std::time::SystemTime,
        _margin: std::time::Duration,
    ) -> Vec<crate::ports::provider::TurnStateRenewal> {
        Vec::new()
    }

    /// 逐个出口发真实请求找符合长度规则的 state；命中即绑定该出口并钉住。
    async fn turn_state_hunt(
        &self,
        _command: TurnStateHuntCommand,
    ) -> Result<TurnStateHuntEventStream, AdminError> {
        Err(AdminError::invalid("当前服务不支持遍历代理找 state"))
    }

    /// 账号成本、到期与票据状态（不含票据明文）。
    async fn account_ticket(
        &self,
        _account_id: &ProviderAccountId,
    ) -> Result<crate::model::account_tickets::AccountTicketFacts, AdminError> {
        Err(AdminError::invalid("当前服务不支持账号成本与票据"))
    }

    async fn update_account_ticket(
        &self,
        _command: crate::model::account_tickets::UpdateAccountTicket,
    ) -> Result<crate::model::account_tickets::AccountTicketFacts, AdminError> {
        Err(AdminError::invalid("当前服务不支持账号成本与票据"))
    }

    /// 解密票据得到 Provider 轮换材料，交给凭据轮换流程登录并写回原账号。
    async fn ticket_restore_material(
        &self,
        _account_id: &ProviderAccountId,
    ) -> Result<crate::model::provider_credentials::ProviderDocument, AdminError> {
        Err(AdminError::invalid("当前服务不支持票据恢复"))
    }

    /// 自动撞：从一条轮换代理模板即时生成多国临时出口反复撞，命中即切静态并钉住。
    async fn auto_turn_state_hunt(
        &self,
        _request: crate::model::accounts::TurnStateAutoHuntRequest,
    ) -> Result<TurnStateHuntEventStream, AdminError> {
        Err(AdminError::invalid("当前服务不支持自动撞 state"))
    }

    /// 自动续期专用：存在轮换代理模板时走「自动撞」的快速多国临时出口，命中切静态；
    /// 否则回退到遍历已存代理。默认实现走遍历，供不支持的服务复用。
    async fn renewal_turn_state_hunt(
        &self,
        command: TurnStateHuntCommand,
    ) -> Result<TurnStateHuntEventStream, AdminError> {
        self.turn_state_hunt(command).await
    }
}

/// 无额度窗口账号的本地累计窗口 key（起点 = 账号创建时间）。
const LIFETIME_WINDOW_KEY: &str = "account-lifetime";

#[derive(Clone)]
pub(crate) struct DefaultAccountsService {
    pub(super) accounts: Arc<dyn AccountStore>,
    pub(super) ticket_cipher: Arc<crate::ticket_cipher::TicketCipher>,
    account_runtime: Arc<dyn AccountRuntimeStore>,
    settings: Arc<dyn SettingsStore>,
    providers: ProviderAdminRegistry,
    snapshot: Arc<dyn SnapshotControl>,
    pub(super) probe: Arc<dyn AccountProbe>,
    pub(super) proxies: Arc<dyn ProxyStore>,
    pub(super) hunts: super::turn_state_hunt::ActiveHunts,
    reset_credit_locks:
        Arc<futures::lock::Mutex<BTreeMap<ProviderAccountId, Arc<futures::lock::Mutex<()>>>>>,
    /// 列表用量投影的 SWR 缓存（只装请求历史聚合，见模块注释）。
    usage_cache: Arc<usage_projection_cache::UsageProjectionCache>,
}

impl DefaultAccountsService {
    #[must_use]
    // 组合根一次性注入各项独立能力，拆成参数结构体只会多一层搬运。
    #[expect(clippy::too_many_arguments)]
    pub(crate) fn new(
        accounts: Arc<dyn AccountStore>,
        account_runtime: Arc<dyn AccountRuntimeStore>,
        settings: Arc<dyn SettingsStore>,
        providers: ProviderAdminRegistry,
        snapshot: Arc<dyn SnapshotControl>,
        probe: Arc<dyn AccountProbe>,
        proxies: Arc<dyn ProxyStore>,
        ticket_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            ticket_cipher: Arc::new(crate::ticket_cipher::TicketCipher::new(ticket_dir)),
            accounts,
            account_runtime,
            settings,
            providers,
            snapshot,
            probe,
            proxies,
            hunts: super::turn_state_hunt::ActiveHunts::default(),
            reset_credit_locks: Arc::new(futures::lock::Mutex::new(BTreeMap::new())),
            usage_cache: Arc::default(),
        }
    }

    /// 账号列表的并发列：实时占用来自 lease 存储，上限未单独设置时继承全局默认。
    /// 两者都只用于展示，任一读取失败只降级为未知，不影响列表。
    async fn concurrency(
        &self,
        accounts: &[&crate::model::accounts::AccountRecord],
    ) -> BTreeMap<String, AccountConcurrency> {
        let ids = accounts
            .iter()
            .map(|account| account.id.clone())
            .collect::<Vec<_>>();
        let (runtime, settings) = futures::join!(
            self.account_runtime.account_runtime(&ids),
            self.settings.load_runtime_settings(),
        );
        let in_flight = match runtime {
            Ok(runtime) => runtime.in_flight,
            Err(error) => {
                tracing::warn!(error = %error, "account concurrency in-flight is unavailable");
                None
            }
        };
        let default_limit = match settings {
            Ok(settings) => Some(settings.max_concurrent_per_account),
            Err(error) => {
                tracing::warn!(error = %error, "default account concurrency is unavailable");
                None
            }
        };
        accounts
            .iter()
            .map(|account| {
                let limit = account
                    .concurrency_limit
                    .map(|limit| u64::from(limit.get()))
                    // 全局默认 0 表示不限。
                    .or_else(|| default_limit.filter(|limit| *limit > 0).map(u64::from));
                let concurrency = AccountConcurrency {
                    in_flight: in_flight
                        .as_ref()
                        .map(|in_flight| in_flight.get(&account.id).copied().unwrap_or(0)),
                    limit,
                };
                (account.id.clone(), concurrency)
            })
            .collect()
    }

    /// 成本与票据状态只用于展示，读取失败降级为空，不影响列表。
    async fn ticket_facts(
        &self,
        account_ids: &[String],
    ) -> BTreeMap<String, crate::model::account_tickets::AccountTicketFacts> {
        self.accounts
            .load_account_tickets(account_ids)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(error = %error, "account tickets are unavailable");
                BTreeMap::new()
            })
    }

    /// 报错次数只用于展示，读取失败降级为零，不影响列表。
    async fn recent_errors(
        &self,
        range: TimeRange,
        account_ids: &[String],
    ) -> BTreeMap<String, AccountRecentErrors> {
        self.accounts
            .load_account_request_outcomes(range, account_ids)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(error = %error, "account request outcomes are unavailable");
                BTreeMap::new()
            })
    }

    async fn reset_credit_lock(
        &self,
        account_id: &ProviderAccountId,
    ) -> Arc<futures::lock::Mutex<()>> {
        let mut locks = self.reset_credit_locks.lock().await;
        Arc::clone(
            locks
                .entry(account_id.clone())
                .or_insert_with(|| Arc::new(futures::lock::Mutex::new(()))),
        )
    }

    async fn load_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<AccountPageItem, AdminError> {
        let runtime = self
            .account_runtime
            .account_runtime(&[account_id.as_str().to_owned()])
            .await
            .map_err(|error| map_store_error(error, "account runtime"))?;
        self.accounts
            .load_account(account_id.as_str(), runtime)
            .await
            .map_err(|error| map_store_error(error, "provider account"))?
            .ok_or_else(|| AdminError::not_found("Provider 账号不存在"))
    }

    pub(super) async fn provider_for_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<
        (
            AccountPageItem,
            Arc<dyn crate::ports::provider::ProviderAdmin>,
        ),
        AdminError,
    > {
        let item = self.load_account(account_id).await?;
        let provider = self
            .providers
            .require(&item.account.provider_kind)
            .map_err(|error| map_provider_error(error, "provider account"))?;
        Ok((item, provider))
    }

    /// 列表需要的本地用量窗口：额度窗口（上游没给本地用量的）+ 终身窗口（API Key 或无额度窗口的账号）。
    fn needed_usage_windows(
        accounts: &[AccountPageItem],
        quotas: &[ProviderQuota],
    ) -> Vec<AccountUsageWindowQuery> {
        let now = Utc::now();
        let mut windows = accounts
            .iter()
            .zip(quotas.iter())
            .flat_map(|(item, quota)| {
                quota
                    .windows
                    .iter()
                    .filter(|window| window.local_usage.is_none())
                    .filter_map(|window| quota_usage_window(&item.account.id, window))
            })
            .collect::<Vec<_>>();
        // API Key 或尚无额度窗口的账号展示本地累计，仅统计账号创建后仍保留的请求记录。
        windows.extend(
            accounts
                .iter()
                .zip(quotas)
                .filter(|(item, quota)| {
                    item.account.authentication_kind == "api_key" || quota.windows.is_empty()
                })
                .map(|(item, _)| AccountUsageWindowQuery {
                    account_id: item.account.id.clone(),
                    key: LIFETIME_WINDOW_KEY.to_owned(),
                    range: TimeRange {
                        start: item.account.created_at,
                        end: now,
                    },
                }),
        );
        windows
    }

    fn attach_quota_local_usage(
        accounts: &[AccountPageItem],
        quotas: &mut [ProviderQuota],
        usage_by_window: &BTreeMap<usage_projection_cache::WindowKey, AccountUsage>,
    ) {
        for (item, quota) in accounts.iter().zip(quotas) {
            for window in &mut quota.windows {
                if window.local_usage.is_none()
                    && window.local_usage_attribution == QuotaLocalUsageAttribution::AccountWide
                {
                    let Some(query) = quota_usage_window(&item.account.id, window) else {
                        continue;
                    };
                    if let Some(usage) =
                        usage_by_window.get(&usage_projection_cache::window_key(&query))
                    {
                        window.local_usage = Some(usage.clone());
                    }
                }
            }
        }
    }

    fn lifetime_usage(
        accounts: &[AccountPageItem],
        quotas: &[ProviderQuota],
        usage_by_window: &BTreeMap<usage_projection_cache::WindowKey, AccountUsage>,
    ) -> BTreeMap<String, AccountUsage> {
        accounts
            .iter()
            .zip(quotas)
            .filter(|(item, quota)| {
                item.account.authentication_kind == "api_key" || quota.windows.is_empty()
            })
            .filter_map(|(item, _)| {
                let key = (
                    item.account.id.clone(),
                    LIFETIME_WINDOW_KEY.to_owned(),
                    item.account.created_at,
                );
                usage_by_window
                    .get(&key)
                    .map(|usage| (item.account.id.clone(), usage.clone()))
            })
            .collect()
    }

    async fn load_directory_item(
        &self,
        account_id: &ProviderAccountId,
        refresh_quota: bool,
    ) -> Result<AccountDirectoryItem, AdminError> {
        let (stored, provider) = self.provider_for_account(account_id).await?;
        let account = &stored.account;
        let now = Utc::now();
        let rolling_range = TimeRange {
            start: now - Duration::hours(24),
            end: now,
        };
        let ids = vec![account.id.clone()];
        let rolling_usage = self
            .accounts
            .load_account_usage(rolling_range, &ids)
            .await
            .map_err(|error| map_store_error(error, "rolling account usage"))?;
        let rolling_usage = rolling_usage.into_iter().next();
        let mut quota = provider
            .quota(ProviderQuotaRequest {
                account_id: account_id.clone(),
                refresh: refresh_quota,
                rolling_usage: rolling_usage.clone(),
            })
            .await
            .map_err(|error| map_provider_error(error, "provider quota"))?;
        let mut stored = if refresh_quota {
            self.load_account(account_id).await?
        } else {
            stored
        };
        // 单账号详情不走列表缓存：只查这一个账号需要的窗口。
        let windows =
            Self::needed_usage_windows(std::slice::from_ref(&stored), std::slice::from_ref(&quota));
        let usage_by_window = if windows.is_empty() {
            BTreeMap::new()
        } else {
            usage_projection_cache::index_window_results(
                &windows,
                self.accounts
                    .load_account_usage_by_windows(&windows)
                    .await
                    .map_err(|error| map_store_error(error, "quota window usage"))?,
            )
        };
        Self::attach_quota_local_usage(
            std::slice::from_ref(&stored),
            std::slice::from_mut(&mut quota),
            &usage_by_window,
        );
        let usage = Self::lifetime_usage(
            std::slice::from_ref(&stored),
            std::slice::from_ref(&quota),
            &usage_by_window,
        )
        .remove(&stored.account.id)
            .or_else(|| {
                quota
                    .usage_window()
                    .and_then(|(window, _)| window.local_usage.clone())
            });
        let concurrency = self
            .concurrency(&[&stored.account])
            .await
            .remove(&stored.account.id)
            .unwrap_or_default();
        let recent_errors = self
            .recent_errors(rolling_range, &ids)
            .await
            .remove(&stored.account.id)
            .unwrap_or_default();
        Ok(AccountDirectoryItem {
            plan_type_display: self.providers.resolve_account_plan(
                stored.account.provider_kind.as_str(),
                &mut stored.account.plan_type,
                Some(&quota),
            ),
            concurrency,
            recent_errors,
            ticket: self
                .ticket_facts(&ids)
                .await
                .remove(&stored.account.id)
                .unwrap_or_default(),
            projection: stored.projection,
            usage,
            account: stored.account,
            quota,
        })
    }
}

#[async_trait]
impl AccountsService for DefaultAccountsService {
    async fn list(&self, query: AccountListQuery) -> Result<AccountDirectoryPage, AdminError> {
        let runtime = self
            .account_runtime
            .active_rate_limits()
            .await
            .map_err(|error| map_store_error(error, "account runtime"))?;
        let page = self
            .accounts
            .list_accounts(query, runtime)
            .await
            .map_err(|error| map_store_error(error, "account directory"))?;
        let rolling_range = usage_projection_cache::rolling_range(Utc::now());
        let ids = page
            .items
            .iter()
            .map(|item| item.account.id.clone())
            .collect::<Vec<_>>();
        // 用量投影走 SWR 缓存：命中即用（过期则后台重算），缺失才内联计算。
        let cache_key = ids.join("\n");
        let cached = self.usage_cache.get(&cache_key);
        let rolling_usage = match &cached {
            Some((projection, _)) => projection.rolling_usage.clone(),
            None => self
                .accounts
                .load_account_usage(rolling_range, &ids)
                .await
                .map_err(|error| map_store_error(error, "rolling account usage"))?
                .into_iter()
                .map(|usage| (usage.account_id.clone(), usage))
                .collect::<BTreeMap<_, _>>(),
        };
        let mut quotas = futures::future::join_all(page.items.iter().map(|item| async {
            let account = &item.account;
            let account_id = ProviderAccountId::new(account.id.clone())
                .map_err(|_| AdminError::invalid("Provider 账号 ID 不合法"))?;
            // 单个账号的 quota 投影失败（Provider 未注册或 quota 读取失败）不拖垮整页：
            // 该账号降级为空额度投影，其余账号与页面状态照常返回。
            let provider = match self.providers.require(&account.provider_kind) {
                Ok(provider) => provider,
                Err(error) => {
                    tracing::warn!(
                        account_id = %account.id,
                        error = %error,
                        "account directory provider is not registered; showing empty quota"
                    );
                    return Ok(empty_quota());
                }
            };
            match provider
                .quota(ProviderQuotaRequest {
                    account_id,
                    refresh: false,
                    rolling_usage: rolling_usage.get(&account.id).cloned(),
                })
                .await
            {
                Ok(quota) => Ok(quota),
                Err(error) => {
                    tracing::warn!(
                        account_id = %account.id,
                        error = %error,
                        "account directory quota projection failed; showing empty quota"
                    );
                    Ok(empty_quota())
                }
            }
        }))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, AdminError>>()?;
        // 额度窗口 / 终身窗口：先从投影里取，缺的（首轮、额度窗口翻期）内联补查并并入缓存。
        let needed_windows = Self::needed_usage_windows(&page.items, &quotas);
        let mut usage_by_window = cached
            .as_ref()
            .map(|(projection, _)| projection.usage_by_window.clone())
            .unwrap_or_default();
        let missing = needed_windows
            .iter()
            .filter(|query| !usage_by_window.contains_key(&usage_projection_cache::window_key(query)))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            let fetched = usage_projection_cache::index_window_results(
                &missing,
                self.accounts
                    .load_account_usage_by_windows(&missing)
                    .await
                    .map_err(|error| map_store_error(error, "quota window usage"))?,
            );
            usage_by_window.extend(fetched.clone());
            if cached.is_some() {
                self.usage_cache
                    .merge_windows(&cache_key, &missing, fetched);
            }
        }
        match &cached {
            None => self.usage_cache.store(
                cache_key.clone(),
                UsageProjection {
                    rolling_usage: rolling_usage.clone(),
                    windows: needed_windows.clone(),
                    usage_by_window: usage_by_window.clone(),
                },
            ),
            Some((_, true)) => {
                if let Some(snapshot) = self.usage_cache.try_begin_refresh(&cache_key) {
                    self.usage_cache.spawn_refresh(
                        Arc::clone(&self.accounts),
                        cache_key.clone(),
                        ids.clone(),
                        snapshot,
                    );
                }
            }
            Some((_, false)) => {}
        }
        Self::attach_quota_local_usage(&page.items, &mut quotas, &usage_by_window);
        let mut lifetime_usage = Self::lifetime_usage(&page.items, &quotas, &usage_by_window);
        let mut concurrency = self
            .concurrency(
                &page
                    .items
                    .iter()
                    .map(|item| &item.account)
                    .collect::<Vec<_>>(),
            )
            .await;
        let mut recent_errors = self.recent_errors(rolling_range, &ids).await;
        let mut tickets = self.ticket_facts(&ids).await;
        let items = page
            .items
            .into_iter()
            .zip(quotas)
            .map(|(mut item, quota)| {
                let usage = lifetime_usage.remove(&item.account.id).or_else(|| {
                    quota
                        .usage_window()
                        .and_then(|(window, _)| window.local_usage.clone())
                });
                AccountDirectoryItem {
                    plan_type_display: self.providers.resolve_account_plan(
                        item.account.provider_kind.as_str(),
                        &mut item.account.plan_type,
                        Some(&quota),
                    ),
                    concurrency: concurrency.remove(&item.account.id).unwrap_or_default(),
                    recent_errors: recent_errors.remove(&item.account.id).unwrap_or_default(),
                    ticket: tickets.remove(&item.account.id).unwrap_or_default(),
                    usage,
                    account: item.account,
                    projection: item.projection,
                    quota,
                }
            })
            .collect();
        Ok(AccountDirectoryPage {
            config_revision: page.config_revision,
            items,
            total: page.total,
            summary: page.summary,
        })
    }

    async fn export(
        &self,
        context: &MutationContext,
        account_ids: Vec<ProviderAccountId>,
    ) -> Result<AccountExportBundle, AdminError> {
        if account_ids.is_empty() || account_ids.len() > 200 {
            return Err(AdminError::invalid("账号导出数量必须在 1 到 200 之间"));
        }
        let exported_ids = account_ids.clone();
        let mut grouped = BTreeMap::<ProviderKind, Vec<ProviderAccountId>>::new();
        for account_id in account_ids {
            let account = self.load_account(&account_id).await?;
            grouped
                .entry(account.account.provider_kind)
                .or_default()
                .push(account_id);
        }
        if grouped.values().any(|ids| {
            let unique = ids.iter().collect::<std::collections::BTreeSet<_>>();
            unique.len() != ids.len()
        }) {
            return Err(AdminError::invalid("账号导出列表包含重复 ID"));
        }
        let mut documents = Vec::with_capacity(grouped.len());
        for (provider_kind, ids) in grouped {
            let provider = self
                .providers
                .require(&provider_kind)
                .map_err(|error| map_provider_error(error, "provider account export"))?;
            let credentials = self
                .accounts
                .load_credentials_for_export(&provider_kind, &ids)
                .await
                .map_err(|error| map_store_error(error, "provider account export"))?;
            documents.push(
                provider
                    .export_credentials(credentials)
                    .await
                    .map_err(|error| map_provider_error(error, "provider account export"))?,
            );
        }
        self.accounts
            .record_credential_export(&exported_ids, context)
            .await
            .map_err(|error| map_store_error(error, "provider account export audit"))?;
        Ok(AccountExportBundle {
            exported_at: Utc::now(),
            documents,
        })
    }

    async fn refresh(
        &self,
        context: &MutationContext,
        account_id: ProviderAccountId,
    ) -> Result<AccountRefreshResult, AdminError> {
        let (stored, provider) = self.provider_for_account(&account_id).await?;
        let account = stored.account;
        let prepared = provider
            .prepare_refresh(PrepareCredentialRefresh {
                account: account.clone(),
            })
            .await
            .map_err(|error| map_provider_error(error, "provider credential refresh"))?;
        validate_prepared_rotation(&account, &prepared, "provider credential refresh")?;
        let result = commit_credential_refresh(
            self.accounts.as_ref(),
            prepared,
            context,
            "provider credential refresh",
        )
        .await?;
        provider
            .account_facts_changed(std::slice::from_ref(&result.account_id))
            .await;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        let account = self.load_directory_item(&result.account_id, false).await?;
        Ok(AccountRefreshResult {
            config_revision: result.config_revision,
            account,
        })
    }

    async fn recover(
        &self,
        context: &MutationContext,
        account_id: ProviderAccountId,
    ) -> Result<AccountRefreshResult, AdminError> {
        let (stored, provider) = self.provider_for_account(&account_id).await?;
        let config_revision = if stored.account.enabled {
            self.accounts
                .recover_account(&account_id, context)
                .await
                .map_err(|error| map_store_error(error, "provider account recovery"))?
                .config_revision
        } else {
            // 停用只表示不参与调度，重新启用不能抹除已观测的额度、凭据或冷却事实。
            self.accounts
                .batch_update_accounts(
                    BatchUpdateAccounts {
                        account_ids: vec![account_id.to_string()],
                        enabled: Some(true),
                        concurrency_limit: None,
                        weight: None,
                        model_access: None,
                        group_ids: None,
                        outbound_proxy: None,
                    },
                    context,
                )
                .await
                .map_err(|error| map_store_error(error, "enable provider account"))?
                .config_revision
        };
        provider
            .account_facts_changed(std::slice::from_ref(&account_id))
            .await;
        publish_committed(self.snapshot.as_ref(), config_revision).await?;
        let account = self.load_directory_item(&account_id, false).await?;
        Ok(AccountRefreshResult {
            config_revision,
            account,
        })
    }

    async fn update(
        &self,
        context: &MutationContext,
        command: UpdateAccount,
    ) -> Result<AccountUpdateResult, AdminError> {
        let account_id = ProviderAccountId::new(command.account_id.clone())
            .map_err(|_| AdminError::invalid("Provider 账号 ID 不合法"))?;
        let (_, provider) = self.provider_for_account(&account_id).await?;
        let enabled = command.enabled;
        let result = self
            .accounts
            .update_account(command, context)
            .await
            .map_err(|error| map_store_error(error, "provider account"))?;
        if !enabled {
            provider.account_unavailable(&account_id).await;
        }
        provider
            .account_facts_changed(std::slice::from_ref(&account_id))
            .await;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }

    async fn lower_concurrency_limit(
        &self,
        context: &MutationContext,
        account_id: ProviderAccountId,
        limit: gateway_core::account::AccountConcurrencyLimit,
    ) -> Result<Option<AccountUpdateResult>, AdminError> {
        let (_, provider) = self.provider_for_account(&account_id).await?;
        let result = self
            .accounts
            .lower_concurrency_limit(&account_id, limit, context)
            .await
            .map_err(|error| map_store_error(error, "provider account"))?;
        if let Some(result) = &result {
            provider
                .account_facts_changed(std::slice::from_ref(&account_id))
                .await;
            publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        }
        Ok(result)
    }

    async fn batch_update(
        &self,
        context: &MutationContext,
        command: BatchUpdateAccounts,
    ) -> Result<AccountsUpdateResult, AdminError> {
        let account_ids = command
            .account_ids
            .iter()
            .map(|id| {
                ProviderAccountId::new(id.clone())
                    .map_err(|_| AdminError::invalid("Provider 账号 ID 不合法"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut providers = BTreeMap::<
            ProviderKind,
            (
                Arc<dyn crate::ports::provider::ProviderAdmin>,
                Vec<ProviderAccountId>,
            ),
        >::new();
        for account_id in &account_ids {
            let (item, provider) = self.provider_for_account(account_id).await?;
            providers
                .entry(item.account.provider_kind)
                .or_insert_with(|| (provider, Vec::new()))
                .1
                .push(account_id.clone());
        }
        let enabled = command.enabled;
        let result = self
            .accounts
            .batch_update_accounts(command, context)
            .await
            .map_err(|error| map_store_error(error, "provider accounts"))?;
        for (provider, provider_ids) in providers.values() {
            if enabled == Some(false) {
                for account_id in provider_ids {
                    provider.account_unavailable(account_id).await;
                }
            }
            provider.account_facts_changed(provider_ids).await;
        }
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }

    async fn account_configuration(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<Option<crate::model::provider_credentials::ProviderDocument>, AdminError> {
        let (_, provider) = self.provider_for_account(account_id).await?;
        provider
            .account_configuration(account_id)
            .await
            .map_err(|error| map_provider_error(error, "provider account configuration"))
    }

    async fn quota(
        &self,
        account_id: &ProviderAccountId,
        refresh: bool,
    ) -> Result<AccountDirectoryItem, AdminError> {
        self.load_directory_item(account_id, refresh).await
    }

    async fn quota_forecast(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<AccountQuotaForecastReport, AdminError> {
        let (stored, provider) = self.provider_for_account(account_id).await?;
        let quota = provider
            .quota(ProviderQuotaRequest {
                account_id: account_id.clone(),
                refresh: false,
                rolling_usage: None,
            })
            .await
            .map_err(|error| map_provider_error(error, "forecast quota snapshot"))?;
        let now = Utc::now();
        let mut samples = Vec::new();
        for (window, _) in quota.usage_windows() {
            let (Some(mut query), Some(observed), Some(percent)) = (
                quota_usage_window(account_id.as_str(), window),
                quota.observed_at,
                window.used_percent,
            ) else {
                continue;
            };
            query.range.start = query.range.start.max(stored.account.created_at);
            if query.range.start >= observed
                || observed > now
                || now >= query.range.end
                || !percent.is_finite()
                || !(0.0..=100.0).contains(&percent)
            {
                continue;
            }
            let reset_at = query.range.end;
            query.range.end = observed;
            let history = self
                .accounts
                .load_quota_forecast_history(&query)
                .await
                .map_err(|error| map_store_error(error, "forecast paired usage"))?;
            let mut points = Vec::new();
            let mut interrupted = false;
            for point in history.points {
                let Some(fact) =
                    provider.quota_forecast_observation(&point.provider_observation, window)
                else {
                    continue;
                };
                let same_plan = quota
                    .plan_type
                    .as_deref()
                    .zip(fact.plan_type.as_deref())
                    .is_some_and(|(current, previous)| current.eq_ignore_ascii_case(previous));
                // 仅容纳已观测到的秒级量化抖动，不用宽时间容差合并实际重置。
                // 不匹配的段截断基线；之后的有效观测可以重新积累。
                if !same_plan || (fact.reset_at - reset_at).abs() > Duration::seconds(2) {
                    points.clear();
                    interrupted = true;
                    continue;
                }
                points.push(QuotaForecastPoint {
                    observed_at: point.completed_at,
                    used_percent: fact.used_percent,
                    usage: point.usage,
                });
            }
            let sample = select_forecast_sample(
                window.key.clone(),
                query.range.start,
                QuotaForecastPoint {
                    observed_at: observed,
                    used_percent: percent,
                    usage: history.usage,
                },
                points,
                history.pending_request_count,
                interrupted,
            );
            samples.push(sample);
        }
        Ok(AccountQuotaForecastReport {
            account_id: account_id.to_string(),
            generated_at: now,
            forecasts: account_quota_forecasts(&quota, stored.account.created_at, now, &samples),
        })
    }

    async fn personal_info(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<AccountPersonalInfo, AdminError> {
        let (initial, provider) = self.provider_for_account(account_id).await?;
        // 两项读取相互独立；不因其中一项失败而取消另一项，也不触发凭据或额度刷新。
        let (profile, subscription) = futures::join!(
            provider.profile_statistics(account_id),
            provider.subscription(account_id),
        );
        let profile =
            profile.map_err(|error| map_provider_error(error, "provider profile statistics"));
        let subscription = subscription
            .map_err(|error| map_provider_error(error, "provider subscription"))
            .ok()
            .flatten();

        // 汇聚等待期间发生重新授权、换绑或删除时，不返回混合身份的数据。
        let current = self.load_account(account_id).await?;
        if current.account.provider_kind != initial.account.provider_kind
            || current.account.credential_revision != initial.account.credential_revision
            || current.account.upstream_user_id != initial.account.upstream_user_id
            || current.account.upstream_account_id != initial.account.upstream_account_id
        {
            return Err(AdminError::conflict("账号身份已变化，请刷新信息后重试"));
        }
        Ok(AccountPersonalInfo {
            profile,
            subscription,
        })
    }

    async fn profile_avatar(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<ProviderProfileAvatar, AdminError> {
        let (_, provider) = self.provider_for_account(account_id).await?;
        provider
            .profile_avatar(account_id)
            .await
            .map_err(|error| map_provider_error(error, "provider profile avatar"))
    }

    async fn reset_credits(
        &self,
        context: &MutationContext,
        account_id: ProviderAccountId,
    ) -> Result<ProviderResetCredits, AdminError> {
        let (_, provider) = self.provider_for_account(&account_id).await?;
        match provider.reset_credits(&account_id).await {
            Ok(credits) => Ok(credits),
            Err(error)
                if error.kind()
                    == crate::ports::provider::ProviderAdminErrorKind::CredentialRefreshRequired =>
            {
                self.refresh(context, account_id.clone()).await?;
                let (_, provider) = self.provider_for_account(&account_id).await?;
                provider
                    .reset_credits(&account_id)
                    .await
                    .map_err(map_reset_credits_error_after_refresh)
            }
            Err(error) => Err(map_provider_error(error, "provider reset credits")),
        }
    }

    async fn consume_reset_credit(
        &self,
        context: &MutationContext,
        command: ConsumeProviderResetCredit,
    ) -> Result<ProviderResetCreditResult, AdminError> {
        let account_id = command.account_id.clone();
        // 覆盖 credential refresh + 同键重试的完整账号级临界区，避免 401 两次调用
        // 之间插入另一笔不可逆消费。
        let lock = self.reset_credit_lock(&account_id).await;
        let _guard = lock.lock().await;
        let (_, provider) = self.provider_for_account(&account_id).await?;
        match provider.consume_reset_credit(command.clone()).await {
            Ok(result) => Ok(result),
            Err(error)
                if error.kind()
                    == crate::ports::provider::ProviderAdminErrorKind::CredentialRefreshRequired =>
            {
                self.refresh(context, account_id.clone()).await?;
                let (_, provider) = self.provider_for_account(&account_id).await?;
                provider
                    .consume_reset_credit(command)
                    .await
                    .map_err(map_reset_credits_error_after_refresh)
            }
            Err(error) => Err(map_provider_error(error, "provider reset-credit consume")),
        }
    }

    async fn models(
        &self,
        account_id: &ProviderAccountId,
        refresh: bool,
    ) -> Result<ProviderModels, AdminError> {
        let (_, provider) = self.provider_for_account(account_id).await?;
        provider
            .models(account_id, refresh)
            .await
            .map_err(|error| map_provider_error(error, "provider model catalog"))
    }

    async fn test_connection(
        &self,
        account_id: ProviderAccountId,
        upstream_model: UpstreamModelId,
    ) -> Result<AccountConnectionTestEventStream, AdminError> {
        let (stored, provider) = self.provider_for_account(&account_id).await?;
        let account = stored.account;
        let model = upstream_model.as_str().to_owned();
        let operation = provider
            .connection_test_operation(&upstream_model, CONNECTION_TEST_INPUT)
            .map_err(|error| map_provider_error(error, "provider connection test"))?;
        let initial = vec![
            AccountConnectionTestEvent::Started {
                model: model.clone(),
            },
            AccountConnectionTestEvent::Request {
                model,
                input_text: CONNECTION_TEST_INPUT.to_owned(),
                stream: true,
                store: false,
            },
        ];
        let probe = Arc::clone(&self.probe);
        let terminal = futures::stream::once(async move {
            let result = probe
                .probe(AccountProbeRequest {
                    account_id,
                    provider_kind: account.provider_kind,
                    upstream_model,
                    operation,
                    egress: None,
                })
                .await;
            match result {
                Ok(result) => result
                    .text
                    .into_iter()
                    .map(|text| AccountConnectionTestEvent::Content { text })
                    .chain(std::iter::once(AccountConnectionTestEvent::Completed))
                    .collect(),
                Err(error) => {
                    let upstream_status = error
                        .upstream_response()
                        .map(gateway_core::engine::probe::AccountProbeUpstreamResponse::status);
                    let upstream_content_type = error
                        .upstream_response()
                        .and_then(|response| response.content_type())
                        .and_then(|value| std::str::from_utf8(value).ok())
                        .map(ToOwned::to_owned);
                    let upstream_body = error
                        .upstream_response()
                        .map(|response| String::from_utf8_lossy(response.body()).into_owned());
                    let message = error.client_message().to_owned();
                    vec![AccountConnectionTestEvent::Failed {
                        source: error.source(),
                        gateway_error_code: error.kind(),
                        send_state: error.send_state(),
                        message,
                        provider_error_code: error.client_error_code().map(ToOwned::to_owned),
                        provider_error_type: error.client_error_type().map(ToOwned::to_owned),
                        upstream_status,
                        upstream_content_type,
                        upstream_body,
                    }]
                }
            }
        })
        .flat_map(futures::stream::iter);
        Ok(Box::pin(futures::stream::iter(initial).chain(terminal)))
    }

    async fn run_test_bench(
        &self,
        account_id: ProviderAccountId,
        upstream_model: UpstreamModelId,
        prompt: String,
        reasoning_effort: Option<String>,
    ) -> Result<AccountConnectionTestEventStream, AdminError> {
        let (stored, provider) = self.provider_for_account(&account_id).await?;
        let account = stored.account;
        let model = upstream_model.as_str().to_owned();
        let operation = provider
            .test_bench_operation(&upstream_model, &prompt, reasoning_effort.as_deref())
            .map_err(|error| map_provider_error(error, "provider test bench"))?;
        let initial = vec![
            AccountConnectionTestEvent::Started {
                model: model.clone(),
            },
            AccountConnectionTestEvent::Request {
                model,
                input_text: prompt,
                stream: true,
                store: false,
            },
        ];
        let probe = Arc::clone(&self.probe);
        let terminal = futures::stream::once(async move {
            let result = probe
                .probe(AccountProbeRequest {
                    account_id,
                    provider_kind: account.provider_kind,
                    upstream_model,
                    operation,
                    egress: None,
                })
                .await;
            match result {
                Ok(result) => result
                    .text
                    .into_iter()
                    .map(|text| AccountConnectionTestEvent::Content { text })
                    .chain(std::iter::once(AccountConnectionTestEvent::Completed))
                    .collect(),
                Err(error) => {
                    let upstream_status = error
                        .upstream_response()
                        .map(gateway_core::engine::probe::AccountProbeUpstreamResponse::status);
                    let upstream_content_type = error
                        .upstream_response()
                        .and_then(|response| response.content_type())
                        .and_then(|value| std::str::from_utf8(value).ok())
                        .map(ToOwned::to_owned);
                    let upstream_body = error
                        .upstream_response()
                        .map(|response| String::from_utf8_lossy(response.body()).into_owned());
                    let message = error.client_message().to_owned();
                    vec![AccountConnectionTestEvent::Failed {
                        source: error.source(),
                        gateway_error_code: error.kind(),
                        send_state: error.send_state(),
                        message,
                        provider_error_code: error.client_error_code().map(ToOwned::to_owned),
                        provider_error_type: error.client_error_type().map(ToOwned::to_owned),
                        upstream_status,
                        upstream_content_type,
                        upstream_body,
                    }]
                }
            }
        })
        .flat_map(futures::stream::iter);
        Ok(Box::pin(futures::stream::iter(initial).chain(terminal)))
    }

    async fn turn_state_renewals(
        &self,
        now: std::time::SystemTime,
        margin: std::time::Duration,
    ) -> Vec<crate::ports::provider::TurnStateRenewal> {
        self.providers.turn_state_hunt_renewals(now, margin).await
    }

    async fn turn_state_hunt(
        &self,
        command: TurnStateHuntCommand,
    ) -> Result<TurnStateHuntEventStream, AdminError> {
        self.start_turn_state_hunt(command).await
    }

    async fn account_ticket(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<crate::model::account_tickets::AccountTicketFacts, AdminError> {
        self.load_ticket_facts(account_id).await
    }

    async fn update_account_ticket(
        &self,
        command: crate::model::account_tickets::UpdateAccountTicket,
    ) -> Result<crate::model::account_tickets::AccountTicketFacts, AdminError> {
        self.save_ticket(command).await
    }

    async fn ticket_restore_material(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<crate::model::provider_credentials::ProviderDocument, AdminError> {
        self.ticket_material(account_id).await
    }

    async fn auto_turn_state_hunt(
        &self,
        request: crate::model::accounts::TurnStateAutoHuntRequest,
    ) -> Result<TurnStateHuntEventStream, AdminError> {
        self.start_auto_turn_state_hunt(request).await
    }

    async fn renewal_turn_state_hunt(
        &self,
        command: TurnStateHuntCommand,
    ) -> Result<TurnStateHuntEventStream, AdminError> {
        self.start_renewal_turn_state_hunt(command).await
    }
}

fn map_reset_credits_error_after_refresh(
    error: crate::ports::provider::ProviderAdminError,
) -> AdminError {
    if error.kind() == crate::ports::provider::ProviderAdminErrorKind::CredentialRefreshRequired {
        return AdminError::bad_gateway("上游服务拒绝了刷新后的凭据");
    }
    map_provider_error(error, "provider reset credits")
}

/// 账号目录中单个账号 quota 读取失败时使用的空额度投影。
fn empty_quota() -> ProviderQuota {
    ProviderQuota {
        plan_type: None,
        observed_at: None,
        refresh_token_expires_at: None,
        windows: Vec::new(),
        limit_reached: false,
        provider_data: None,
    }
}

fn quota_usage_window(
    account_id: &str,
    window: &ProviderQuotaWindow,
) -> Option<AccountUsageWindowQuery> {
    if window.local_usage_attribution != QuotaLocalUsageAttribution::AccountWide {
        return None;
    }
    let reset_at = window.reset_at?;
    let seconds = i64::try_from(window.window_seconds?).ok()?;
    let start = reset_at.checked_sub_signed(Duration::try_seconds(seconds)?)?;
    let range = TimeRange::new(start, reset_at).ok()?;
    // 上游百分比以该 reset 边界定义；以当前时间回推会让本地 Token 属于另一窗口。
    Some(AccountUsageWindowQuery {
        account_id: account_id.to_owned(),
        key: window.key.clone(),
        range,
    })
}
