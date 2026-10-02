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
    error::{GatewayErrorKind, ProviderErrorKind},
    event::ProviderResponseHeader,
    upstream::UpstreamSendState,
};

use super::{
    accounts::{AccountsService, CONNECTION_TEST_INPUT, DefaultAccountsService},
    map_provider_error, map_store_error,
};
use crate::{
    model::{
        AdminError, PageSize, Revision,
        accounts::{
            BatchUpdateAccounts, CredentialState, TurnStateHuntAttemptError, TurnStateHuntCommand,
            TurnStateHuntEgress, TurnStateHuntEvent, TurnStateHuntEventStream,
        },
        proxies::{AccountProxySelection, ProxyListQuery, ProxyRecord},
    },
    ports::provider::{ProviderAdmin, TurnStateHuntTicket},
};

/// 账号忙或请求间隔未到时的等待；这类尝试没有发往上游，不消耗次数。
const BUSY_RETRY_DELAY: Duration = Duration::from_secs(2);
const MAX_BUSY_RETRIES: u8 = 5;
/// 同一出口连续失败到此数即认为出口不通，换下一个。
const MAX_EGRESS_FAILURES: u8 = 2;
/// 上游的「该模型暂无容量」多是秒级的瞬时过载：歇一下再发往往就通。
const CAPACITY_RETRY_DELAY: Duration = Duration::from_secs(3);
/// 连续这么多次都被上游以无容量拒绝，才认定确实没有容量并中止整轮；
/// 少于它就继续遍历，避免一次抖动让整轮（尤其是自动续期）落空。
const MAX_CAPACITY_STREAK: u8 = 3;
/// 命中收尾时若发现令牌刚好刷新（凭据变了），重新取票继续撞，最多这么多次；
/// 繁忙的 Business 号令牌刷新频繁，整轮因此中止会让它永远绑不上 state。
const MAX_TICKET_REFRESHES: u8 = 3;

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
    /// 自动撞的临时出口：不来自已存代理，探测时直接用这个地址，命中不绑定它（改绑静态）。
    ephemeral: Option<OutboundProxy>,
}

struct Hunt {
    service: DefaultAccountsService,
    provider: Arc<dyn ProviderAdmin>,
    command: TurnStateHuntCommand,
    ticket: TurnStateHuntTicket,
    request: AccountProbeRequest,
    events: tokio::sync::mpsc::Sender<TurnStateHuntEvent>,
    requests: u32,
    /// 跨出口累计的连续「上游无容量」次数；任何一次请求拿到上游应答就清零。
    capacity_streak: u8,
    /// 命中收尾时因令牌刷新而重新取票的次数；超过 [`MAX_TICKET_REFRESHES`] 才放弃。
    ticket_refreshes: u8,
}

enum Outcome {
    Continue,
    Stop,
}

/// 命中收尾的结果。
enum FinalizeOutcome {
    /// 已收尾（钉住成功、或因确凿原因失败并已发事件）：停止整轮。
    Committed,
    /// 令牌刚好刷新、已重新取票：丢弃这次命中，继续用新票撞下一个出口。
    Retry,
}

/// 实际发出探测的那个出口。命中后要绑定的必须就是它：同一个代理 ID 在探测之后
/// 可能已被改了地址，账号也可能已被管理员改绑。
struct ProbedEgress {
    proxy_id: Option<String>,
    revision: Option<Revision>,
    proxy: Option<OutboundProxy>,
}

impl DefaultAccountsService {
    pub(super) async fn start_turn_state_hunt(
        &self,
        command: TurnStateHuntCommand,
    ) -> Result<TurnStateHuntEventStream, AdminError> {
        if command.attempts == 0 || command.attempts > TurnStateHuntCommand::MAX_ATTEMPTS {
            return Err(AdminError::invalid(
                "每个代理的尝试次数必须在 1 到 200 之间",
            ));
        }
        let (stored, provider) = self.provider_for_account(&command.account_id).await?;
        let ticket = provider
            .turn_state_hunt_prepare(&command.account_id, &command.upstream_model)
            .await
            .map_err(|error| map_provider_error(error, "provider turn state hunt"))?;
        let operation = provider
            .connection_test_operation(&command.upstream_model, CONNECTION_TEST_INPUT)
            .map_err(|error| map_provider_error(error, "provider turn state hunt"))?;
        let egresses = if let Some(ephemeral) = command.ephemeral.as_ref() {
            // 自动撞：即时生成临时出口，不读已存代理，也不看 only_proxy_id。
            Self::ephemeral_egresses(ephemeral)?
        } else {
            let mut egresses = self
                .hunt_egresses(
                    stored.account.outbound_proxy.as_ref(),
                    command.include_direct,
                )
                .await?;
            if let Some(only) = command.only_proxy_id.as_deref() {
                egresses.retain(|egress| egress.view.proxy_id.as_deref() == Some(only));
                if egresses.is_empty() {
                    return Err(AdminError::invalid(
                        "指定的代理不存在或尚未通过测试，请先在代理页测试通过",
                    ));
                }
            }
            egresses
        };
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
            capacity_streak: 0,
            ticket_refreshes: 0,
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
                        ephemeral: None,
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
                ephemeral: None,
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

    /// 自动撞：解析请求（读模板代理地址、挑测试通过且账号数最少的静态出口），
    /// 落成一条带 `ephemeral`/`bind_to` 的命令，复用遍历的循环、容量退避、取消与命中收尾。
    pub(super) async fn start_auto_turn_state_hunt(
        &self,
        request: crate::model::accounts::TurnStateAutoHuntRequest,
    ) -> Result<crate::model::accounts::TurnStateHuntEventStream, AdminError> {
        use crate::model::accounts::{EphemeralHunt, TurnStateHuntCommand};

        let template = self
            .proxies
            .get(&request.template_proxy_id)
            .await
            .map_err(|_| AdminError::invalid("找不到轮换代理模板，请先在代理页添加并测试通过"))?;
        // 静态池：只保留测试通过的；账号数最少者优先，稳定 tie-break 用 id。命中后改绑它。
        let mut candidates = Vec::new();
        for id in &request.static_proxy_ids {
            if let Ok(record) = self.proxies.get(id).await
                && record.last_test.as_ref().is_some_and(|test| test.success)
            {
                candidates.push(record);
            }
        }
        candidates.sort_by(|a, b| {
            a.account_count
                .cmp(&b.account_count)
                .then_with(|| a.id.cmp(&b.id))
        });
        let bind_to = candidates
            .first()
            .map(|record| record.id.clone())
            .ok_or_else(|| {
                AdminError::invalid("没有可用的静态出口：请确认所选静态代理已测试通过")
            })?;

        let command = TurnStateHuntCommand {
            account_id: request.account_id,
            upstream_model: request.upstream_model,
            // 每个临时 IP 只打 1 次（同地址复用会 strip turn-state）；IP 数由 count 控制。
            attempts: 1,
            include_direct: false,
            only_proxy_id: None,
            ephemeral: Some(EphemeralHunt {
                template_url: template.proxy.expose_url().to_owned(),
                countries: request.countries,
                count: request.max_ips,
            }),
            bind_to: Some(bind_to),
            require_schedulable: false,
            context: request.context,
        };
        self.start_turn_state_hunt(command).await
    }

    /// 自动续期：若代理池里有轮换代理模板（用户名含 `_area-` 的 smartproxy 式地址），
    /// 就走「自动撞」的快速多国临时出口、命中切静态；否则回退到遍历已存代理。
    /// 续期在 System 身份下运行，账号被停用/凭据失效时不发请求。
    pub(super) async fn start_renewal_turn_state_hunt(
        &self,
        sweep_command: TurnStateHuntCommand,
    ) -> Result<TurnStateHuntEventStream, AdminError> {
        use crate::model::accounts::{EphemeralHunt, HuntCountry};
        /// 续期每轮最多试多少个临时 IP；命中即止。远小于手动自动撞的上限，控制额度。
        const RENEWAL_EPHEMERAL_IPS: u16 = 60;

        // 收集测试通过的代理：轮换模板 vs 可作静态目标的固定出口。
        let mut template_url: Option<String> = None;
        let mut statics: Vec<ProxyRecord> = Vec::new();
        let page_size =
            PageSize::new(PageSize::MAX).map_err(|_| AdminError::internal("分页大小不合法"))?;
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
            for record in listed.items {
                if !record.last_test.as_ref().is_some_and(|test| test.success) {
                    continue;
                }
                if record.proxy.expose_url().contains("_area-") {
                    // 第一条轮换代理即模板；其余轮换代理不作静态目标。
                    template_url.get_or_insert_with(|| record.proxy.expose_url().to_owned());
                } else {
                    statics.push(record);
                }
            }
            if fetched < usize::from(PageSize::MAX) {
                break;
            }
            page += 1;
        }

        // 有模板 + 至少一个静态出口才走自动撞；否则回退遍历。
        let (Some(template_url), false) = (template_url, statics.is_empty()) else {
            return self.start_turn_state_hunt(sweep_command).await;
        };
        statics.sort_by(|a, b| {
            a.account_count
                .cmp(&b.account_count)
                .then_with(|| a.id.cmp(&b.id))
        });
        let bind_to = statics[0].id.clone();
        let command = TurnStateHuntCommand {
            attempts: 1,
            include_direct: false,
            only_proxy_id: None,
            ephemeral: Some(EphemeralHunt {
                template_url,
                countries: vec![
                    HuntCountry::Us,
                    HuntCountry::Jp,
                    HuntCountry::De,
                    HuntCountry::Ph,
                ],
                count: RENEWAL_EPHEMERAL_IPS,
            }),
            bind_to: Some(bind_to),
            ..sweep_command
        };
        self.start_turn_state_hunt(command).await
    }

    /// 从模板即时生成 `count` 个「一 IP 一条唯一 session」的临时出口，国家随机取自 `countries`。
    fn ephemeral_egresses(
        ephemeral: &crate::model::accounts::EphemeralHunt,
    ) -> Result<Vec<Egress>, AdminError> {
        use rand_core::{OsRng, RngCore as _};
        if ephemeral.countries.is_empty() {
            return Err(AdminError::invalid("请至少选择一个国家"));
        }
        let count = ephemeral.count.min(TurnStateHuntCommand::MAX_EPHEMERAL_IPS);
        let mut egresses = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let country = ephemeral.countries
                [usize::try_from(OsRng.next_u32()).unwrap_or(0) % ephemeral.countries.len()];
            let session = format!("{:016x}", OsRng.next_u64());
            let url = ephemeral_proxy_url(&ephemeral.template_url, country.area_code(), &session)
                .ok_or_else(|| {
                AdminError::invalid("轮换代理模板地址不合法，无法生成临时出口")
            })?;
            let proxy = OutboundProxy::parse(&url)
                .map_err(|_| AdminError::invalid("生成的临时出口地址不合法，请检查模板"))?;
            egresses.push(Egress {
                bound: false,
                view: TurnStateHuntEgress {
                    proxy_id: None,
                    name: format!("{} · 动态", country.area_code()),
                    endpoint: Some(proxy.endpoint()),
                },
                ephemeral: Some(proxy),
            });
        }
        Ok(egresses)
    }
}

/// 把轮换代理模板地址改造成一次性出口：用户名里的 `_area-`/`_session-`/`_life-` 段全部换成
/// 目标国家与一次性 session。仅支持 smartproxy 式下划线用户名（`base_key-value_key-value`）。
fn ephemeral_proxy_url(template: &str, country: &str, session: &str) -> Option<String> {
    let (scheme, after) = template.split_once("://")?;
    let (authority, path) = after
        .split_once('/')
        .map_or((after, None), |(a, p)| (a, Some(p)));
    // 主机不含 '@'，凭据里的 '@' 也不在 smartproxy 账号中出现：从右侧切开凭据与主机。
    let (creds, host) = authority.rsplit_once('@')?;
    let (user, pass) = creds
        .split_once(':')
        .map_or((creds, None), |(u, p)| (u, Some(p)));
    let area = format!("area-{country}");
    let sess = format!("session-{session}");
    let mut segments: Vec<&str> = user
        .split('_')
        .filter(|segment| {
            !(segment.starts_with("area-")
                || segment.starts_with("session-")
                || segment.starts_with("life-"))
        })
        .collect();
    segments.push(&area);
    segments.push(&sess);
    let new_user = segments.join("_");
    let creds = pass.map_or_else(|| new_user.clone(), |pass| format!("{new_user}:{pass}"));
    Some(match path {
        Some(path) => format!("{scheme}://{creds}@{host}/{path}"),
        None => format!("{scheme}://{creds}@{host}/"),
    })
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

    /// 续期是系统替管理员发请求：账号已被停用或凭据已失效时必须立刻停手。
    /// 手动遍历不受此限（管理员可以对停用账号做诊断）。
    async fn still_schedulable(&self) -> bool {
        if !self.command.require_schedulable {
            return true;
        }
        self.service
            .provider_for_account(&self.command.account_id)
            .await
            .is_ok_and(|(item, _)| {
                item.account.enabled && item.account.credential_state == CredentialState::Ready
            })
    }

    async fn try_egress(&mut self, egress: Egress, index: usize, total: usize) -> Outcome {
        let proxy_id = egress.view.proxy_id.clone();
        // 自动撞的临时出口：地址就地生成、不在库里，直接用它探测，不做「重新读取代理」的核对。
        if let Some(proxy) = egress.ephemeral.clone() {
            return self
                .probe_egress(
                    ProbedEgress {
                        proxy_id: None,
                        revision: None,
                        proxy: Some(proxy),
                    },
                    None,
                    egress.view.clone(),
                    index,
                    total,
                )
                .await;
        }
        // 列表之后代理可能被删除、改地址或测试失效，逐个重新读取。
        let (probed, location) = match &proxy_id {
            None => (
                ProbedEgress {
                    proxy_id: None,
                    revision: None,
                    proxy: None,
                },
                None,
            ),
            Some(id) => match self.service.proxies.get(id).await {
                Ok(record) if record.last_test.as_ref().is_some_and(|test| test.success) => (
                    ProbedEgress {
                        proxy_id: Some(record.id),
                        revision: Some(record.revision),
                        proxy: Some(record.proxy),
                    },
                    record.location,
                ),
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
        self.probe_egress(probed, location, egress.view.clone(), index, total)
            .await
    }

    /// 对一个已解析出的出口发探测：发 `attempts` 次真实请求，命中即收尾。已存出口与临时出口共用。
    async fn probe_egress(
        &mut self,
        probed: ProbedEgress,
        location: Option<gateway_core::account::RequestLocation>,
        view: TurnStateHuntEgress,
        index: usize,
        total: usize,
    ) -> Outcome {
        let proxy_id = probed.proxy_id.clone();
        self.emit(TurnStateHuntEvent::EgressStarted {
            egress: view.clone(),
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
            // 每个请求前都核对：账号在遍历途中被停用后，连同一出口上剩下的尝试也不再发。
            if !self.still_schedulable().await {
                self.fail(
                    "account_unschedulable",
                    "账号已停用或凭据已失效，续期已停止",
                )
                .await;
                return Outcome::Stop;
            }
            let captured_at = SystemTime::now();
            let result = self
                .service
                .probe
                .probe(AccountProbeRequest {
                    egress: Some(DiagnosticEgress::new(
                        probed.proxy.clone(),
                        location.clone(),
                    )),
                    ..self.request.clone()
                })
                .await;
            let error = match result {
                Ok(result) => {
                    self.requests += 1;
                    used += 1;
                    (busy, failures) = (0, 0);
                    self.capacity_streak = 0;
                    // 这个出口后来拿到了应答：此前的「无容量」不再是它落空的原因。
                    skipped = None;
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
                        // 命中事件先发（保持「Completed 收尾在最后」的次序），再进收尾。
                        self.log_egress(&view, &lengths, true);
                        self.emit(TurnStateHuntEvent::EgressFinished {
                            proxy_id: proxy_id.clone(),
                            attempts: used,
                            matched: true,
                            skipped: None,
                        })
                        .await;
                        match self
                            .finalize(&probed, used, &result.response_headers, captured_at)
                            .await
                        {
                            FinalizeOutcome::Committed => return Outcome::Stop,
                            // 令牌刚好刷新、已重新取票：这次命中不作数，继续撞下一个出口。
                            FinalizeOutcome::Retry => return Outcome::Continue,
                        }
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
            // 只有真的发往上游的才算一次请求；在本地就失败的尝试不计。
            if !matches!(error.send_state(), None | Some(UpstreamSendState::NotSent)) {
                self.requests += 1;
            }
            used += 1;
            lengths.push(None);
            let class = FailureClass::of(&error);
            self.emit(TurnStateHuntEvent::Attempt {
                proxy_id: proxy_id.clone(),
                index: used,
                length: None,
                matched: false,
                error: Some(TurnStateHuntAttemptError {
                    code: error.kind(),
                    source: error.source(),
                    upstream_status: error
                        .upstream_response()
                        .map(gateway_core::engine::probe::AccountProbeUpstreamResponse::status),
                    // 上游原文可能回显请求材料，事件里只放按分类给出的固定文案。
                    message: class.message().to_owned(),
                }),
            })
            .await;
            if class == FailureClass::Capacity {
                self.capacity_streak += 1;
                if self.capacity_streak >= MAX_CAPACITY_STREAK {
                    self.log_egress(&view, &lengths, false);
                    // 连续多次都无容量：不是抖动，再打剩下的出口只会白耗额度。
                    self.fail("upstream_capacity", "上游该模型暂无容量，稍后再试")
                        .await;
                    return Outcome::Stop;
                }
                // 无容量与出口无关，不计入出口失败；歇一下再继续，仍受「每个代理最多尝试」约束。
                skipped = Some("capacity");
                tokio::time::sleep(CAPACITY_RETRY_DELAY).await;
                continue;
            }
            skipped = None;
            if class == FailureClass::ModelUnavailable {
                // 与出口好坏无关（动态出口每次换 IP），只占用本次尝试次数。
                continue;
            }
            if let Some((code, message)) = class.abort() {
                self.log_egress(&view, &lengths, false);
                // 这些失败换出口也不会好，继续只会白耗额度。
                self.fail(code, message).await;
                return Outcome::Stop;
            }
            failures += 1;
            if failures >= MAX_EGRESS_FAILURES {
                skipped = Some("unreachable");
                break;
            }
        }
        self.log_egress(&view, &lengths, false);
        self.emit(TurnStateHuntEvent::EgressFinished {
            proxy_id,
            attempts: used,
            matched: false,
            skipped,
        })
        .await;
        Outcome::Continue
    }

    /// 账号当前保存的出口是否就是刚探测的这个。
    async fn bound_to(&self, probed: &ProbedEgress) -> Result<bool, AdminError> {
        let (item, _) = self
            .service
            .provider_for_account(&self.command.account_id)
            .await?;
        Ok(item.account.outbound_proxy == probed.proxy)
    }

    /// 先绑后钉：state 是在这个出口上观测到的，只有账号确实走这个出口之后才值得钉。
    async fn finalize(
        &mut self,
        probed: &ProbedEgress,
        attempt_index: u8,
        response_headers: &[ProviderResponseHeader],
        captured_at: SystemTime,
    ) -> FinalizeOutcome {
        let account_id = self.command.account_id.clone();
        let proxy_id = probed.proxy_id.clone();
        if !self.still_schedulable().await {
            self.fail(
                "account_unschedulable",
                "账号已停用或凭据已失效，未改动账号",
            )
            .await;
            return FinalizeOutcome::Committed;
        }
        // 命中收尾前先核对凭据：这次命中的 state 是在收尾这一刻的凭据下观测的才可信。
        // 令牌刚好在命中前后刷新（繁忙 Business 号很常见）时，不要整轮中止——重新取票、
        // 丢弃这次命中、继续用新票撞下一个出口，最多 [`MAX_TICKET_REFRESHES`] 次。
        match self
            .provider
            .turn_state_hunt_prepare(&account_id, &self.command.upstream_model)
            .await
        {
            Ok(current) if current == self.ticket => {}
            Ok(current) => {
                self.ticket_refreshes += 1;
                if self.ticket_refreshes > MAX_TICKET_REFRESHES {
                    self.fail(
                        "credential_changed",
                        "凭据在遍历期间反复变化（令牌频繁刷新），请稍后重试",
                    )
                    .await;
                    return FinalizeOutcome::Committed;
                }
                self.ticket = current;
                tracing::info!(
                    target: "turn_state_hunt",
                    account_id = account_id.as_str(),
                    refreshes = self.ticket_refreshes,
                    "命中时令牌刚好刷新，已重新取票，丢弃本次命中继续撞"
                );
                return FinalizeOutcome::Retry;
            }
            Err(_) => {
                self.fail("credential_changed", "凭据在遍历期间已变化，请重新遍历")
                    .await;
                return FinalizeOutcome::Committed;
            }
        }
        // 提交边界：命中事件送达之后才进入不可取消的收尾。页面在探测途中已经断开
        // （用户点了取消）时事件送不出去，此时什么都不改，兑现「取消不改动账号」。
        let hit = TurnStateHuntEvent::Hit {
            proxy_id: proxy_id.clone(),
            attempt_index,
            length: self.ticket.expected_length(),
        };
        if self.events.send(hit).await.is_err() {
            tracing::info!(
                target: "turn_state_hunt",
                account_id = account_id.as_str(),
                "遍历命中时页面已取消，未改动账号"
            );
            return FinalizeOutcome::Committed;
        }
        // 目标出口 = 要绑定并按其指纹钉 state 的那个。自动撞在轮换 IP 上命中，却要落到稳定的
        // 静态家宽（`bind_to`，state 已确认可移植）；普通遍历则就是命中的那个出口。
        let target = match self.command.bind_to.clone() {
            Some(static_id) => match self.service.proxies.get(&static_id).await {
                Ok(record) if record.last_test.as_ref().is_some_and(|test| test.success) => {
                    ProbedEgress {
                        proxy_id: Some(record.id),
                        revision: Some(record.revision),
                        proxy: Some(record.proxy),
                    }
                }
                _ => {
                    self.fail(
                        "static_unavailable",
                        "要改绑的静态出口不存在或未测试通过，未改动账号",
                    )
                    .await;
                    return FinalizeOutcome::Committed;
                }
            },
            None => ProbedEgress {
                proxy_id: probed.proxy_id.clone(),
                revision: probed.revision,
                proxy: probed.proxy.clone(),
            },
        };
        let target_proxy_id = target.proxy_id.clone();
        // 目标是已存代理（普通遍历命中的、或自动撞要切的静态）时，核对它在收尾这一刻仍是原样、测试通过。
        if let Some(id) = &target_proxy_id {
            let unchanged = self.service.proxies.get(id).await.is_ok_and(|record| {
                Some(record.revision) == target.revision
                    && Some(&record.proxy) == target.proxy.as_ref()
                    && record.last_test.as_ref().is_some_and(|test| test.success)
            });
            if !unchanged {
                self.fail("egress_changed", "该代理在遍历期间被修改，请重新遍历")
                    .await;
                return FinalizeOutcome::Committed;
            }
        }
        let Ok(already_bound) = self.bound_to(&target).await else {
            self.fail("bind_failed", "读取账号当前绑定失败，未改动账号")
                .await;
            return FinalizeOutcome::Committed;
        };
        if !already_bound {
            let selection = target_proxy_id
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
            if let Err(error) = &bound {
                tracing::warn!(
                    target: "turn_state_hunt",
                    account_id = account_id.as_str(),
                    error_kind = ?error.kind(),
                    "遍历命中但绑定代理返回失败"
                );
            }
            // 绑定调用的返回值不足为凭（提交与发布分两步，也可能被并发改绑），
            // 以回读到的账号绑定为准：不是目标出口就不钉。
            if !self.bound_to(&target).await.unwrap_or(false) {
                self.fail(
                    "bind_mismatch",
                    "账号当前绑定的不是目标出口，未钉住 state，请刷新后重试",
                )
                .await;
                return FinalizeOutcome::Committed;
            }
        }
        tracing::info!(
            target: "turn_state_hunt",
            account_id = account_id.as_str(),
            proxy_id = target_proxy_id.as_deref().unwrap_or("direct"),
            changed = !already_bound,
            "遍历命中，账号已绑定到目标出口"
        );
        self.emit(TurnStateHuntEvent::Bound {
            proxy_id: target_proxy_id.clone(),
            changed: !already_bound,
        })
        .await;
        match self
            .provider
            .turn_state_hunt_pin(
                &self.ticket,
                response_headers,
                captured_at,
                target.proxy.as_ref(),
            )
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
        FinalizeOutcome::Committed
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
    // Provider 已经明确说「选不出可用账号」时不是忙，是账号不可用：重试五次也不会变好。
    if error.provider_kind() == Some(ProviderErrorKind::NoEligibleAccount) {
        return false;
    }
    matches!(
        error.kind(),
        GatewayErrorKind::AccountCapacityUnavailable
            | GatewayErrorKind::NoAvailableProvider
            | GatewayErrorKind::ConcurrencyQueueFull
            | GatewayErrorKind::ConcurrencyQueueTimeout
    ) && matches!(error.send_state(), None | Some(UpstreamSendState::NotSent))
}

/// 一次失败说明的是账号还是出口。
///
/// 必须看 Provider 的原始分类：面向客户端的 [`GatewayErrorKind`] 把凭据失效、无权限
/// 都折叠成「上游不可用」，按它分类会把已经没救的账号拿去把所有代理打一遍。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureClass {
    /// 换出口也不会好：凭据失效、被封、额度耗尽、限流、模型不支持、请求本身不合法。
    Account,
    /// 上游明确拒绝的是这个模型的容量，与出口无关。它多为瞬时过载，所以不立刻中止：
    /// 歇一下继续遍历，连续 [`MAX_CAPACITY_STREAK`] 次才认定确实没有容量。
    Capacity,
    /// 本机的账号存储、租约协调或凭据数据不可用，或请求被取消：与出口无关，整轮中止。
    System,
    /// 出口不通、被 Cloudflare 拦截、超时、返回了不合法的协议等：换一个出口再试。
    Egress,
    /// 上游这次答复该模型不存在（404）：不中止、不算出口不通，消耗本次尝试后继续。
    ModelUnavailable,
}

impl FailureClass {
    fn of(error: &AccountProbeError) -> Self {
        // 上游 404（如 model_not_found）随出口/后端分片时有时无：同一账号换个出口或下一次
        // 就可能成功。流式失败时没有 HTTP 响应，只剩 Provider 转述的错误码。
        if error
            .upstream_response()
            .is_some_and(|response| response.status() == 404)
            || error.client_error_code() == Some("model_not_found")
        {
            return Self::ModelUnavailable;
        }
        match error.provider_kind() {
            Some(
                ProviderErrorKind::Unauthorized
                | ProviderErrorKind::PermissionDenied
                | ProviderErrorKind::QuotaExhausted
                | ProviderErrorKind::RateLimited
                | ProviderErrorKind::Unsupported
                | ProviderErrorKind::InvalidRequest,
            ) => Self::Account,
            Some(ProviderErrorKind::UpstreamCapacityUnavailable) => Self::Capacity,
            Some(
                ProviderErrorKind::ProviderInfrastructureUnavailable
                | ProviderErrorKind::NoEligibleAccount
                | ProviderErrorKind::ContinuationRecoveryRequired
                | ProviderErrorKind::Cancelled
                | ProviderErrorKind::ProcessTerminated,
            ) => Self::System,
            // 只剩与链路有关的类别：Transport / Timeout / Unavailable / Protocol，
            // 以及本应在发出前就被当作「账号忙」处理掉的排队类。
            Some(_) => Self::Egress,
            // 没有 Provider 分类时只能看网关层；限流与不支持在那一层不会被折叠。
            None => match error.kind() {
                GatewayErrorKind::RateLimited
                | GatewayErrorKind::Unsupported
                | GatewayErrorKind::InvalidRequest
                | GatewayErrorKind::ModelNotFound => Self::Account,
                // 没有 Provider 分类的内部错误来自网关自身（如运行时快照不可用），与出口无关。
                GatewayErrorKind::Internal | GatewayErrorKind::Cancelled => Self::System,
                _ => Self::Egress,
            },
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::Account => "上游拒绝了该账号的请求",
            Self::Capacity => "上游该模型暂无容量",
            Self::System => "网关本地错误，请求未能完成",
            Self::Egress => "经该出口的请求失败",
            Self::ModelUnavailable => "上游这次答复该模型不可用（404），继续下一次",
        }
    }

    /// 需要中止整轮遍历时返回事件码与说明；出口类失败返回 `None`，换下一个出口。
    const fn abort(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Account => Some(("account_rejected", "上游拒绝了该账号的请求，遍历已中止")),
            // 是否中止由连续次数决定，见 `try_egress`。
            Self::Capacity => None,
            Self::System => Some(("system_error", "网关本地错误，遍历已中止")),
            Self::Egress | Self::ModelUnavailable => None,
        }
    }
}

#[cfg(test)]
mod failure_class_tests {
    use gateway_core::{
        engine::probe::{AccountProbeError, AccountProbeErrorSource},
        error::{GatewayError, GatewayErrorKind, ProviderErrorKind},
    };

    use super::FailureClass;

    fn provider_error(kind: ProviderErrorKind, code: Option<&'static str>) -> AccountProbeError {
        let mut gateway = GatewayError::new(GatewayErrorKind::InvalidRequest, "upstream rejected");
        if let Some(code) = code {
            gateway = gateway.with_client_code(code);
        }
        AccountProbeError::new(gateway, AccountProbeErrorSource::Upstream, None, None)
            .with_provider_kind(Some(kind))
    }

    #[test]
    fn model_not_found_moves_on_instead_of_aborting_the_hunt() {
        let class = FailureClass::of(&provider_error(
            ProviderErrorKind::InvalidRequest,
            Some("model_not_found"),
        ));
        assert_eq!(class, FailureClass::ModelUnavailable);
        assert!(class.abort().is_none());
    }

    #[test]
    fn other_invalid_requests_still_abort_the_hunt() {
        let class = FailureClass::of(&provider_error(ProviderErrorKind::InvalidRequest, None));
        assert_eq!(class, FailureClass::Account);
        assert!(class.abort().is_some());
    }
}

#[cfg(test)]
mod ephemeral_url_tests {
    use super::ephemeral_proxy_url;

    #[test]
    fn injects_country_and_session_replacing_existing_tokens() {
        let url = ephemeral_proxy_url(
            "http://user_area-US_life-30_session-old:pass@host.example:3120/",
            "JP",
            "abc123",
        )
        .expect("valid template");
        // 旧的 area/session/life 段被剥掉，换成目标国家与一次性 session；base 段与其余部分保留。
        assert_eq!(
            url,
            "http://user_area-JP_session-abc123:pass@host.example:3120/"
        );
    }

    #[test]
    fn appends_tokens_when_username_has_none() {
        let url = ephemeral_proxy_url("http://base:pass@host.example:3120/", "DE", "s1")
            .expect("valid template");
        assert_eq!(
            url,
            "http://base_area-DE_session-s1:pass@host.example:3120/"
        );
    }

    #[test]
    fn rejects_a_template_without_credentials() {
        assert!(ephemeral_proxy_url("http://host.example:3120/", "US", "s1").is_none());
    }
}
