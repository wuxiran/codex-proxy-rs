//! 从账号、模板与实际连接 owner 读取一个只读快照，读取失败不伪装成空库存。

use gateway_core::account::CredentialState;
use secrecy::ExposeSecret as _;
use std::{collections::BTreeMap, time::SystemTime};
use turn_state::pool::{PoolAccount, PoolSnapshot, PoolUnavailable};

use crate::{
    credential::{
        CODEX_AUTHENTICATION_KIND_OAUTH, CodexCredentialRepository, CodexRuntimeAuthentication,
    },
    route_pair::RoutePairRef,
    transport::{CodexWebSocketPool, WarmConnectionApproval},
    turn_state_mint::CloudMintService,
    turn_state_pin::{TurnStatePins, credential_binding, egress_fingerprint},
    ws_warm_pool::WarmPoolService,
};

pub(crate) async fn snapshot(
    repository: &CodexCredentialRepository,
    pins: &TurnStatePins,
    pool: &CodexWebSocketPool,
    warm: &WarmPoolService,
    mint: &CloudMintService,
) -> Result<PoolSnapshot, PoolUnavailable> {
    let started = SystemTime::now();
    let now_ms = millis(started);
    let settings = pins.service().settings();
    let accounts = repository
        .list_for_provider()
        .await
        .map_err(|_| PoolUnavailable)?;
    let mut rows = Vec::new();
    for account in accounts {
        if account.authentication_kind() != CODEX_AUTHENTICATION_KIND_OAUTH {
            continue;
        }
        let Some(account) = repository
            .store()
            .get_account(account.id())
            .await
            .map_err(|_| PoolUnavailable)?
        else {
            continue;
        };
        let credential = repository
            .load_runtime_credential(&account)
            .await
            .map_err(|_| PoolUnavailable)?;
        let CodexRuntimeAuthentication::OAuth(secret) = &credential.authentication else {
            continue;
        };
        let participating = credential.turn_state_pin.is_some();
        let schedulable = account.enabled()
            && account.credential_state() == CredentialState::Ready
            && account
                .access_token_expires_at()
                .is_none_or(|until| until > SystemTime::now())
            && !account.quota().is_exhausted();
        let route = RoutePairRef::sent(&credential.cookies);
        let egress = egress_fingerprint(account.outbound_proxy().map(|proxy| proxy.expose_url()));
        let tickets = credential
            .turn_state_pin
            .as_deref()
            .map(|generation| {
                let binding = credential_binding(generation, secret.access_token.expose_secret());
                pins.service().pool_tickets(
                    account.id().as_str(),
                    &binding,
                    &egress,
                    route.as_ref().map(|route| route.fingerprint.as_str()),
                    SystemTime::now(),
                )
            })
            .unwrap_or_default();
        let fingerprints: BTreeMap<_, _> = tickets
            .iter()
            .map(|(ticket, fingerprint)| (ticket.model.clone(), *fingerprint))
            .collect();
        let policy = WarmConnectionApproval::policy_key(
            &settings.warm_pool,
            settings.ttl().min(settings.cloud_mint.ticket_ttl()),
            credential.turn_state_pin.as_deref(),
        );
        let mut connections = pool.warm_inventory(
            account.id().as_str(),
            policy,
            &fingerprints,
            route.as_ref(),
            &crate::transport::client::egress_key(account.id().as_str(), account.outbound_proxy()),
            now_ms,
        );
        for connection in &mut connections {
            connection.available &= participating
                && schedulable
                && !settings.dry_run
                && settings.warm_pool.enabled
                && settings.warm_pool.business_reuse
                && (!settings.warm_pool.require_verified || connection.verification == "fresh");
        }
        rows.push(PoolAccount {
            account_id: account.id().as_str().to_owned(),
            name: account.name().to_owned(),
            participating,
            schedulable,
            phase: String::new(),
            tickets: tickets.into_iter().map(|(ticket, _)| ticket).collect(),
            connections,
            mint: Default::default(),
            warm: Default::default(),
        });
    }
    // 列表加载期间参数可能变化，旧策略下的验证结果不能当作新策略的绿色状态。
    if pins.service().settings() != settings {
        return Err(PoolUnavailable);
    }
    let observed = millis(SystemTime::now());
    let mut mint_activity = mint.pool_activity(observed)?;
    let mut warm_activity = warm.pool_activity(observed)?;
    for row in &mut rows {
        row.mint = mint_activity.remove(&row.account_id).unwrap_or_default();
        row.warm = warm_activity.remove(&row.account_id).unwrap_or_default();
        row.phase = if !row.participating {
            "inactive"
        } else if !row.schedulable {
            "unavailable"
        } else if row.warm.in_flight {
            "verifying"
        } else if row.mint.in_flight {
            "minting"
        } else if row
            .connections
            .iter()
            .any(|connection| connection.available && connection.expires_at_ms > observed)
        {
            "ready"
        } else if [&row.mint, &row.warm].iter().any(|activity| {
            activity
                .cooldown_until_ms
                .is_some_and(|until| until > observed)
        }) {
            "cooling"
        } else {
            "idle"
        }
        .to_owned();
    }
    Ok(PoolSnapshot::new(observed, rows))
}

fn millis(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}
