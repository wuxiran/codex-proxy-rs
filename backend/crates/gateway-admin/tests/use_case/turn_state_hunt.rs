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
    error::{GatewayError, GatewayErrorKind},
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
}

/// 按顺序回放探测结果，并记下每次请求走的出口。
struct ScriptedProbe {
    replies: Mutex<VecDeque<Reply>>,
    egresses: Mutex<Vec<Option<String>>>,
    on_probe: Mutex<Option<Box<dyn Fn() + Send>>>,
}

impl ScriptedProbe {
    fn new(replies: impl IntoIterator<Item = Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().collect()),
            egresses: Mutex::new(Vec::new()),
            on_probe: Mutex::new(None),
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
        Box::pin(async move {
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
    let services = super::AdminHarness::new()
        .accounts(FakeAccountStore::with_account(account, log.clone()))
        .provider(provider.clone())
        .probe(probe)
        .proxies(Arc::new(TestProxies {
            records: Some(proxies),
            ..Default::default()
        }))
        .build()
        .await;
    Setup {
        services,
        provider,
        log,
    }
}

fn command(attempts: u8, include_direct: bool) -> TurnStateHuntCommand {
    TurnStateHuntCommand {
        account_id: ProviderAccountId::new("acct_test").unwrap(),
        upstream_model: UpstreamModelId::new("gpt-6-astra").unwrap(),
        attempts,
        include_direct,
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
    for attempts in [0, 21] {
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
