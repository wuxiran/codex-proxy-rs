//! 账号级 state 的到期前自动续期：对临近到期（或本进程内缺失）的账号重新遍历代理。
//!
//! 续期复用管理端的遍历用例：账号当前绑定的出口排最前，续不上就继续打其它出口。
//! state 只存在于进程内存，所以本任务不加跨实例租约——每个实例各自维护自己的 state。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures::StreamExt as _;
use gateway_core::account::ProviderAccountId;
use gateway_core::task::{ScheduledTask, WorkerCycleContext, WorkerTaskError};

use crate::model::accounts::{TurnStateHuntCommand, TurnStateHuntEvent};
use crate::model::{MutationActor, MutationContext};
use crate::use_case::accounts::AccountsService;

pub const TURN_STATE_RENEWAL_INTERVAL: Duration = Duration::from_secs(60);
pub const TURN_STATE_RENEWAL_WORKER_OWNER: &str = "turn-state-renewal";
pub const WORKER_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
pub const WORKER_MAXIMUM_BACKOFF: Duration = Duration::from_secs(60);
/// 到期前这么久开始续；留出一整轮遍历的时间，旧 state 在新 state 钉住前继续生效。
pub const RENEWAL_MARGIN: Duration = Duration::from_secs(5 * 60);
/// 一整轮遍历都没续上后的等待：每轮最多「出口数 × 次数」个真实请求，不能每分钟重来。
const MISS_BACKOFF: Duration = Duration::from_secs(5 * 60);
/// 上游无容量是瞬时过载：退避必须远短于 [`RENEWAL_MARGIN`]，否则一次抖动就把到期前
/// 唯一的续期机会用掉，state 会在下一次重试之前过期。
const CAPACITY_BACKOFF: Duration = Duration::from_secs(60);
/// 上游拒绝账号（401/429 等）后的等待；换出口也不会好。
const REJECTED_BACKOFF: Duration = Duration::from_secs(15 * 60);
const SYSTEM_REQUEST_ID: &str = "turn-state-renewal";

pub struct TurnStateRenewalTask {
    accounts: Arc<dyn AccountsService>,
    /// 续期失败的账号在此时刻之前不再尝试。
    retry_after: Mutex<BTreeMap<ProviderAccountId, SystemTime>>,
}

impl TurnStateRenewalTask {
    pub fn new(accounts: Arc<dyn AccountsService>) -> Self {
        Self {
            accounts,
            retry_after: Mutex::new(BTreeMap::new()),
        }
    }

    fn system_context() -> MutationContext {
        MutationContext {
            actor: MutationActor::System,
            request_id: SYSTEM_REQUEST_ID.to_owned(),
        }
    }

    fn backing_off(&self, account_id: &ProviderAccountId, now: SystemTime) -> bool {
        self.retry_after
            .lock()
            .is_ok_and(|retry| retry.get(account_id).is_some_and(|until| *until > now))
    }

    fn settle(&self, account_id: &ProviderAccountId, backoff: Option<Duration>) {
        if let Ok(mut retry) = self.retry_after.lock() {
            match backoff {
                Some(delay) => retry.insert(account_id.clone(), SystemTime::now() + delay),
                None => retry.remove(account_id),
            };
        }
    }

    pub async fn run_cycle_inner(&self, context: &WorkerCycleContext) {
        let now = SystemTime::now();
        for renewal in self.accounts.turn_state_renewals(now, RENEWAL_MARGIN).await {
            if context.cancellation().is_cancelled() {
                return;
            }
            if self.backing_off(&renewal.account_id, now) {
                continue;
            }
            // 前面账号的整轮遍历可能耗时数分钟；轮到这个账号时它可能已被停用、
            // 关了续期，或已经被别处续上。以此刻的事实为准，而不是周期开始时的名单。
            // 同时换用此刻的参数：管理员可能刚改了模型、次数或是否含直连。
            let Some(renewal) = self
                .accounts
                .turn_state_renewals(SystemTime::now(), RENEWAL_MARGIN)
                .await
                .into_iter()
                .find(|current| current.account_id == renewal.account_id)
            else {
                continue;
            };
            let account_id = renewal.account_id.clone();
            let stream = self
                .accounts
                .renewal_turn_state_hunt(TurnStateHuntCommand {
                    account_id: renewal.account_id,
                    upstream_model: renewal.upstream_model,
                    attempts: renewal.attempts,
                    include_direct: renewal.include_direct,
                    // 有轮换代理模板时续期改走「自动撞」（见 renewal_turn_state_hunt）；
                    // 没有模板时回退遍历，此处传的就是遍历用的基础命令。
                    only_proxy_id: None,
                    ephemeral: None,
                    bind_to: None,
                    require_schedulable: true,
                    context: Self::system_context(),
                })
                .await;
            let backoff = match stream {
                // 管理员正在手动遍历同一账号等：下个周期再看，不计失败。
                Err(error) if error.kind() == crate::model::AdminErrorKind::Conflict => continue,
                Err(error) => {
                    tracing::warn!(
                        target: "turn_state_hunt",
                        account_id = account_id.as_str(),
                        error_kind = ?error.kind(),
                        "state 自动续期无法开始"
                    );
                    Some(MISS_BACKOFF)
                }
                Ok(stream) => match stream.collect::<Vec<_>>().await.last() {
                    Some(TurnStateHuntEvent::Completed { success: true, .. }) => None,
                    Some(TurnStateHuntEvent::Failed {
                        code: "account_rejected",
                        ..
                    }) => Some(REJECTED_BACKOFF),
                    Some(TurnStateHuntEvent::Failed {
                        code: "upstream_capacity",
                        ..
                    }) => Some(CAPACITY_BACKOFF),
                    // 停用是本地事实，不是上游的拒绝：不记退避。停用期间它本就不在到期名单里，
                    // 管理员重新启用后应当立刻恢复续期，而不是被一段旧退避挡住。
                    Some(TurnStateHuntEvent::Failed {
                        code: "account_unschedulable",
                        ..
                    }) => continue,
                    _ => Some(MISS_BACKOFF),
                },
            };
            tracing::info!(
                target: "turn_state_hunt",
                account_id = account_id.as_str(),
                renewed = backoff.is_none(),
                retry_after_secs = backoff.map(|delay| delay.as_secs()),
                "state 自动续期结束"
            );
            self.settle(&account_id, backoff);
        }
    }
}

impl ScheduledTask for TurnStateRenewalTask {
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
