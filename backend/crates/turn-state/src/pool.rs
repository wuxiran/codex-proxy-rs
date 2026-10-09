//! 票池只读观测合同与聚合；运行资源仍由 Provider 和连接池持有。

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
};

use serde::Serialize;

pub type PoolSnapshotFuture<'a> =
    Pin<Box<dyn Future<Output = Result<PoolSnapshot, PoolUnavailable>> + Send + 'a>>;

pub trait PoolRuntime: Send + Sync {
    fn snapshot(&self) -> PoolSnapshotFuture<'_>;
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("pool runtime snapshot is unavailable")]
pub struct PoolUnavailable;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolTicket {
    pub model: String,
    pub gateway: Option<String>,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolConnection {
    pub id: String,
    pub model: String,
    pub gateway: Option<String>,
    pub verification: String,
    pub available: bool,
    pub verified_at_ms: Option<u64>,
    pub verification_expires_at_ms: Option<u64>,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolAttempt {
    pub at_ms: u64,
    pub kind: String,
    pub attempts: u64,
    pub verdict: Option<String>,
    pub error: Option<String>,
    pub gateway: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolActivity {
    pub in_flight: bool,
    pub cooldown_until_ms: Option<u64>,
    pub last: Option<PoolAttempt>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolAccount {
    pub account_id: String,
    pub name: String,
    pub participating: bool,
    pub schedulable: bool,
    pub phase: String,
    pub tickets: Vec<PoolTicket>,
    pub connections: Vec<PoolConnection>,
    pub mint: PoolActivity,
    pub warm: PoolActivity,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolGateway {
    pub gateway: String,
    pub account_ids: BTreeSet<String>,
    pub models: BTreeSet<String>,
    pub tickets: usize,
    pub connections: usize,
    pub available_connections: usize,
    pub verified_connections: usize,
    pub next_expiry_ms: Option<u64>,
    pub latest_verified_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolTotals {
    pub gateways: usize,
    pub tickets: usize,
    pub connections: usize,
    pub available_connections: usize,
    pub verified_connections: usize,
    pub preparing_accounts: usize,
    pub cooling_accounts: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolSnapshot {
    pub observed_at_ms: u64,
    pub valid_until_ms: u64,
    pub scope: &'static str,
    pub totals: PoolTotals,
    pub gateways: Vec<PoolGateway>,
    pub accounts: Vec<PoolAccount>,
}

impl PoolSnapshot {
    /// 只聚合本实例实际观察到的资源，不推算上游全网库存或累计使用时长。
    pub fn new(now_ms: u64, mut accounts: Vec<PoolAccount>) -> Self {
        let mut gateways = BTreeMap::<String, PoolGateway>::new();
        let mut totals = PoolTotals::default();
        let mut valid_until_ms = now_ms.saturating_add(5_000);
        accounts.sort_by(|a, b| a.account_id.cmp(&b.account_id));
        for account in &mut accounts {
            account
                .tickets
                .retain(|ticket| ticket.expires_at_ms > now_ms);
            for connection in &mut account.connections {
                let expired = connection.expires_at_ms <= now_ms
                    || (connection.verification == "fresh"
                        && connection
                            .verification_expires_at_ms
                            .is_none_or(|until| until <= now_ms));
                if !account.participating || !account.schedulable || expired {
                    connection.available = false;
                    if expired && connection.verification == "fresh" {
                        connection.verification = "expired".to_owned();
                    }
                }
            }
            if account.phase == "ready"
                && !account
                    .connections
                    .iter()
                    .any(|connection| connection.available)
            {
                account.phase = "idle".to_owned();
            }
            totals.tickets += account.tickets.len();
            totals.connections += account.connections.len();
            totals.preparing_accounts +=
                usize::from(account.mint.in_flight || account.warm.in_flight);
            totals.cooling_accounts +=
                usize::from([&account.mint, &account.warm].iter().any(|activity| {
                    activity
                        .cooldown_until_ms
                        .is_some_and(|until| until > now_ms)
                }));
            for activity in [&account.mint, &account.warm] {
                if let Some(until) = activity.cooldown_until_ms.filter(|until| *until > now_ms) {
                    valid_until_ms = valid_until_ms.min(until);
                }
            }
            for ticket in &account.tickets {
                valid_until_ms = valid_until_ms.min(ticket.expires_at_ms);
                if let Some(name) = &ticket.gateway {
                    let row = gateway(&mut gateways, name, account);
                    row.models.insert(ticket.model.clone());
                    row.tickets += 1;
                    row.next_expiry_ms = Some(
                        row.next_expiry_ms
                            .map_or(ticket.expires_at_ms, |at| at.min(ticket.expires_at_ms)),
                    );
                }
            }
            for connection in &account.connections {
                if connection.available {
                    valid_until_ms = valid_until_ms.min(connection.expires_at_ms);
                    if let Some(expiry) = connection.verification_expires_at_ms {
                        valid_until_ms = valid_until_ms.min(expiry);
                    }
                }
                let verified = connection.available && connection.verification == "fresh";
                totals.available_connections += usize::from(connection.available);
                totals.verified_connections += usize::from(verified);
                if let Some(name) = &connection.gateway {
                    let row = gateway(&mut gateways, name, account);
                    row.models.insert(connection.model.clone());
                    row.connections += 1;
                    row.available_connections += usize::from(connection.available);
                    row.verified_connections += usize::from(verified);
                    if connection.available {
                        let expiry = connection
                            .verification_expires_at_ms
                            .unwrap_or(connection.expires_at_ms)
                            .min(connection.expires_at_ms);
                        row.next_expiry_ms =
                            Some(row.next_expiry_ms.map_or(expiry, |at| at.min(expiry)));
                    }
                    row.latest_verified_at_ms =
                        row.latest_verified_at_ms.max(connection.verified_at_ms);
                }
            }
            for last in [&account.mint.last, &account.warm.last]
                .into_iter()
                .flatten()
            {
                if let Some(name) = &last.gateway {
                    gateway(&mut gateways, name, account);
                }
            }
        }
        totals.gateways = gateways.len();
        Self {
            observed_at_ms: now_ms,
            valid_until_ms,
            scope: "current_process",
            totals,
            gateways: gateways.into_values().collect(),
            accounts,
        }
    }
}

fn gateway<'a>(
    gateways: &'a mut BTreeMap<String, PoolGateway>,
    name: &str,
    account: &PoolAccount,
) -> &'a mut PoolGateway {
    let row = gateways
        .entry(name.to_owned())
        .or_insert_with(|| PoolGateway {
            gateway: name.to_owned(),
            ..PoolGateway::default()
        });
    row.account_ids.insert(account.account_id.clone());
    row
}
