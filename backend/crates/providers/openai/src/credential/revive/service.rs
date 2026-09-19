//! 401 失效后把已归档的签名号池交给 revive-api，并把恢复到的 AT/RT 写回原账号。

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use gateway_core::account::{CredentialState, ProviderAccount};
use gateway_core::routing::ProviderKind;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, OwnedMutexGuard};

use super::client::{ReviveApiClient, ReviveClientError};
use super::detect::{RecoveredOAuthTokens, document_accounts, recovered_oauth_tokens};
use super::store::{ReviveExportStore, ReviveStoreError};
use crate::config::CodexReviveSettings;
use crate::credential::repository::{CodexCredentialRepository, CredentialRepositoryError};
use crate::credential::types::{CodexOAuthSecret, parse_access_token_expiration};

const FAILURE_COOLDOWN: Duration = Duration::from_secs(30 * 60);
const PROVIDER_NAME: &str = "openai";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodexReviveError {
    #[error(transparent)]
    Store(#[from] ReviveStoreError),
    #[error(transparent)]
    Client(#[from] ReviveClientError),
    #[error("revive credential store is unavailable")]
    Repository,
    #[error("account has no signed Guanlan export")]
    Unsupported,
    #[error("revive operation is already running")]
    Busy,
    #[error("revive operation is cooling down")]
    Cooldown,
    #[error("Guanlan returned no recovered credentials for this account")]
    NoRecovery,
}

pub struct CodexReviveService {
    settings: CodexReviveSettings,
    store: ReviveExportStore,
    client: ReviveApiClient,
    repository: CodexCredentialRepository,
    operation: Arc<Mutex<()>>,
}

impl std::fmt::Debug for CodexReviveService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexReviveService")
            .field("enabled", &self.settings.enabled)
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

impl CodexReviveService {
    pub fn new(
        data_dir: PathBuf,
        settings: CodexReviveSettings,
        repository: CodexCredentialRepository,
    ) -> Result<Self, CodexReviveError> {
        let client = ReviveApiClient::new(settings.clone())?;
        Ok(Self {
            store: ReviveExportStore::new(data_dir)?,
            settings,
            client,
            repository,
            operation: Arc::new(Mutex::new(())),
        })
    }

    pub fn for_test(
        data_dir: PathBuf,
        settings: CodexReviveSettings,
        repository: CodexCredentialRepository,
    ) -> Result<Self, CodexReviveError> {
        let client = ReviveApiClient::new(settings.clone())?
            .with_timing(Duration::from_millis(1), Duration::from_secs(5));
        Ok(Self {
            store: ReviveExportStore::new(data_dir)?,
            settings,
            client,
            repository,
            operation: Arc::new(Mutex::new(())),
        })
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.settings.enabled
    }

    pub fn record_signed_document(&self, payload: &Value) -> Result<(), CodexReviveError> {
        self.store.record_signed_document(payload)?;
        Ok(())
    }

    pub fn supports_account(&self, account: &ProviderAccount) -> Result<bool, CodexReviveError> {
        if account.provider().as_str() != PROVIDER_NAME || account.authentication_kind() != "oauth"
        {
            return Ok(false);
        }
        let Some(user_id) = account.upstream_user_id() else {
            return Ok(false);
        };
        Ok(self.store.export_for_user(user_id)?.is_some())
    }

    /// 手动复活只准备目标账号的凭据，写回与审计沿用管理端 CAS 事务。
    pub async fn prepare_manual_revival(
        &self,
        account: &ProviderAccount,
    ) -> Result<(CodexOAuthSecret, OwnedMutexGuard<()>), CodexReviveError> {
        let guard = self
            .operation
            .clone()
            .try_lock_owned()
            .map_err(|_| CodexReviveError::Busy)?;
        if !self.supports_account(account)? {
            return Err(CodexReviveError::Unsupported);
        }
        let user_id = account
            .upstream_user_id()
            .ok_or(CodexReviveError::Unsupported)?;
        let (digest, bytes) = self
            .store
            .export_for_user(user_id)?
            .ok_or(CodexReviveError::Unsupported)?;
        if !self.due_for_attempt(&digest) {
            return Err(CodexReviveError::Cooldown);
        }
        let recovered = match self.client.recover_signed_export(&bytes).await {
            Ok(document) => document,
            Err(error) => {
                let error = CodexReviveError::Client(error);
                self.write_state(&digest, "failed", Some(error_class(&error)));
                return Err(error);
            }
        };
        let tokens = recovered_accounts(&recovered)
            .into_iter()
            .find(|tokens| tokens.user_id == user_id)
            .ok_or(CodexReviveError::NoRecovery)?;
        let metadata = crate::credential::types::parse_chatgpt_jwt_claims(&tokens.access_token)
            .map_err(|_| CodexReviveError::NoRecovery)?;
        if metadata
            .chatgpt_account_id
            .as_deref()
            .is_some_and(|id| Some(id) != account.upstream_account_id())
        {
            return Err(CodexReviveError::NoRecovery);
        }
        let secret = CodexOAuthSecret {
            access_token: SecretString::from(tokens.access_token),
            refresh_token: tokens.refresh_token.map(SecretString::from),
            id_token: tokens.id_token.map(SecretString::from),
        };
        Ok((secret, guard))
    }

    pub async fn run_cycle(&self) -> Result<CodexReviveCycleSummary, CodexReviveError> {
        let mut summary = CodexReviveCycleSummary::default();
        if !self.settings.enabled {
            return Ok(summary);
        }
        let Ok(_guard) = self.operation.try_lock() else {
            return Ok(summary);
        };
        let provider =
            ProviderKind::new(PROVIDER_NAME).map_err(|_| CodexReviveError::Repository)?;
        let accounts = self
            .repository
            .store()
            .list_for_provider(&provider)
            .await
            .map_err(|_| CodexReviveError::Repository)?;
        let mut groups: BTreeMap<String, Vec<ProviderAccount>> = BTreeMap::new();
        for account in accounts {
            if !eligible(&account) {
                continue;
            }
            let Some(user_id) = account.upstream_user_id() else {
                continue;
            };
            match self.store.export_for_user(user_id) {
                Ok(Some((digest, _))) => {
                    groups.entry(digest).or_default().push(account);
                }
                Ok(None) => summary.skipped_unsigned += 1,
                Err(error) => return Err(error.into()),
            }
        }
        for (digest, accounts) in groups {
            if !self.due_for_attempt(&digest) {
                summary.cooled_down += 1;
                continue;
            }
            match self.revive_export(&digest, &accounts).await {
                Ok(applied) => {
                    summary.applied += applied;
                    self.write_state(&digest, "recovered", None);
                }
                Err(error) => {
                    tracing::warn!(
                        export = %digest,
                        error = %error,
                        "OpenAI signed-export revive failed"
                    );
                    summary.failed += 1;
                    self.write_state(&digest, "failed", Some(error_class(&error)));
                }
            }
        }
        Ok(summary)
    }

    async fn revive_export(
        &self,
        _digest: &str,
        accounts: &[ProviderAccount],
    ) -> Result<u64, CodexReviveError> {
        let Some(user_id) = accounts
            .iter()
            .find_map(|account| account.upstream_user_id().map(str::to_owned))
        else {
            return Ok(0);
        };
        let Some((_, bytes)) = self.store.export_for_user(&user_id)? else {
            return Ok(0);
        };
        let recovered = self.client.recover_signed_export(&bytes).await?;
        let mut applied = 0_u64;
        for tokens in recovered_accounts(&recovered) {
            if let Some(account) = accounts
                .iter()
                .find(|account| account.upstream_user_id() == Some(tokens.user_id.as_str()))
            {
                self.apply_tokens(account, tokens).await?;
                applied += 1;
            }
        }
        Ok(applied)
    }

    async fn apply_tokens(
        &self,
        account: &ProviderAccount,
        tokens: RecoveredOAuthTokens,
    ) -> Result<(), CodexReviveError> {
        let loaded = self
            .repository
            .store()
            .load_current_credential(account.id())
            .await
            .map_err(|_| CodexReviveError::Repository)?;
        let expires_at = parse_access_token_expiration(&tokens.access_token).map(SystemTime::from);
        let secret = CodexOAuthSecret {
            access_token: SecretString::from(tokens.access_token),
            refresh_token: tokens.refresh_token.map(SecretString::from),
            id_token: tokens.id_token.map(SecretString::from),
        };
        match self
            .repository
            .rotate_refreshed_oauth_secret(&loaded.account, secret, expires_at, None)
            .await
        {
            Ok(_) => Ok(()),
            Err(CredentialRepositoryError::RevisionConflict) => Ok(()),
            Err(_) => Err(CodexReviveError::Repository),
        }
    }

    fn due_for_attempt(&self, digest: &str) -> bool {
        let Ok(bytes) = fs::read(self.store.state_path(digest)) else {
            return true;
        };
        let Ok(state) = serde_json::from_slice::<ReviveAttemptState>(&bytes) else {
            return true;
        };
        if state.status != "failed" {
            return true;
        }
        let Some(next) = state.next_attempt_unix else {
            return true;
        };
        Utc::now().timestamp() >= next
    }

    fn write_state(&self, digest: &str, status: &str, error_class: Option<&str>) {
        let next = if status == "failed" {
            Some(
                (Utc::now() + chrono::Duration::from_std(FAILURE_COOLDOWN).unwrap_or_default())
                    .timestamp(),
            )
        } else {
            None
        };
        let state = ReviveAttemptState {
            status: status.to_owned(),
            next_attempt_unix: next,
            error_class: error_class.map(str::to_owned),
        };
        if let Ok(bytes) = serde_json::to_vec(&state) {
            let _ = fs::write(self.store.state_path(digest), bytes);
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CodexReviveCycleSummary {
    pub applied: u64,
    pub failed: u64,
    pub skipped_unsigned: u64,
    pub cooled_down: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct ReviveAttemptState {
    status: String,
    #[serde(default)]
    next_attempt_unix: Option<i64>,
    #[serde(default)]
    error_class: Option<String>,
}

fn eligible(account: &ProviderAccount) -> bool {
    account.enabled()
        && account.authentication_kind() == "oauth"
        && account.credential_state() == CredentialState::Expired
}

fn recovered_accounts(document: &Value) -> Vec<RecoveredOAuthTokens> {
    document_accounts(document)
        .iter()
        .filter_map(recovered_oauth_tokens)
        .collect()
}

fn error_class(error: &CodexReviveError) -> &'static str {
    match error {
        CodexReviveError::Client(ReviveClientError::RateLimited) => "rate_limited",
        CodexReviveError::Client(ReviveClientError::Expired) => "snapshot_expired",
        CodexReviveError::Client(ReviveClientError::Rejected) => "rejected",
        CodexReviveError::Client(ReviveClientError::Timeout) => "timeout",
        _ => "failed",
    }
}
