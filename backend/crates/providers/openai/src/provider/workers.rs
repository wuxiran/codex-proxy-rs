//! OpenAI Provider 向 Host 贡献的后台 worker。

use super::*;
use crate::transport::profile::cli_release::CliReleaseService;
use crate::transport::profile::platform_release::PlatformDesktopReleaseService;

pub(crate) struct ClientReleaseServices {
    pub desktop: Arc<CodexDesktopReleaseService>,
    pub cli: Arc<CliReleaseService>,
    pub platforms: Arc<PlatformDesktopReleaseService>,
}

pub(super) const WORKER_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
pub(super) const WORKER_MAXIMUM_BACKOFF: Duration = Duration::from_secs(60);
pub(super) const WORKER_LEASE_TTL: Duration = Duration::from_secs(15 * 60);
pub(super) const WORKER_LEASE_RENEWAL: Duration = Duration::from_secs(5 * 60);
pub(super) const OAUTH_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
pub(super) const QUOTA_CHECK_INTERVAL: Duration = Duration::from_secs(30);
pub(super) const DESKTOP_RELEASE_WORKER_OWNER: &str = "openai-desktop-release";
pub(super) const MODEL_ETAG_WORKER_OWNER: &str = "openai-model-etag";
pub(super) const MODEL_CATALOG_WORKER_OWNER: &str = "openai-model-catalog";
pub(super) const OAUTH_REVIVE_WORKER_OWNER: &str = "openai-oauth-revive";
pub(super) const OAUTH_REVIVE_INTERVAL: Duration = Duration::from_secs(60);
pub(super) const CLOUD_MINT_WORKER_OWNER: &str = "openai-cloud-mint";
/// 票只有 ~240s，续打要比到期余量（60s）扫得更勤。
pub(super) const CLOUD_MINT_INTERVAL: Duration = Duration::from_secs(20);
pub(super) const WARM_POOL_WORKER_OWNER: &str = "openai-ws-warm-pool";
/// WS 保活：补齐/低频复探，20s 扫一轮（也可被账号导入事件唤醒）。
pub(super) const WARM_POOL_INTERVAL: Duration = Duration::from_secs(20);

// 每个参数都是独立注入的服务，打包成结构体只会多一层无意义的搬运。
#[expect(clippy::too_many_arguments)]
pub(crate) fn worker_contributions(
    refresh: Arc<CodexCredentialRefreshService>,
    quota: Arc<CodexCredentialQuotaService>,
    catalog: Arc<CodexCredentialCatalogService>,
    quota_refresh_policy: CodexQuotaRefreshPolicy,
    oauth_refresh_enabled: bool,
    releases: ClientReleaseServices,
    revive: Arc<crate::credential::CodexReviveService>,
    cloud_mint: Arc<crate::turn_state_mint::CloudMintService>,
    ws_warm_pool: Arc<crate::ws_warm_pool::WarmPoolService>,
) -> Result<Vec<WorkerContribution>, WorkerDefinitionError> {
    let refresh_id = WorkerId::try_new(WorkerKind::OAuthRefresh, PROVIDER_NAME)?;
    let quota_id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, PROVIDER_NAME)?;
    let catalog_id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, MODEL_CATALOG_WORKER_OWNER)?;
    let etag_id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, MODEL_ETAG_WORKER_OWNER)?;
    let desktop_release_id =
        WorkerId::try_new(WorkerKind::QuotaCatalogHealth, DESKTOP_RELEASE_WORKER_OWNER)?;
    let cli_release_id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, "openai-cli-release")?;
    let mut contributions = Vec::new();
    if oauth_refresh_enabled {
        contributions.push(WorkerContribution::Registration(scheduled_registration(
            refresh_id,
            OAUTH_REFRESH_INTERVAL,
            Box::new(OpenAiOAuthRefreshTask { service: refresh }),
        )?));
    }
    if revive.enabled() {
        let revive_id = WorkerId::try_new(WorkerKind::OAuthRefresh, OAUTH_REVIVE_WORKER_OWNER)?;
        contributions.push(WorkerContribution::Registration(scheduled_registration(
            revive_id,
            OAUTH_REVIVE_INTERVAL,
            Box::new(OpenAiOAuthReviveTask { service: revive }),
        )?));
    }
    contributions.push(WorkerContribution::Registration(scheduled_registration(
        WorkerId::try_new(WorkerKind::QuotaCatalogHealth, CLOUD_MINT_WORKER_OWNER)?,
        CLOUD_MINT_INTERVAL,
        Box::new(OpenAiCloudMintTask {
            service: cloud_mint,
        }),
    )?));
    contributions.push(WorkerContribution::Registration(
        WorkerRegistration::try_new(
            WorkerId::try_new(WorkerKind::QuotaCatalogHealth, WARM_POOL_WORKER_OWNER)?,
            WorkerRunnable::Daemon {
                restart: DaemonRestartPolicy::try_new(
                    WORKER_INITIAL_BACKOFF,
                    WORKER_MAXIMUM_BACKOFF,
                )?,
                task: Box::new(OpenAiWarmPoolTask {
                    service: ws_warm_pool,
                }),
            },
        )?,
    ));
    contributions.extend([
        WorkerContribution::Registration(scheduled_registration(
            WorkerId::try_new(
                WorkerKind::QuotaCatalogHealth,
                "openai-platform-desktop-release",
            )?,
            APPCAST_POLL_INTERVAL,
            Box::new(OpenAiPlatformDesktopReleaseTask {
                service: releases.platforms,
            }),
        )?),
        WorkerContribution::Registration(scheduled_registration(
            cli_release_id,
            APPCAST_POLL_INTERVAL,
            Box::new(OpenAiCliReleaseTask {
                service: releases.cli,
            }),
        )?),
        WorkerContribution::Registration(scheduled_registration(
            quota_id,
            QUOTA_CHECK_INTERVAL,
            Box::new(OpenAiQuotaTask { quota }),
        )?),
        WorkerContribution::Registration(scheduled_registration(
            catalog_id,
            quota_refresh_policy.interval(),
            Box::new(OpenAiCatalogTask {
                catalog: Arc::clone(&catalog),
            }),
        )?),
        WorkerContribution::Registration(WorkerRegistration::try_new(
            etag_id,
            WorkerRunnable::Daemon {
                restart: DaemonRestartPolicy::try_new(
                    WORKER_INITIAL_BACKOFF,
                    WORKER_MAXIMUM_BACKOFF,
                )?,
                task: Box::new(OpenAiCatalogEtagTask { catalog }),
            },
        )?),
        WorkerContribution::Registration(scheduled_registration(
            desktop_release_id,
            APPCAST_POLL_INTERVAL,
            Box::new(OpenAiDesktopReleaseTask {
                service: releases.desktop,
            }),
        )?),
    ]);
    Ok(contributions)
}

pub(super) fn scheduled_registration(
    id: WorkerId,
    interval: Duration,
    task: Box<dyn ScheduledTask>,
) -> Result<WorkerRegistration, WorkerDefinitionError> {
    let schedule = WorkerSchedule::try_new(
        interval,
        WORKER_INITIAL_BACKOFF,
        WORKER_MAXIMUM_BACKOFF,
        WORKER_LEASE_TTL,
        WORKER_LEASE_RENEWAL,
    )?;
    let lease = WorkerLeaseRequest::try_new(id.clone(), WORKER_LEASE_TTL)?;
    WorkerRegistration::try_new(
        id,
        WorkerRunnable::Scheduled {
            schedule,
            lease: Some(lease),
            task,
        },
    )
}

pub(super) struct OpenAiOAuthRefreshTask {
    service: Arc<CodexCredentialRefreshService>,
}

impl ScheduledTask for OpenAiOAuthRefreshTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            if context.cancellation().is_cancelled() {
                return Ok(());
            }
            let outcomes = self.service.refresh_due().await.map_err(|error| {
                tracing::error!(error = %error, "OpenAI OAuth refresh cycle failed");
                WorkerTaskError::safe("OpenAI OAuth refresh failed")
            })?;
            let mut refreshed = 0_u64;
            let mut invalidated = 0_u64;
            let mut banned = 0_u64;
            let mut transient = 0_u64;
            let mut lease_unavailable = 0_u64;
            let mut stale = 0_u64;
            let mut failed = 0_u64;
            let mut transient_accounts = Vec::new();
            let mut failed_accounts = Vec::new();
            for outcome in &outcomes {
                match outcome {
                    CodexCredentialRefreshOutcome::Refreshed { .. } => refreshed += 1,
                    CodexCredentialRefreshOutcome::Invalidated { .. } => invalidated += 1,
                    CodexCredentialRefreshOutcome::Banned { .. } => banned += 1,
                    CodexCredentialRefreshOutcome::Transient { account_id } => {
                        transient += 1;
                        transient_accounts.push(account_id);
                    }
                    CodexCredentialRefreshOutcome::LeaseUnavailable { .. } => {
                        lease_unavailable += 1;
                    }
                    CodexCredentialRefreshOutcome::Stale { .. } => stale += 1,
                    CodexCredentialRefreshOutcome::Failed { account_id } => {
                        failed += 1;
                        failed_accounts.push(account_id);
                    }
                }
            }
            if !outcomes.is_empty() {
                tracing::info!(
                    refreshed,
                    invalidated,
                    banned,
                    transient,
                    lease_unavailable,
                    stale,
                    failed,
                    "OpenAI OAuth refresh cycle completed"
                );
            }
            if transient > 0 || failed > 0 {
                tracing::warn!(
                    refreshed,
                    invalidated,
                    banned,
                    transient,
                    lease_unavailable,
                    stale,
                    failed,
                    transient_accounts = ?transient_accounts,
                    failed_accounts = ?failed_accounts,
                    "OpenAI OAuth refresh cycle contained operational failures"
                );
            }
            Ok(())
        })
    }
}

pub(super) struct OpenAiOAuthReviveTask {
    service: Arc<crate::credential::CodexReviveService>,
}

impl ScheduledTask for OpenAiOAuthReviveTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            if context.cancellation().is_cancelled() {
                return Ok(());
            }
            match self.service.run_cycle().await {
                Ok(summary)
                    if summary.applied > 0
                        || summary.failed > 0
                        || summary.skipped_unsigned > 0 =>
                {
                    tracing::info!(
                        applied = summary.applied,
                        failed = summary.failed,
                        skipped_unsigned = summary.skipped_unsigned,
                        cooled_down = summary.cooled_down,
                        "OpenAI signed-export 401 revive cycle completed"
                    );
                    Ok(())
                }
                Ok(_) => Ok(()),
                Err(error) => {
                    tracing::warn!(error = %error, "OpenAI signed-export 401 revive cycle failed");
                    Err(WorkerTaskError::safe("OpenAI 401 revive failed"))
                }
            }
        })
    }
}

/// 云端打票续打：设置未开启时每轮直接空转。
pub(super) struct OpenAiCloudMintTask {
    service: Arc<crate::turn_state_mint::CloudMintService>,
}

impl ScheduledTask for OpenAiCloudMintTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            if context.cancellation().is_cancelled() {
                return Ok(());
            }
            let minted = self.service.renew_cycle().await;
            if minted > 0 {
                tracing::info!(target: "turn_state", minted, "[turn-state] mint renew cycle");
            }
            Ok(())
        })
    }
}

pub(super) struct OpenAiQuotaTask {
    quota: Arc<CodexCredentialQuotaService>,
}

pub(super) struct OpenAiCatalogTask {
    catalog: Arc<CodexCredentialCatalogService>,
}

pub(super) struct OpenAiCatalogEtagTask {
    catalog: Arc<CodexCredentialCatalogService>,
}

pub(super) struct OpenAiWarmPoolTask {
    service: Arc<crate::ws_warm_pool::WarmPoolService>,
}

impl DaemonTask for OpenAiWarmPoolTask {
    fn run(
        &self,
        cancellation: gateway_core::lifecycle::CancellationToken,
    ) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let mut interval = tokio::time::interval(WARM_POOL_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    () = cancellation.cancelled() => return Ok(()),
                    () = self.service.wait_wake() => {},
                    _ = interval.tick() => {},
                };
                self.service.run_cycle().await;
            }
        })
    }
}

pub(super) struct OpenAiDesktopReleaseTask {
    service: Arc<CodexDesktopReleaseService>,
}

impl ScheduledTask for OpenAiDesktopReleaseTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let refresh = self.service.refresh();
            tokio::pin!(refresh);
            let result = tokio::select! {
                () = context.cancellation().cancelled() => return Ok(()),
                result = &mut refresh => result,
            };
            if let Err(error) = result {
                // 上游检查失败已经作为 Provider 观察事实保存；本周期本身正常完成，
                // 避免 Host 的短退避持续请求固定官方 appcast。
                tracing::warn!(error = %error, "OpenAI Desktop release check failed");
            }
            Ok(())
        })
    }
}

impl ScheduledTask for OpenAiQuotaTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            if context.cancellation().is_cancelled() {
                return Ok(());
            }
            match self.quota.synchronize().await {
                Ok(summary) if summary.has_operational_failures() => {
                    tracing::warn!(
                        updated = summary.updated,
                        exhausted = summary.exhausted,
                        banned = summary.banned,
                        transient = summary.transient,
                        stale = summary.stale,
                        "OpenAI quota cycle contained operational failures"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "OpenAI quota synchronization failed"
                    );
                    return Err(WorkerTaskError::safe("OpenAI quota synchronization failed"));
                }
            }
            Ok(())
        })
    }
}

impl ScheduledTask for OpenAiCatalogTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            if context.cancellation().is_cancelled() {
                return Ok(());
            }
            match self.catalog.refresh_catalogs().await {
                Ok(_) | Err(CodexCredentialCatalogError::NoEligibleCredential) => Ok(()),
                Err(error) => {
                    tracing::warn!(error = %error, "OpenAI model catalog refresh failed");
                    Err(WorkerTaskError::safe(
                        "OpenAI model catalog synchronization failed",
                    ))
                }
            }
        })
    }
}

impl DaemonTask for OpenAiCatalogEtagTask {
    fn run(
        &self,
        cancellation: gateway_core::lifecycle::CancellationToken,
    ) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            loop {
                tokio::select! {
                    () = cancellation.cancelled() => return Ok(()),
                    () = self.catalog.wait_for_etag_refresh() => {},
                };
                if let Err(error) = self.catalog.refresh().await {
                    tracing::warn!(
                        error = %error,
                        "OpenAI model catalog ETag refresh failed"
                    );
                }
            }
        })
    }
}

struct OpenAiCliReleaseTask {
    service: Arc<CliReleaseService>,
}

impl ScheduledTask for OpenAiCliReleaseTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            tokio::select! {
                () = context.cancellation().cancelled() => {},
                result = self.service.refresh() => {
                    if let Err(error) = result { tracing::warn!(error = %error, "OpenAI CLI release check failed"); }
                }
            }
            Ok(())
        })
    }
}

struct OpenAiPlatformDesktopReleaseTask {
    service: Arc<PlatformDesktopReleaseService>,
}
impl ScheduledTask for OpenAiPlatformDesktopReleaseTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            tokio::select! {
                () = context.cancellation().cancelled() => {},
                () = self.service.refresh() => {},
            }
            Ok(())
        })
    }
}
