//! 票据自动复活：令牌失效（expired/invalid）且存了票据的账号，自动用票据重新登录并写回。
//!
//! 每个账号每轮失效最多尝试 [`MAX_ATTEMPTS`] 次，两次之间冷却 [`RETRY_COOLDOWN`]；
//! 尝试次数记在 fork 子表里，成功、换票据或账号恢复正常后清零。任务加跨实例租约，
//! 蓝绿切换期间两个槽位不会同时对同一账号登录。

use std::sync::Arc;
use std::time::Duration;

use gateway_core::account::ProviderAccountId;
use gateway_core::task::{ScheduledTask, WorkerCycleContext, WorkerTaskError};

use crate::model::provider_credentials::{CredentialMutation, RotateCredential};
use crate::model::{MutationActor, MutationContext};
use crate::ports::store::AccountStore;
use crate::use_case::{accounts::AccountsService, openai::OpenAiService};

pub const TICKET_REVIVE_INTERVAL: Duration = Duration::from_secs(60);
pub const TICKET_REVIVE_WORKER_OWNER: &str = "ticket-auto-revive";
pub const WORKER_INITIAL_BACKOFF: Duration = Duration::from_secs(5);
pub const WORKER_MAXIMUM_BACKOFF: Duration = Duration::from_secs(300);
/// 每轮失效最多自动复活的次数。
pub const MAX_ATTEMPTS: i32 = 3;
/// 同一账号两次尝试之间的间隔；登录失败多为风控或临时故障，立刻重试只会更糟。
pub const RETRY_COOLDOWN: Duration = Duration::from_secs(10 * 60);
/// 每轮最多处理的账号数；登录要跑 PoW，sidecar 并发很小。
const BATCH: i64 = 5;
const SYSTEM_REQUEST_ID: &str = "ticket-auto-revive";

pub struct TicketReviveTask {
    accounts: Arc<dyn AccountsService>,
    openai: Arc<dyn OpenAiService>,
    store: Arc<dyn AccountStore>,
}

impl TicketReviveTask {
    #[must_use]
    pub fn new(
        accounts: Arc<dyn AccountsService>,
        openai: Arc<dyn OpenAiService>,
        store: Arc<dyn AccountStore>,
    ) -> Self {
        Self {
            accounts,
            openai,
            store,
        }
    }

    async fn run_cycle_inner(&self, context: &WorkerCycleContext) {
        let cooldown = chrono::Duration::from_std(RETRY_COOLDOWN).unwrap_or_default();
        let candidates = match self
            .store
            .ticket_revive_candidates(MAX_ATTEMPTS, chrono::Utc::now() - cooldown, BATCH)
            .await
        {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::warn!(target: "account_ticket", error = %error, "ticket revive candidates unavailable");
                return;
            }
        };
        for account_id in candidates {
            if context.cancellation().is_cancelled() {
                return;
            }
            let Ok(typed_id) = ProviderAccountId::new(account_id.clone()) else {
                continue;
            };
            let outcome = self.revive(typed_id).await;
            let error = outcome.as_ref().err().map(String::as_str);
            match error {
                None => tracing::info!(target: "account_ticket", account_id = %account_id,
                    "account revived from ticket"),
                Some(reason) => tracing::warn!(target: "account_ticket", account_id = %account_id,
                    reason, "ticket auto revive failed"),
            }
            if let Err(record_error) = self.store.record_ticket_revive(&account_id, error).await {
                tracing::warn!(target: "account_ticket", account_id = %account_id,
                    error = %record_error, "ticket revive result could not be recorded");
            }
        }
    }

    async fn revive(&self, account_id: ProviderAccountId) -> Result<(), String> {
        let material = self
            .accounts
            .ticket_restore_material(&account_id)
            .await
            .map_err(|error| error.message().to_owned())?;
        self.openai
            .rotate(RotateCredential {
                mutation: CredentialMutation {
                    context: MutationContext {
                        actor: MutationActor::System,
                        request_id: SYSTEM_REQUEST_ID.to_owned(),
                    },
                    account_id,
                },
                provider_material: material,
                settings: None,
            })
            .await
            .map(|_| ())
            .map_err(|error| error.message().to_owned())
    }
}

impl ScheduledTask for TicketReviveTask {
    fn run_cycle(
        &self,
        context: WorkerCycleContext,
    ) -> futures::future::BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            self.run_cycle_inner(&context).await;
            Ok(())
        })
    }
}
