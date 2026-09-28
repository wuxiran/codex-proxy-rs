//! fork 的管理端装配：运行目录、fork 服务与后台任务。
//!
//! 上游 `lib.rs` 只保留带 `fork:` 标记的接入行，fork 对组合根的扩展集中在这里。

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use gateway_core::task::{
    WorkerContribution, WorkerId, WorkerKind, WorkerLeaseRequest, WorkerRegistration,
    WorkerRunnable, WorkerSchedule,
};
use serde_json::{Map, Value};

use crate::{
    AdminServices, OpenAiService, PublicImportService, freeze_recovery,
    model::{
        AdminError, MutationContext,
        public_import::{
            PublicImportConfig, PublicImportEntry, PublicImportResult, PublicTicketImport,
            UpdatePublicImportConfig,
        },
    },
    ops_report,
    ports::{
        proxy::ProxyStore,
        store::{AdminStorePorts, SettingsStore},
    },
    ticket_revive, turn_state_renewal, use_case,
};

/// fork 在 runtime 数据目录下使用的目录（蓝绿槽位共享）。
#[derive(Debug, Clone)]
pub struct ForkRuntimePorts {
    /// 免登录导入入口的配置目录。
    pub public_import_dir: PathBuf,
    /// 登录票据加密密钥所在目录。
    pub account_ticket_dir: PathBuf,
    /// 经营日报快照目录。
    pub ops_report_dir: PathBuf,
}

impl ForkRuntimePorts {
    /// 按约定的子目录名从 runtime 数据目录派生。
    #[must_use]
    pub fn under(runtime_data_dir: &Path) -> Self {
        Self {
            public_import_dir: runtime_data_dir.join("public_import"),
            account_ticket_dir: runtime_data_dir.join("account_tickets"),
            ops_report_dir: runtime_data_dir.join("ops_report"),
        }
    }
}

/// 账号用例的 fork 依赖：运行设置、代理目录与票据密钥目录。
pub(crate) struct AccountsDeps {
    pub(crate) settings: Arc<dyn SettingsStore>,
    pub(crate) proxies: Arc<dyn ProxyStore>,
    pub(crate) ticket_dir: PathBuf,
}

impl AccountsDeps {
    pub(crate) fn new(store: &AdminStorePorts, ports: &ForkRuntimePorts) -> Self {
        Self {
            settings: store.settings(),
            proxies: store.proxies(),
            ticket_dir: ports.account_ticket_dir.clone(),
        }
    }
}

/// fork 服务。组合根先以未接入的占位值建出 `AdminServices`，[`attach`] 在上游服务
/// 建好后换成真实实现；占位值只返回「不可用」，不会 panic。
#[derive(Clone)]
pub(crate) struct ForkServices {
    public_import: Arc<dyn PublicImportService>,
    ops_report: Arc<ops_report::OpsReportService>,
}

impl Default for ForkServices {
    fn default() -> Self {
        Self {
            public_import: Arc::new(DetachedPublicImport),
            ops_report: Arc::new(ops_report::OpsReportService::new(None, PathBuf::new())),
        }
    }
}

impl AdminServices {
    #[must_use]
    pub fn ops_report(&self) -> &ops_report::OpsReportService {
        self.fork.ops_report.as_ref()
    }

    #[must_use]
    pub fn public_import(&self) -> &dyn PublicImportService {
        self.fork.public_import.as_ref()
    }

    /// 票据恢复、免登录导入等 OpenAI 固定入口；每次操作从已发布目录冻结 openai 实现。
    #[must_use]
    pub fn openai(&self) -> &dyn OpenAiService {
        self.credentials.as_ref()
    }
}

/// 接入 fork 服务并登记 fork 后台任务；复用上游已建好的代理、分组、账号与凭据用例。
pub(crate) fn attach(
    mut services: AdminServices,
    store: &AdminStorePorts,
    ports: ForkRuntimePorts,
    worker_contributions: &mut Vec<WorkerContribution>,
) -> Result<AdminServices, AdminError> {
    // 票据复活、免登录导入与票据恢复按 OpenAI 固定入口消费通用凭据用例。
    let openai = Arc::clone(&services.credentials) as Arc<dyn OpenAiService>;
    let accounts = Arc::clone(&services.accounts);
    let ops_report = Arc::new(ops_report::OpsReportService::new(
        store.ops_report(),
        ports.ops_report_dir,
    ));
    services.fork = ForkServices {
        public_import: Arc::new(use_case::public_import::DefaultPublicImportService::new(
            ports.public_import_dir,
            Arc::clone(&openai),
            Arc::clone(&services.proxies),
            Arc::clone(&services.account_groups),
            Arc::clone(&accounts),
        )),
        ops_report: Arc::clone(&ops_report),
    };
    worker_contributions.extend(turn_state_renewal_worker_contribution(
        turn_state_renewal::TurnStateRenewalTask::new(Arc::clone(&accounts)),
    )?);
    worker_contributions.extend(ticket_revive_worker_contribution(
        ticket_revive::TicketReviveTask::new(accounts, openai, store.accounts()),
    )?);
    worker_contributions.extend(ops_report_worker_contribution(ops_report::OpsReportTask(
        ops_report,
    ))?);
    Ok(services)
}

/// state 自动续期 Worker 注册。
///
/// 归在账号自动维护这一类（与冻结恢复同 kind、不同 owner）。不带租约：state 只存在于
/// 进程内存，每个实例都要各自续，跨实例选主反而会让没拿到租约的实例一直没有 state。
fn turn_state_renewal_worker_contribution(
    task: turn_state_renewal::TurnStateRenewalTask,
) -> Result<Vec<WorkerContribution>, AdminError> {
    let id = WorkerId::try_new(
        WorkerKind::AccountFreezeRecovery,
        turn_state_renewal::TURN_STATE_RENEWAL_WORKER_OWNER,
    )
    .map_err(|_| AdminError::internal("state 续期 Worker ID 不合法"))?;
    let schedule = WorkerSchedule::try_new(
        turn_state_renewal::TURN_STATE_RENEWAL_INTERVAL,
        turn_state_renewal::WORKER_INITIAL_BACKOFF,
        turn_state_renewal::WORKER_MAXIMUM_BACKOFF,
        freeze_recovery::WORKER_LEASE_TTL,
        freeze_recovery::WORKER_LEASE_RENEWAL,
    )
    .map_err(|_| AdminError::internal("state 续期 Worker 调度配置不合法"))?;
    let registration = WorkerRegistration::try_new(
        id,
        WorkerRunnable::Scheduled {
            schedule,
            lease: None,
            task: Box::new(task),
        },
    )
    .map_err(|_| AdminError::internal("state 续期 Worker 注册信息不合法"))?;
    Ok(vec![WorkerContribution::Registration(registration)])
}

/// 票据自动复活 Worker 注册：加跨实例租约，同一时刻只有一个实例对失效账号登录。
fn ticket_revive_worker_contribution(
    task: ticket_revive::TicketReviveTask,
) -> Result<Vec<WorkerContribution>, AdminError> {
    let id = WorkerId::try_new(
        WorkerKind::AccountFreezeRecovery,
        ticket_revive::TICKET_REVIVE_WORKER_OWNER,
    )
    .map_err(|_| AdminError::internal("票据复活 Worker ID 不合法"))?;
    let schedule = WorkerSchedule::try_new(
        ticket_revive::TICKET_REVIVE_INTERVAL,
        ticket_revive::WORKER_INITIAL_BACKOFF,
        ticket_revive::WORKER_MAXIMUM_BACKOFF,
        freeze_recovery::WORKER_LEASE_TTL,
        freeze_recovery::WORKER_LEASE_RENEWAL,
    )
    .map_err(|_| AdminError::internal("票据复活 Worker 调度配置不合法"))?;
    let lease = WorkerLeaseRequest::try_new(id.clone(), freeze_recovery::WORKER_LEASE_TTL)
        .map_err(|_| AdminError::internal("票据复活 Worker 租约配置不合法"))?;
    let registration = WorkerRegistration::try_new(
        id,
        WorkerRunnable::Scheduled {
            schedule,
            lease: Some(lease),
            task: Box::new(task),
        },
    )
    .map_err(|_| AdminError::internal("票据复活 Worker 注册信息不合法"))?;
    Ok(vec![WorkerContribution::Registration(registration)])
}

/// 经营日报 Worker 注册：加跨实例租约，同一时刻只有一个实例写快照。
fn ops_report_worker_contribution(
    task: ops_report::OpsReportTask,
) -> Result<Vec<WorkerContribution>, AdminError> {
    let id = WorkerId::try_new(
        WorkerKind::AccountFreezeRecovery,
        ops_report::OPS_REPORT_WORKER_OWNER,
    )
    .map_err(|_| AdminError::internal("经营日报 Worker ID 不合法"))?;
    let schedule = WorkerSchedule::try_new(
        ops_report::OPS_REPORT_INTERVAL,
        ops_report::WORKER_INITIAL_BACKOFF,
        ops_report::WORKER_MAXIMUM_BACKOFF,
        freeze_recovery::WORKER_LEASE_TTL,
        freeze_recovery::WORKER_LEASE_RENEWAL,
    )
    .map_err(|_| AdminError::internal("经营日报 Worker 调度配置不合法"))?;
    let lease = WorkerLeaseRequest::try_new(id.clone(), freeze_recovery::WORKER_LEASE_TTL)
        .map_err(|_| AdminError::internal("经营日报 Worker 租约配置不合法"))?;
    let registration = WorkerRegistration::try_new(
        id,
        WorkerRunnable::Scheduled {
            schedule,
            lease: Some(lease),
            task: Box::new(task),
        },
    )
    .map_err(|_| AdminError::internal("经营日报 Worker 注册信息不合法"))?;
    Ok(vec![WorkerContribution::Registration(registration)])
}

/// [`attach`] 之前的占位实现。
struct DetachedPublicImport;

fn detached<T>() -> Result<T, AdminError> {
    Err(AdminError::unavailable("免登录导入尚未初始化"))
}

#[async_trait]
impl PublicImportService for DetachedPublicImport {
    async fn list(&self) -> Result<Vec<PublicImportConfig>, AdminError> {
        detached()
    }

    async fn create(
        &self,
        _: &MutationContext,
        _: UpdatePublicImportConfig,
    ) -> Result<PublicImportConfig, AdminError> {
        detached()
    }

    async fn update(
        &self,
        _: &MutationContext,
        _: &str,
        _: UpdatePublicImportConfig,
    ) -> Result<PublicImportConfig, AdminError> {
        detached()
    }

    async fn delete(&self, _: &MutationContext, _: &str) -> Result<(), AdminError> {
        detached()
    }

    async fn rotate_token(
        &self,
        _: &MutationContext,
        _: &str,
    ) -> Result<PublicImportConfig, AdminError> {
        detached()
    }

    async fn entry(&self, _: &str) -> Result<Option<PublicImportEntry>, AdminError> {
        detached()
    }

    async fn import(
        &self,
        _: &str,
        _: &str,
        _: Map<String, Value>,
    ) -> Result<Option<PublicImportResult>, AdminError> {
        detached()
    }

    async fn import_tickets(
        &self,
        _: &str,
        _: &str,
        _: PublicTicketImport,
    ) -> Result<Option<PublicImportResult>, AdminError> {
        detached()
    }
}
