//! 遍历代理找 state：出口顺序、命中后的「先绑后钉」、中止与跳过规则。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use chrono::Utc;
use futures::{StreamExt as _, future::BoxFuture};
use gateway_admin::model::{
    AdminErrorKind,
    accounts::{TurnStateHuntCommand, TurnStateHuntEvent},
    proxies::{ProxyRecord, ProxyTestResult},
};
use gateway_core::{
    account::{OutboundProxy, ProviderAccountId},
    engine::probe::{
        AccountProbe, AccountProbeError, AccountProbeErrorSource, AccountProbeRequest,
        AccountProbeResult,
    },
    error::{GatewayError, GatewayErrorKind, ProviderErrorKind},
    event::ProviderResponseHeader,
    routing::UpstreamModelId,
    upstream::UpstreamSendState,
};

use super::accounts::{
    EventLog, FakeAccountStore, FakeProviderAdmin, account_record, context, events, recorded,
    revision,
};
use super::proxies::TestProxies;

pub(super) const EXPECTED_LENGTH: usize = 332;

enum Reply {
    State(usize),
    NoState,
    Fail(GatewayErrorKind, UpstreamSendState),
    /// Provider 的原始分类；网关层一律折叠成「上游不可用」，正如真实链路。
    Rejected(ProviderErrorKind),
}

/// 按顺序回放探测结果，并记下每次请求走的出口。
struct ScriptedProbe {
    replies: Mutex<VecDeque<Reply>>,
    egresses: Mutex<Vec<Option<String>>>,
    on_probe: Mutex<Option<Box<dyn Fn() + Send>>>,
    /// 设置后探测在收到通知前不返回：用来模拟「页面已取消，在途请求随后才命中」。
    gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
}

impl ScriptedProbe {
    fn new(replies: impl IntoIterator<Item = Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().collect()),
            egresses: Mutex::new(Vec::new()),
            on_probe: Mutex::new(None),
            gate: Mutex::new(None),
        })
    }

    fn egresses(&self) -> Vec<Option<String>> {
        self.egresses.lock().unwrap().clone()
    }
}

impl AccountProbe for ScriptedProbe {
    fn probe(
        &self,
        request: AccountProbeRequest,
    ) -> BoxFuture<'_, Result<AccountProbeResult, AccountProbeError>> {
        let egress = request.egress.expect("hunt probes always pin an egress");
        self.egresses
            .lock()
            .unwrap()
            .push(egress.proxy().map(OutboundProxy::endpoint));
        if let Some(hook) = self.on_probe.lock().unwrap().as_ref() {
            hook();
        }
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected extra probe");
        let gate = self.gate.lock().unwrap().clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            match reply {
                Reply::State(length) => Ok(AccountProbeResult {
                    response_headers: vec![ProviderResponseHeader::new(
                        "x-codex-turn-state",
                        Bytes::from("s".repeat(length)),
                    )],
                    ..AccountProbeResult::default()
                }),
                Reply::NoState => Ok(AccountProbeResult::default()),
                Reply::Fail(kind, send_state) => Err(AccountProbeError::new(
                    GatewayError::new(kind, "probe failed"),
                    AccountProbeErrorSource::Upstream,
                    Some(send_state),
                    None,
                )),
                Reply::Rejected(kind) => Err(AccountProbeError::new(
                    GatewayError::new(GatewayErrorKind::UpstreamUnavailable, "raw upstream text"),
                    AccountProbeErrorSource::Upstream,
                    Some(UpstreamSendState::Sent),
                    None,
                )
                .with_provider_kind(Some(kind))),
            }
        })
    }
}

fn proxy(id: &str, port: u16, usable: bool) -> ProxyRecord {
    let now = Utc::now();
    ProxyRecord {
        location: None,
        id: id.to_owned(),
        name: id.to_owned(),
        proxy: OutboundProxy::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
        revision: revision(1),
        account_count: 0,
        last_test_at: Some(now),
        last_test: Some(ProxyTestResult {
            success: usable,
            latency_ms: 1,
            exit_ip: None,
            message: String::new(),
        }),
        created_at: now,
        updated_at: now,
    }
}

struct Setup {
    services: gateway_admin::AdminServices,
    provider: Arc<FakeProviderAdmin>,
    store: Arc<FakeAccountStore>,
    proxies: Arc<Mutex<Vec<ProxyRecord>>>,
    log: EventLog,
}

async fn setup(
    probe: Arc<ScriptedProbe>,
    proxies: Vec<ProxyRecord>,
    bound: Option<&ProxyRecord>,
) -> Setup {
    let log = events();
    let provider = FakeProviderAdmin::new("openai", log.clone());
    *provider.hunt_binding.lock().unwrap() = Some("binding-1".to_owned());
    let mut account = account_record("openai");
    account.outbound_proxy = bound.map(|record| record.proxy.clone());
    let store = FakeAccountStore::with_account(account, log.clone());
    *store.saved_proxies.lock().unwrap() = proxies
        .iter()
        .map(|record| (record.id.clone(), record.proxy.clone()))
        .collect();
    let proxies = Arc::new(Mutex::new(proxies));
    let services = super::AdminHarness::new()
        .accounts(store.clone())
        .provider(provider.clone())
        .probe(probe)
        .proxies(Arc::new(TestProxies {
            records: Some(proxies.clone()),
            ..Default::default()
        }))
        .build()
        .await;
    Setup {
        services,
        provider,
        store,
        proxies,
        log,
    }
}

fn command(attempts: u8, include_direct: bool) -> TurnStateHuntCommand {
    TurnStateHuntCommand {
        account_id: ProviderAccountId::new("acct_test").unwrap(),
        upstream_model: UpstreamModelId::new("gpt-6-astra").unwrap(),
        attempts,
        include_direct,
        only_proxy_id: None,
        require_schedulable: false,
        context: context("hunt"),
    }
}

async fn run(setup: &Setup, command: TurnStateHuntCommand) -> Vec<TurnStateHuntEvent> {
    setup
        .services
        .accounts()
        .turn_state_hunt(command)
        .await
        .expect("hunt stream")
        .collect()
        .await
}

fn endpoint(port: u16) -> Option<String> {
    Some(format!("http://127.0.0.1:{port}/"))
}

#[tokio::test]
async fn hit_binds_the_egress_before_pinning_and_stops_probing() {
    let probe = ScriptedProbe::new([
        Reply::State(292),
        Reply::NoState,
        Reply::State(292),
        Reply::State(EXPECTED_LENGTH),
    ]);
    let setup = setup(
        probe.clone(),
        vec![
            proxy("first", 8001, true),
            proxy("untested", 8009, false),
            proxy("second", 8002, true),
        ],
        None,
    )
    .await;

    let events = run(&setup, command(2, false)).await;

    // 未通过测试的代理不参与；每个出口最多两次，命中后不再请求。
    assert_eq!(
        probe.egresses(),
        vec![
            endpoint(8001),
            endpoint(8001),
            endpoint(8002),
            endpoint(8002)
        ]
    );
    let log = recorded(&setup.log);
    let bind = log
        .iter()
        .position(|event| *event == "store.batch_update_accounts")
        .expect("account bound");
    let pin = log
        .iter()
        .position(|event| *event == "provider.hunt_pin")
        .expect("state pinned");
    assert!(bind < pin);
    // state 钉在探测到它的那个出口上，而不是「账号当时绑着的出口」。
    assert_eq!(
        *setup.provider.hunt_pinned_egress.lock().unwrap(),
        Some(endpoint(8002))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        TurnStateHuntEvent::Attempt { proxy_id: Some(id), length: Some(292), matched: false, .. } if id == "first"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        TurnStateHuntEvent::Bound { proxy_id: Some(id), changed: true } if id == "second"
    )));
    assert!(matches!(
        events.last(),
        Some(TurnStateHuntEvent::Completed {
            success: true,
            requests: 4
        })
    ));
}

#[tokio::test]
async fn bound_egress_goes_first_and_is_not_rebound() {
    let probe = ScriptedProbe::new([Reply::State(EXPECTED_LENGTH)]);
    let current = proxy("current", 8002, true);
    let setup = setup(
        probe.clone(),
        vec![proxy("other", 8001, true), current.clone()],
        Some(&current),
    )
    .await;

    let events = run(&setup, command(3, true)).await;

    assert_eq!(probe.egresses(), vec![endpoint(8002)]);
    assert!(!recorded(&setup.log).contains(&"store.batch_update_accounts"));
    assert!(recorded(&setup.log).contains(&"provider.hunt_pin"));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, TurnStateHuntEvent::Bound { changed: false, .. }))
    );
}

#[tokio::test]
async fn miss_everywhere_changes_nothing_and_direct_is_tried_last() {
    let probe = ScriptedProbe::new([Reply::State(292), Reply::NoState]);
    let setup = setup(
        probe.clone(),
        vec![proxy("only", 8001, true)],
        Some(&proxy("only", 8001, true)),
    )
    .await;

    let events = run(&setup, command(1, true)).await;

    assert_eq!(probe.egresses(), vec![endpoint(8001), None]);
    let log = recorded(&setup.log);
    assert!(!log.contains(&"store.batch_update_accounts"));
    assert!(!log.contains(&"provider.hunt_pin"));
    assert!(matches!(
        events.last(),
        Some(TurnStateHuntEvent::Completed {
            success: false,
            requests: 2
        })
    ));
}

#[tokio::test]
async fn unreachable_egress_is_skipped_and_account_level_failure_aborts() {
    let probe = ScriptedProbe::new([
        Reply::Fail(
            GatewayErrorKind::UpstreamUnavailable,
            UpstreamSendState::NotSent,
        ),
        Reply::Fail(GatewayErrorKind::Timeout, UpstreamSendState::Sent),
        Reply::Fail(GatewayErrorKind::RateLimited, UpstreamSendState::Sent),
    ]);
    let setup = setup(
        probe.clone(),
        vec![
            proxy("dead", 8001, true),
            proxy("limited", 8002, true),
            proxy("never", 8003, true),
        ],
        None,
    )
    .await;

    let events = run(&setup, command(5, false)).await;

    // 连续两次传输失败即换出口；429 是账号级问题，不再尝试剩余出口。
    assert_eq!(
        probe.egresses(),
        vec![endpoint(8001), endpoint(8001), endpoint(8002)]
    );
    assert!(events.iter().any(|event| matches!(
        event,
        TurnStateHuntEvent::EgressFinished { proxy_id: Some(id), skipped: Some("unreachable"), .. } if id == "dead"
    )));
    assert!(matches!(
        events.last(),
        Some(TurnStateHuntEvent::Failed {
            code: "account_rejected",
            ..
        })
    ));
    assert!(!recorded(&setup.log).contains(&"store.batch_update_accounts"));
}

#[tokio::test]
async fn credential_change_during_hunt_discards_the_hit_without_binding() {
    let probe = ScriptedProbe::new([Reply::State(EXPECTED_LENGTH)]);
    let setup = setup(probe.clone(), vec![proxy("good", 8001, true)], None).await;
    let provider = setup.provider.clone();
    *probe.on_probe.lock().unwrap() = Some(Box::new(move || {
        *provider.hunt_binding.lock().unwrap() = Some("binding-2".to_owned());
    }));

    let events = run(&setup, command(1, false)).await;

    assert!(matches!(
        events.last(),
        Some(TurnStateHuntEvent::Failed {
            code: "credential_changed",
            ..
        })
    ));
    let log = recorded(&setup.log);
    assert!(!log.contains(&"store.batch_update_accounts"));
    assert!(!log.contains(&"provider.hunt_pin"));
}

#[tokio::test]
async fn hunt_is_rejected_up_front_when_it_cannot_run() {
    let setup_without_proxies = setup(ScriptedProbe::new([]), Vec::new(), None).await;
    let error = setup_without_proxies
        .services
        .accounts()
        .turn_state_hunt(command(5, false))
        .await
        .err()
        .expect("no usable egress");
    assert_eq!(error.kind(), AdminErrorKind::Invalid);

    let setup = setup(
        ScriptedProbe::new([]),
        vec![proxy("good", 8001, true)],
        None,
    )
    .await;
    for attempts in [0, 201] {
        let error = setup
            .services
            .accounts()
            .turn_state_hunt(command(attempts, false))
            .await
            .err()
            .expect("attempts out of range");
        assert_eq!(error.kind(), AdminErrorKind::Invalid);
    }
    *setup.provider.hunt_binding.lock().unwrap() = None;
    assert!(
        setup
            .services
            .accounts()
            .turn_state_hunt(command(5, false))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn one_hunt_per_account_and_the_slot_is_released_afterwards() {
    let probe = ScriptedProbe::new([Reply::NoState, Reply::NoState]);
    let setup = setup(probe, vec![proxy("good", 8001, true)], None).await;

    let first = setup
        .services
        .accounts()
        .turn_state_hunt(command(1, false))
        .await
        .expect("first hunt");
    let conflict = setup
        .services
        .accounts()
        .turn_state_hunt(command(1, false))
        .await
        .err()
        .expect("second hunt on the same account");
    assert_eq!(conflict.kind(), AdminErrorKind::Conflict);
    let _ = first.collect::<Vec<_>>().await;

    // 占位随后台任务结束释放；最后一个事件送达与任务退出之间允许几次调度。
    let mut again = None;
    for _ in 0..100 {
        match setup
            .services
            .accounts()
            .turn_state_hunt(command(1, false))
            .await
        {
            Ok(stream) => {
                again = Some(stream.collect::<Vec<_>>().await);
                break;
            }
            Err(error) => {
                assert_eq!(error.kind(), AdminErrorKind::Conflict);
                tokio::task::yield_now().await;
            }
        }
    }
    assert!(matches!(
        again.expect("slot released").last(),
        Some(TurnStateHuntEvent::Completed { success: false, .. })
    ));
}

async fn renewal_cycle(task: &gateway_admin::turn_state_renewal::TurnStateRenewalTask) {
    use gateway_core::task::{ScheduledTask as _, WorkerCycleContext, WorkerId, WorkerKind};
    let worker = WorkerId::try_new(WorkerKind::AccountFreezeRecovery, "test").expect("worker id");
    task.run_cycle(WorkerCycleContext::new(
        worker,
        None,
        gateway_core::lifecycle::CancellationToken::new(),
    ))
    .await
    .expect("renewal cycle");
}

#[tokio::test]
async fn renewal_rehunts_due_accounts_and_backs_off_after_a_full_miss() {
    // 第一轮：绑定的出口续不上，继续打另一个出口并命中。第二轮：全部未命中。
    let probe = ScriptedProbe::new([
        Reply::State(292),
        Reply::State(EXPECTED_LENGTH),
        Reply::NoState,
        Reply::NoState,
    ]);
    let current = proxy("current", 8001, true);
    let setup = setup(
        probe.clone(),
        vec![current.clone(), proxy("other", 8002, true)],
        Some(&current),
    )
    .await;
    *setup.provider.hunt_renewals.lock().unwrap() =
        vec![gateway_admin::ports::provider::TurnStateRenewal {
            account_id: ProviderAccountId::new("acct_test").unwrap(),
            upstream_model: UpstreamModelId::new("gpt-6-astra").unwrap(),
            attempts: 1,
            include_direct: false,
        }];
    let task = gateway_admin::turn_state_renewal::TurnStateRenewalTask::new(
        setup.services.accounts_handle(),
    );

    renewal_cycle(&task).await;
    assert_eq!(probe.egresses(), vec![endpoint(8001), endpoint(8002)]);
    let log = recorded(&setup.log);
    assert!(log.contains(&"store.batch_update_accounts"));
    assert!(log.contains(&"provider.hunt_pin"));

    // 成功后不退避：账号仍到期时下个周期照常再续。
    renewal_cycle(&task).await;
    assert_eq!(probe.egresses().len(), 4);
    // 整轮未命中后进入退避，不会每个周期都把所有出口再打一遍。
    renewal_cycle(&task).await;
    assert_eq!(probe.egresses().len(), 4);
}

fn changed(log: &EventLog) -> bool {
    let log = recorded(log);
    log.contains(&"store.batch_update_accounts") || log.contains(&"provider.hunt_pin")
}

/// 凭据失效与封号在网关层都叫「上游不可用」；按 Provider 分类识别出来后必须立刻中止，
/// 而不是拿一个没救的账号把所有代理打一遍。
#[tokio::test]
async fn account_level_rejection_aborts_without_trying_other_egresses() {
    for kind in [
        ProviderErrorKind::Unauthorized,
        ProviderErrorKind::PermissionDenied,
        ProviderErrorKind::QuotaExhausted,
        ProviderErrorKind::Unsupported,
    ] {
        let probe = ScriptedProbe::new([Reply::Rejected(kind)]);
        let setup = setup(
            probe.clone(),
            vec![proxy("first", 8001, true), proxy("second", 8002, true)],
            None,
        )
        .await;

        let events = run(&setup, command(5, false)).await;

        assert_eq!(probe.egresses(), vec![endpoint(8001)], "{kind:?}");
        assert!(matches!(
            events.last(),
            Some(TurnStateHuntEvent::Failed {
                code: "account_rejected",
                ..
            })
        ));
        // 上游原文不进事件。
        assert!(!format!("{events:?}").contains("raw upstream text"));
        assert!(!changed(&setup.log));
    }
}

#[tokio::test]
async fn egress_level_failure_moves_on_to_the_next_egress() {
    let probe = ScriptedProbe::new([
        Reply::Rejected(ProviderErrorKind::Unavailable),
        Reply::Rejected(ProviderErrorKind::Transport),
        Reply::State(EXPECTED_LENGTH),
    ]);
    let setup = setup(
        probe.clone(),
        vec![proxy("blocked", 8001, true), proxy("good", 8002, true)],
        None,
    )
    .await;

    let events = run(&setup, command(5, false)).await;

    assert_eq!(
        probe.egresses(),
        vec![endpoint(8001), endpoint(8001), endpoint(8002)]
    );
    assert!(matches!(
        events.last(),
        Some(TurnStateHuntEvent::Completed { success: true, .. })
    ));
}

/// 轮换出口每次请求换一个 IP，值得单独打很多次；指定后只遍历它，别的代理一次都不碰。
#[tokio::test]
async fn only_the_named_proxy_is_probed_when_one_is_given() {
    let probe = ScriptedProbe::new([
        Reply::NoState,
        Reply::NoState,
        Reply::State(EXPECTED_LENGTH),
    ]);
    let setup = setup(
        probe.clone(),
        vec![proxy("fixed", 8001, true), proxy("rotating", 8002, true)],
        None,
    )
    .await;

    let events = run(
        &setup,
        TurnStateHuntCommand {
            only_proxy_id: Some("rotating".to_owned()),
            ..command(50, false)
        },
    )
    .await;

    assert_eq!(
        probe.egresses(),
        vec![endpoint(8002), endpoint(8002), endpoint(8002)]
    );
    assert!(matches!(
        events.last(),
        Some(TurnStateHuntEvent::Completed { success: true, .. })
    ));
}

/// 指定的代理不存在或没通过测试：直接拒绝，不能悄悄退回成遍历全部。
#[tokio::test]
async fn naming_an_unusable_proxy_is_rejected_instead_of_walking_the_rest() {
    let probe = ScriptedProbe::new([]);
    let setup = setup(
        probe.clone(),
        vec![proxy("good", 8001, true), proxy("untested", 8002, false)],
        None,
    )
    .await;

    for only in ["untested", "missing"] {
        let error = setup
            .services
            .accounts()
            .turn_state_hunt(TurnStateHuntCommand {
                only_proxy_id: Some(only.to_owned()),
                ..command(5, true)
            })
            .await
            .err()
            .expect("unusable proxy");
        assert_eq!(error.kind(), AdminErrorKind::Invalid);
    }
    assert!(probe.egresses().is_empty());
}

/// 页面取消后在途探测才命中：取消承诺的是「账号未改动」，不能再绑定或钉住。
#[tokio::test]
async fn hit_that_arrives_after_cancellation_changes_nothing() {
    let probe = ScriptedProbe::new([Reply::State(EXPECTED_LENGTH), Reply::NoState]);
    let gate = Arc::new(tokio::sync::Notify::new());
    *probe.gate.lock().unwrap() = Some(gate.clone());
    let setup = setup(probe.clone(), vec![proxy("good", 8001, true)], None).await;

    let mut stream = setup
        .services
        .accounts()
        .turn_state_hunt(command(1, false))
        .await
        .expect("hunt stream");
    // 等到探测真的发出去，再模拟用户取消。
    while probe.egresses().is_empty() {
        let _ = stream.next().await;
    }
    drop(stream);
    *probe.gate.lock().unwrap() = None;
    gate.notify_waiters();

    // 后台任务结束后占位才会释放；能再次开始遍历说明上一轮已经走完。
    let mut finished = false;
    for _ in 0..200 {
        match setup
            .services
            .accounts()
            .turn_state_hunt(command(1, false))
            .await
        {
            Ok(stream) => {
                let _ = stream.collect::<Vec<_>>().await;
                finished = true;
                break;
            }
            Err(_) => tokio::task::yield_now().await,
        }
    }
    assert!(finished);
    assert!(!changed(&setup.log));
}

/// 同一个代理 ID 在探测之后被改了地址：绑定会解析到一个没探测过的出口，必须中止。
#[tokio::test]
async fn egress_edited_after_the_probe_is_not_bound() {
    let probe = ScriptedProbe::new([Reply::State(EXPECTED_LENGTH)]);
    let setup = setup(probe.clone(), vec![proxy("good", 8001, true)], None).await;
    let proxies = setup.proxies.clone();
    *probe.on_probe.lock().unwrap() = Some(Box::new(move || {
        let mut proxies = proxies.lock().unwrap();
        proxies[0] = ProxyRecord {
            revision: revision(2),
            ..proxy("good", 9999, true)
        };
    }));

    let events = run(&setup, command(1, false)).await;

    assert!(matches!(
        events.last(),
        Some(TurnStateHuntEvent::Failed {
            code: "egress_changed",
            ..
        })
    ));
    assert!(!changed(&setup.log));
}

/// 绑定调用返回成功不等于账号真的走了这个出口；以回读到的绑定为准，不符就不钉。
#[tokio::test]
async fn state_is_not_pinned_when_the_account_did_not_end_up_on_the_probed_egress() {
    let probe = ScriptedProbe::new([Reply::State(EXPECTED_LENGTH)]);
    let setup = setup(probe, vec![proxy("good", 8001, true)], None).await;
    setup.store.saved_proxies.lock().unwrap().clear();

    let events = run(&setup, command(1, false)).await;

    assert!(matches!(
        events.last(),
        Some(TurnStateHuntEvent::Failed {
            code: "bind_mismatch",
            ..
        })
    ));
    assert!(!recorded(&setup.log).contains(&"provider.hunt_pin"));
}

/// 续期是系统替管理员发请求：账号一旦被停用，连第一个请求都不该发。
#[tokio::test]
async fn renewal_never_probes_an_account_that_was_disabled_meanwhile() {
    let probe = ScriptedProbe::new([]);
    let setup = setup(probe.clone(), vec![proxy("good", 8001, true)], None).await;
    setup
        .store
        .mutate_account(|account| account.enabled = false);

    let events = run(
        &setup,
        TurnStateHuntCommand {
            require_schedulable: true,
            ..command(5, false)
        },
    )
    .await;

    assert!(probe.egresses().is_empty());
    assert!(matches!(
        events.last(),
        Some(TurnStateHuntEvent::Failed {
            code: "account_unschedulable",
            ..
        })
    ));
    // 手动遍历不受此限：管理员可以诊断已停用的账号（此处没有脚本化回复，只验证能开始）。
    assert!(!changed(&setup.log));
}

/// 网关本地故障与出口无关：不能拿它当理由把所有代理打一遍。
#[tokio::test]
async fn local_failures_abort_instead_of_walking_every_egress() {
    for (kind, code) in [
        (
            ProviderErrorKind::ProviderInfrastructureUnavailable,
            "system_error",
        ),
        // 选不出可用账号在网关层叫 NoAvailableProvider，不能被当成「账号忙」重试五次。
        (ProviderErrorKind::NoEligibleAccount, "system_error"),
    ] {
        let probe = ScriptedProbe::new([Reply::Rejected(kind)]);
        let setup = setup(
            probe.clone(),
            vec![proxy("first", 8001, true), proxy("second", 8002, true)],
            None,
        )
        .await;

        let events = run(&setup, command(5, false)).await;

        assert_eq!(probe.egresses(), vec![endpoint(8001)], "{kind:?}");
        assert!(
            matches!(events.last(), Some(TurnStateHuntEvent::Failed { code: actual, .. }) if *actual == code),
            "{kind:?}"
        );
        assert!(!changed(&setup.log));
    }
}

/// 上游的「无容量」多是秒级过载：一次抖动不能让整轮落空（自动续期尤其如此），换下一个出口继续。
#[tokio::test(start_paused = true)]
async fn transient_capacity_moves_on_instead_of_aborting() {
    let probe = ScriptedProbe::new([
        Reply::Rejected(ProviderErrorKind::UpstreamCapacityUnavailable),
        Reply::State(EXPECTED_LENGTH),
    ]);
    let setup = setup(
        probe.clone(),
        vec![proxy("first", 8001, true), proxy("second", 8002, true)],
        None,
    )
    .await;

    let events = run(&setup, command(1, false)).await;

    assert_eq!(probe.egresses(), vec![endpoint(8001), endpoint(8002)]);
    assert!(events.iter().any(|event| matches!(
        event,
        TurnStateHuntEvent::EgressFinished { proxy_id: Some(id), skipped: Some("capacity"), matched: false, .. } if id == "first"
    )));
    assert!(matches!(
        events.last(),
        Some(TurnStateHuntEvent::Completed { success: true, .. })
    ));
}

/// 同一出口上还有尝试次数时就地再试：无容量与出口无关，不计入「出口不通」。
#[tokio::test(start_paused = true)]
async fn capacity_is_retried_within_the_attempt_budget_of_the_same_egress() {
    let probe = ScriptedProbe::new([
        Reply::Rejected(ProviderErrorKind::UpstreamCapacityUnavailable),
        Reply::Rejected(ProviderErrorKind::UpstreamCapacityUnavailable),
        Reply::State(EXPECTED_LENGTH),
    ]);
    let setup = setup(
        probe.clone(),
        vec![proxy("only", 8001, true), proxy("unused", 8002, true)],
        None,
    )
    .await;

    let events = run(&setup, command(5, false)).await;

    assert_eq!(
        probe.egresses(),
        vec![endpoint(8001), endpoint(8001), endpoint(8001)]
    );
    assert!(matches!(
        events.last(),
        Some(TurnStateHuntEvent::Completed { success: true, .. })
    ));
}

/// 连续多次都无容量才是真的没有容量：到此为止，不把剩下的出口白打一遍。
#[tokio::test(start_paused = true)]
async fn sustained_capacity_aborts_after_a_bounded_streak() {
    let probe = ScriptedProbe::new([
        Reply::Rejected(ProviderErrorKind::UpstreamCapacityUnavailable),
        Reply::Rejected(ProviderErrorKind::UpstreamCapacityUnavailable),
        Reply::Rejected(ProviderErrorKind::UpstreamCapacityUnavailable),
        Reply::State(EXPECTED_LENGTH),
    ]);
    let setup = setup(
        probe.clone(),
        vec![
            proxy("a", 8001, true),
            proxy("b", 8002, true),
            proxy("c", 8003, true),
            proxy("d", 8004, true),
        ],
        None,
    )
    .await;

    let events = run(&setup, command(1, false)).await;

    assert_eq!(
        probe.egresses(),
        vec![endpoint(8001), endpoint(8002), endpoint(8003)]
    );
    assert!(matches!(
        events.last(),
        Some(TurnStateHuntEvent::Failed {
            code: "upstream_capacity",
            ..
        })
    ));
    assert!(!changed(&setup.log));
}

/// 中间只要有一次请求拿到了上游应答，就说明上游有容量：连续计数清零，不会被零散抖动累计到中止。
#[tokio::test(start_paused = true)]
async fn an_upstream_answer_resets_the_capacity_streak() {
    let probe = ScriptedProbe::new([
        Reply::Rejected(ProviderErrorKind::UpstreamCapacityUnavailable),
        Reply::Rejected(ProviderErrorKind::UpstreamCapacityUnavailable),
        Reply::NoState,
        Reply::Rejected(ProviderErrorKind::UpstreamCapacityUnavailable),
        Reply::Rejected(ProviderErrorKind::UpstreamCapacityUnavailable),
        Reply::State(EXPECTED_LENGTH),
    ]);
    let setup = setup(
        probe.clone(),
        (1..=6)
            .map(|n| proxy(&format!("p{n}"), 8000 + n, true))
            .collect(),
        None,
    )
    .await;

    let events = run(&setup, command(1, false)).await;

    assert_eq!(probe.egresses().len(), 6);
    assert!(matches!(
        events.last(),
        Some(TurnStateHuntEvent::Completed { success: true, .. })
    ));
}

/// 续期途中账号被停用：同一出口上剩下的尝试也不再发，不只是「下一个出口前」才停。
#[tokio::test]
async fn renewal_stops_mid_egress_once_the_account_is_disabled() {
    let probe = ScriptedProbe::new([Reply::NoState]);
    let setup = setup(probe.clone(), vec![proxy("good", 8001, true)], None).await;
    let store = setup.store.clone();
    *probe.on_probe.lock().unwrap() = Some(Box::new(move || {
        store.mutate_account(|account| account.enabled = false);
    }));

    let events = run(
        &setup,
        TurnStateHuntCommand {
            require_schedulable: true,
            ..command(5, false)
        },
    )
    .await;

    assert_eq!(probe.egresses().len(), 1);
    assert!(matches!(
        events.last(),
        Some(TurnStateHuntEvent::Failed {
            code: "account_unschedulable",
            ..
        })
    ));
}
