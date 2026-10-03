//! fork: account-ticket — 账号「已过期」判定，打票与预热都据此跳过。
//!
//! 与管理目录的口径一致（`gateway-store` admin_queries）：购买票据 `expires_at` 到点、
//! 且账号已不能调度（停用、凭据不可用、令牌过期或额度耗尽）才算已过期；
//! 票据到点但还在正常服务的账号照常打票。

use std::{sync::Arc, time::SystemTime};

use gateway_core::account::{CredentialState, ProviderAccount, ProviderAccountStore};

/// 账号是否已过期；存储查不到票据时视为未过期，不因此拦住打票。
pub(crate) async fn is_expired(
    store: &Arc<dyn ProviderAccountStore>,
    account: &ProviderAccount,
    now: SystemTime,
) -> bool {
    if schedulable(account, now) {
        return false;
    }
    matches!(
        store.account_ticket_expires_at(account.id()).await,
        Ok(Some(expires_at)) if expires_at <= now
    )
}

fn schedulable(account: &ProviderAccount, now: SystemTime) -> bool {
    account.enabled()
        && account.credential_state() == CredentialState::Ready
        && account
            .access_token_expires_at()
            .is_none_or(|expires_at| expires_at > now)
        && !account.quota().is_exhausted()
}
