//! 遍历代理找 state：逐个出口发真实请求，首次拿到符合长度规则的 state 即绑定该出口并钉住。
//!
//! 探测只替换单次请求的出口，不改账号已保存的绑定；只有命中后才提交一次绑定。
//! state 的值始终留在 Provider 内，这里只看得到字节数。

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use gateway_core::{
    account::{OutboundProxy, ProviderAccountId},
    engine::{
        DiagnosticEgress,
        probe::{AccountProbeError, AccountProbeRequest},
    },
    error::GatewayErrorKind,
    event::ProviderResponseHeader,
    upstream::UpstreamSendState,
};

use super::{
    accounts::{AccountsService, CONNECTION_TEST_INPUT, DefaultAccountsService},
    map_provider_error, map_store_error,
};
use crate::{
    model::{
        AdminError, PageSize,
        accounts::{
            BatchUpdateAccounts, TurnStateHuntAttemptError, TurnStateHuntCommand,
            TurnStateHuntEgress, TurnStateHuntEvent, TurnStateHuntEventStream,
        },
        proxies::{AccountProxySelection, ProxyListQuery},
    },
    ports::provider::{ProviderAdmin, TurnStateHuntTicket},
};

/// 账号忙或请求间隔未到时的等待；这类尝试没有发往上游，不消耗次数。
const BUSY_RETRY_DELAY: Duration = Duration::from_secs(2);
const MAX_BUSY_RETRIES: u8 = 5;
/// 同一出口连续失败到此数即认为出口不通，换下一个。
const MAX_EGRESS_FAILURES: u8 = 2;

/// 同一账号同时只允许一个遍历任务；离开作用域即释放。
pub(super) type ActiveHunts = Arc<Mutex<BTreeSet<ProviderAccountId>>>;

struct HuntGuard {
    hunts: ActiveHunts,
    account_id: ProviderAccountId,
}

impl Drop for HuntGuard {
    fn drop(&mut self) {
        if let Ok(mut hunts) = self.hunts.lock() {
            hunts.remove(&self.account_id);
        }
    }
}

struct Egress {
    view: TurnStateHuntEgress,
    /// 账号当前已保存的绑定是否就是这个出口。
    bound: bool,
}

struct Hunt {
    service: DefaultAccountsService,
    provider: Arc<dyn ProviderAdmin>,
    command: TurnStateHuntCommand,
    ticket: TurnStateHuntTicket,
    request: AccountProbeRequest,
    events: tokio::sync::mpsc::Sender<TurnStateHuntEvent>,
    requests: u32,
}

enum Outcome {
    Continue,
    Stop,
}

impl DefaultAccountsService {
    pub(super) async fn start_turn_state_hunt(
        &self,
        command: TurnStateHuntCommand,
    ) -> Result<TurnStateHuntEventStream, AdminError> {
        if command.attempts == 0 || command.attempts > TurnStateHuntCommand::MAX_ATTEMPTS {
            return Err(AdminError::invalid("每个代理的尝试次数必须在 1 到 20 之间"));
        }
        let (stored, provider) = self.provider_for_account(&command.account_id).await?;
        let ticket = provider
            .turn_state_hunt_prepare(&command.account_id, &command.upstream_model)
            .await
            .map_err(|error| map_provider_error(error, "provider turn state hunt"))?;
        let operation = provider
            .connection_test_operation(&command.upstream_model, CONNECTION_TEST_INPUT)
            .map_err(|error| map_provider_error(error, "provider turn state hunt"))?;
        let egresses = self
            .hunt_egresses(
                stored.account.outbound_proxy.as_ref(),
                command.include_direct,
            )
            .await?;
        if egresses.is_empty() {
            return Err(AdminError::invalid(
                "没有已通过测试的代理，请先在代理页测试通过后再遍历",
            ));
        }
        let guard = {
            let mut hunts = self
                .hunts
                .lock()
                .map_err(|_| AdminError::internal("遍历任务状态不可用"))?;
            if !hunts.insert(command.account_id.clone()) {
                return Err(AdminError::conflict("该账号已有遍历任务在运行"));
            }
            HuntGuard {
                hunts: Arc::clone(&self.hunts),
                account_id: command.account_id.clone(),
            }
        };
        let (events, receiver) = tokio::sync::mpsc::channel(64);
        let hunt = Hunt {
            service: self.clone(),
            provider,
            request: AccountProbeRequest {
                account_id: command.account_id.clone(),
                provider_kind: stored.account.provider_kind,
                upstream_model: command.upstream_model.clone(),
                operation,
                egress: None,
            },
            command,
            ticket,
            events,
            requests: 0,
        };
        // 独立任务：页面断开只会让遍历在下一次尝试前停下，命中后的「绑定 + 钉住」不会被半途丢弃。
        tokio::spawn(async move {
            let _guard = guard;
            hunt.run(egresses).await;
        });
        Ok(Box::pin(futures::stream::unfold(
            receiver,
            |mut receiver| async move { receiver.recv().await.map(|event| (event, receiver)) },
        )))
    }

    /// 已通过测试的出口；账号当前绑定的排最前，重跑时最先验证它。
    async fn hunt_egresses(
        &self,
        bound: Option<&OutboundProxy>,
        include_direct: bool,
    ) -> Result<Vec<Egress>, AdminError> {
        let page_size =
            PageSize::new(PageSize::MAX).map_err(|_| AdminError::internal("分页大小不合法"))?;
        let mut egresses = Vec::new();
        let mut page = 1;
        loop {
            let listed = self
                .proxies
                .list(ProxyListQuery {
                    page,
                    page_size,
                    search: String::new(),
                })
                .await
                .map_err(|error| map_store_error(error, "proxies"))?;
            let fetched = listed.items.len();
            egresses.extend(
                listed
                    .items
                    .into_iter()
                    .filter(|record| record.last_test.as_ref().is_some_and(|test| test.success))
                    .map(|record| Egress {
                        bound: bound == Some(&record.proxy),
                        view: TurnStateHuntEgress {
                            proxy_id: Some(record.id),
                            name: record.name,
                            endpoint: Some(record.proxy.endpoint()),
                        },
                    }),
            );
            if fetched < usize::from(PageSize::MAX) {
                break;
            }
            page += 1;
        }
        if include_direct {
            egresses.push(Egress {
                bound: bound.is_none(),
                view: TurnStateHuntEgress {
                    proxy_id: None,
                    name: "直连".to_owned(),
                    endpoint: None,
                },
            });
        }
        egresses.sort_by_key(|egress| !egress.bound);
        Ok(egresses)
    }
}

impl Hunt {
    async fn run(mut self, egresses: Vec<Egress>) {
        tracing::info!(
            target: "turn_state_hunt",
            account_id = self.command.account_id.as_str(),
            upstream_model = self.command.upstream_model.as_str(),
            expected_length = self.ticket.expected_length(),
            egresses = egresses.len(),
            attempts = self.command.attempts,
            "遍历代理找 state 开始"
        );
        self.emit(TurnStateHuntEvent::Started {
            model: self.command.upstream_model.as_str().to_owned(),
            expected_length: self.ticket.expected_length(),
            attempts: self.command.attempts,
            egresses: egresses.iter().map(|egress| egress.view.clone()).collect(),
        })
        .await;
        let total = egresses.len();
        for (index, egress) in egresses.into_iter().enumerate() {
            if self.events.is_closed() {
                tracing::info!(
                    target: "turn_state_hunt",
                    account_id = self.command.account_id.as_str(),
                    requests = self.requests,
                    "遍历代理找 state 已被页面取消"
                );
                return;
            }
            if matches!(self.try_egress(egress, index, total).await, Outcome::Stop) {
                return;
            }
        }
        tracing::info!(
            target: "turn_state_hunt",
            account_id = self.command.account_id.as_str(),
            requests = self.requests,
            "遍历代理找 state 结束：所有出口都没有返回符合规则的 state"
        );
        self.emit(TurnStateHuntEvent::Completed {
            success: false,
            requests: self.requests,
        })
        .await;
    }

    async fn try_egress(&mut self, egress: Egress, index: usize, total: usize) -> Outcome {
        let proxy_id = egress.view.proxy_id.clone();
        // 列表之后代理可能被删除、改地址或测试失效，逐个重新读取。
        let (proxy, location) = match &proxy_id {
            None => (None, None),
            Some(id) => match self.service.proxies.get(id).await {
                Ok(record) if record.last_test.as_ref().is_some_and(|test| test.success) => {
                    (Some(record.proxy), record.location)
                }
                _ => {
                    self.emit(TurnStateHuntEvent::EgressFinished {
                        proxy_id,
                        attempts: 0,
                        matched: false,
                        skipped: Some("unavailable"),
                    })
                    .await;
                    return Outcome::Continue;
                }
            },
        };
        self.emit(TurnStateHuntEvent::EgressStarted {
            egress: egress.view.clone(),
            index,
            total,
        })
        .await;
        let mut lengths = Vec::new();
        let (mut used, mut busy, mut failures) = (0_u8, 0_u8, 0_u8);
        let mut skipped = None;
        while used < self.command.attempts {
            if self.events.is_closed() {
                return Outcome::Stop;
            }
            let captured_at = SystemTime::now();
            let result = self
                .service
                .probe
                .probe(AccountProbeRequest {
                    egress: Some(DiagnosticEgress::new(proxy.clone(), location.clone())),
                    ..self.request.clone()
                })
                .await;
            let error = match result {
                Ok(result) => {
                    self.requests += 1;
                    used += 1;
                    (busy, failures) = (0, 0);
                    let observed = self
                        .provider
                        .turn_state_hunt_inspect(&self.ticket, &result.response_headers);
                    lengths.push(observed.length);
                    self.emit(TurnStateHuntEvent::Attempt {
                        proxy_id: proxy_id.clone(),
                        index: used,
                        length: observed.length,
                        matched: observed.matched,
                        error: None,
                    })
                    .await;
                    if observed.matched {
                        self.log_egress(&egress.view, &lengths, true);
                        self.emit(TurnStateHuntEvent::EgressFinished {
                            proxy_id: proxy_id.clone(),
                            attempts: used,
                            matched: true,
                            skipped: None,
                        })
                        .await;
                        self.finalize(&egress, used, &result.response_headers, captured_at)
                            .await;
                        return Outcome::Stop;
                    }
                    continue;
                }
                Err(error) => error,
            };
            if not_sent_because_busy(&error) {
                busy += 1;
                if busy >= MAX_BUSY_RETRIES {
                    self.fail("account_busy", "账号正被线上流量占用，请稍后再遍历")
                        .await;
                    return Outcome::Stop;
                }
                tokio::time::sleep(BUSY_RETRY_DELAY).await;
                continue;
            }
            self.requests += 1;
            used += 1;
            lengths.push(None);
            let upstream_status = error
                .upstream_response()
                .map(gateway_core::engine::probe::AccountProbeUpstreamResponse::status);
            self.emit(TurnStateHuntEvent::Attempt {
                proxy_id: proxy_id.clone(),
                index: used,
                length: None,
                matched: false,
                error: Some(TurnStateHuntAttemptError {
                    code: error.kind(),
                    source: error.source(),
                    upstream_status,
                    message: error.client_message().to_owned(),
                }),
            })
            .await;
            if is_account_level_failure(&error, upstream_status) {
                self.log_egress(&egress.view, &lengths, false);
                // 这些失败换出口也不会好，继续只会白耗额度。
                self.fail("account_rejected", "上游拒绝了该账号的请求，遍历已中止")
                    .await;
                return Outcome::Stop;
            }
            failures += 1;
            if failures >= MAX_EGRESS_FAILURES {
                skipped = Some("unreachable");
                break;
            }
        }
        self.log_egress(&egress.view, &lengths, false);
        self.emit(TurnStateHuntEvent::EgressFinished {
            proxy_id,
            attempts: used,
            matched: false,
            skipped,
        })
        .await;
        Outcome::Continue
    }

    /// 先绑后钉：state 是在这个出口上观测到的，只有账号确实走这个出口之后才值得钉。
    async fn finalize(
        &mut self,
        egress: &Egress,
        attempt_index: u8,
        response_headers: &[ProviderResponseHeader],
        captured_at: SystemTime,
    ) {
        let account_id = self.command.account_id.clone();
        let proxy_id = egress.view.proxy_id.clone();
        self.emit(TurnStateHuntEvent::Hit {
            proxy_id: proxy_id.clone(),
            attempt_index,
            length: self.ticket.expected_length(),
        })
        .await;
        // 遍历期间凭据被刷新或重新捕获，观测到的 state 不再属于当前凭据。
        match self
            .provider
            .turn_state_hunt_prepare(&account_id, &self.command.upstream_model)
            .await
        {
            Ok(current) if current == self.ticket => {}
            _ => {
                self.fail("credential_changed", "凭据在遍历期间已变化，请重新遍历")
                    .await;
                return;
            }
        }
        if !egress.bound {
            let selection = proxy_id
                .clone()
                .map_or(AccountProxySelection::Direct, AccountProxySelection::Saved);
            let bound = self
                .service
                .batch_update(
                    &self.command.context,
                    BatchUpdateAccounts {
                        account_ids: vec![account_id.as_str().to_owned()],
                        enabled: None,
                        concurrency_limit: None,
                        weight: None,
                        model_access: None,
                        group_ids: None,
                        outbound_proxy: Some(selection),
                    },
                )
                .await;
            if let Err(error) = bound {
                tracing::warn!(
                    target: "turn_state_hunt",
                    account_id = account_id.as_str(),
                    error_kind = ?error.kind(),
                    "遍历命中但绑定代理失败，未钉住 state"
                );
                self.fail("bind_failed", "绑定代理失败，未钉住 state，请刷新后重试")
                    .await;
                return;
            }
        }
        tracing::info!(
            target: "turn_state_hunt",
            account_id = account_id.as_str(),
            proxy_id = proxy_id.as_deref().unwrap_or("direct"),
            changed = !egress.bound,
            "遍历命中，账号已绑定到该出口"
        );
        self.emit(TurnStateHuntEvent::Bound {
            proxy_id: proxy_id.clone(),
            changed: !egress.bound,
        })
        .await;
        match self
            .provider
            .turn_state_hunt_pin(&self.ticket, response_headers, captured_at)
            .await
        {
            Ok(expires_at) => {
                tracing::info!(
                    target: "turn_state_hunt",
                    account_id = account_id.as_str(),
                    upstream_model = self.command.upstream_model.as_str(),
                    length = self.ticket.expected_length(),
                    requests = self.requests,
                    "遍历命中的 state 已钉为账号级 state"
                );
                self.emit(TurnStateHuntEvent::Pinned {
                    model: self.command.upstream_model.as_str().to_owned(),
                    length: self.ticket.expected_length(),
                    expires_at: expires_at.into(),
                })
                .await;
                self.emit(TurnStateHuntEvent::Completed {
                    success: true,
                    requests: self.requests,
                })
                .await;
            }
            Err(error) => {
                tracing::warn!(
                    target: "turn_state_hunt",
                    account_id = account_id.as_str(),
                    error_kind = ?error.kind(),
                    "出口已绑定，但钉住 state 失败"
                );
                // 出口已被证明可用，保持绑定；重跑时它排第一。
                self.fail(
                    "pin_failed",
                    error
                        .public_message()
                        .unwrap_or("出口已绑定，但钉住 state 失败，请重新遍历"),
                )
                .await;
            }
        }
    }

    fn log_egress(&self, egress: &TurnStateHuntEgress, lengths: &[Option<usize>], matched: bool) {
        tracing::info!(
            target: "turn_state_hunt",
            account_id = self.command.account_id.as_str(),
            proxy_id = egress.proxy_id.as_deref().unwrap_or("direct"),
            proxy_name = egress.name.as_str(),
            ?lengths,
            matched,
            "遍历代理找 state：出口结果"
        );
    }

    async fn fail(&self, code: &'static str, message: &str) {
        tracing::warn!(
            target: "turn_state_hunt",
            account_id = self.command.account_id.as_str(),
            code,
            requests = self.requests,
            "遍历代理找 state 中止"
        );
        self.emit(TurnStateHuntEvent::Failed {
            code,
            message: message.to_owned(),
        })
        .await;
    }

    /// 页面已断开时事件无人接收；遍历是否继续由调用方在下一次尝试前判断。
    async fn emit(&self, event: TurnStateHuntEvent) {
        let _ = self.events.send(event).await;
    }
}

fn not_sent_because_busy(error: &AccountProbeError) -> bool {
    matches!(
        error.kind(),
        GatewayErrorKind::AccountCapacityUnavailable
            | GatewayErrorKind::NoAvailableProvider
            | GatewayErrorKind::ConcurrencyQueueFull
            | GatewayErrorKind::ConcurrencyQueueTimeout
    ) && matches!(error.send_state(), None | Some(UpstreamSendState::NotSent))
}

fn is_account_level_failure(error: &AccountProbeError, upstream_status: Option<u16>) -> bool {
    // 403 常见于出口 IP 被拒，按出口问题处理。
    if upstream_status == Some(403) {
        return false;
    }
    matches!(upstream_status, Some(401 | 429))
        || matches!(
            error.kind(),
            GatewayErrorKind::Unauthorized
                | GatewayErrorKind::RateLimited
                | GatewayErrorKind::ModelNotFound
                | GatewayErrorKind::PolicyDenied
        )
}
