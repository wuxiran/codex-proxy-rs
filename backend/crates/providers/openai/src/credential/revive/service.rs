//! 仅为管理员勾选、身份匹配且有签名原件的失效账号执行观澜复活。

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use gateway_core::account::{CredentialState, ProviderAccount};
use gateway_core::routing::ProviderKind;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::client::{ReviveApiClient, ReviveClientError};
use super::detect::{RecoveredOAuthTokens, document_accounts, recovered_oauth_tokens};
use super::store::{ReviveExportStore, ReviveStoreError};
use crate::config::CodexReviveSettings;
use crate::credential::types::{CodexOAuthSecret, parse_access_token_expiration};
use crate::credential::{
    CodexCredentialCodec, CodexCredentialRepository, CredentialRepositoryError,
};

const FAILURE_COOLDOWN: i64 = 30 * 60;
const PROVIDER_NAME: &str = "openai";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodexReviveError {
    #[error(transparent)]
    Store(#[from] ReviveStoreError),
    #[error(transparent)]
    Client(#[from] ReviveClientError),
    #[error("revive credential store is unavailable")]
    Repository,
    #[error("revive result does not match the requested account")]
    IdentityMismatch,
    #[error("no matching account was recovered")]
    NoRecoveredAccount,
}

pub struct CodexReviveService {
    settings: CodexReviveSettings,
    store: ReviveExportStore,
    client: ReviveApiClient,
    repository: CodexCredentialRepository,
}

impl std::fmt::Debug for CodexReviveService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexReviveService")
            .field("enabled", &self.settings.enabled)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuanlanReviveStatus {
    pub source: Option<&'static str>,
    pub eligible: bool,
    pub enabled: bool,
    pub service_enabled: bool,
    pub status: String,
    pub reason: Option<String>,
    pub last_attempt_at: Option<i64>,
    pub next_attempt_at: Option<i64>,
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

    pub fn record_raw_document(&self, raw: &[u8]) -> Result<(), CodexReviveError> {
        self.store.record_raw_document(raw)?;
        Ok(())
    }

    fn archive(
        &self,
        account: &ProviderAccount,
    ) -> Result<Option<(String, Vec<u8>)>, CodexReviveError> {
        let Some(user) = account.upstream_user_id() else {
            return Ok(None);
        };
        let Some((digest, bytes)) = self
            .store
            .export_for_user(user, account.upstream_account_id())?
        else {
            return Ok(None);
        };
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| ReviveStoreError::InvalidDocument)?;
        if !document_accounts(&value)
            .iter()
            .filter_map(recovered_oauth_tokens)
            .any(|t| identity_matches(account, &t))
        {
            return Err(CodexReviveError::IdentityMismatch);
        }
        Ok(Some((digest, bytes)))
    }

    pub fn account_status(&self, account: &ProviderAccount, enabled: bool) -> GuanlanReviveStatus {
        let mut status = GuanlanReviveStatus {
            source: None,
            eligible: false,
            enabled,
            service_enabled: self.enabled(),
            status: "unavailable".to_owned(),
            reason: None,
            last_attempt_at: None,
            next_attempt_at: None,
        };
        match self.archive(account) {
            Ok(Some((digest, _))) => {
                status.source = Some("guanlan");
                status.eligible = true;
                let attempt = self.attempt_state(&digest);
                status.last_attempt_at = attempt.as_ref().and_then(|s| s.last_attempt_unix);
                status.next_attempt_at = attempt.as_ref().and_then(|s| s.next_attempt_unix);
                if !enabled {
                    status.status = "disabled".to_owned();
                } else if !self.enabled() {
                    status.status = "service_disabled".to_owned();
                } else if !account.enabled() {
                    status.status = "paused".to_owned();
                } else if let Some(attempt) = attempt {
                    status.status = attempt.status;
                    status.reason = attempt.error_class;
                } else {
                    status.status = "waiting".to_owned();
                }
            }
            Ok(None) => status.reason = Some("missing_signed_export".to_owned()),
            Err(_) => status.reason = Some("invalid_signed_export".to_owned()),
        }
        status
    }

    async fn opted_in(&self, account: &ProviderAccount) -> Result<bool, CodexReviveError> {
        let loaded = self
            .repository
            .store()
            .load_current_credential(account.id())
            .await
            .map_err(|_| CodexReviveError::Repository)?;
        if loaded.account.revision() != account.revision() {
            return Ok(false);
        }
        let data = CodexCredentialCodec::decode_complete(&loaded.credential)
            .map_err(|_| CodexReviveError::Repository)?;
        Ok(data.oauth().is_some_and(|o| o.guanlan_auto_revive))
    }

    pub async fn run_cycle(&self) -> Result<CodexReviveCycleSummary, CodexReviveError> {
        let mut summary = CodexReviveCycleSummary::default();
        if !self.settings.enabled {
            return Ok(summary);
        }
        let provider =
            ProviderKind::new(PROVIDER_NAME).map_err(|_| CodexReviveError::Repository)?;
        let all = self
            .repository
            .store()
            .list_for_provider(&provider)
            .await
            .map_err(|_| CodexReviveError::Repository)?;
        let mut groups: BTreeMap<String, (Vec<u8>, Vec<ProviderAccount>)> = BTreeMap::new();
        for account in all {
            if !eligible(&account) || !self.opted_in(&account).await? {
                continue;
            }
            match self.archive(&account) {
                Ok(Some((digest, bytes))) => groups
                    .entry(digest)
                    .or_insert_with(|| (bytes, Vec::new()))
                    .1
                    .push(account),
                Ok(None) => summary.skipped_unsigned += 1,
                Err(error) => {
                    tracing::warn!(account_id=%account.id().as_str(), error=%error, "Guanlan revive archive unavailable");
                    summary.failed += 1;
                }
            }
        }
        for (digest, (bytes, accounts)) in groups {
            if !self.due_for_attempt(&digest) {
                summary.cooled_down += 1;
                continue;
            }
            // 一份签名原件不能截取后重签：仅在其中每个账号都明确勾选且失效时提交，
            // 防止用户勾选单账号，却向第三方上传/复活未授权的其他账号。
            let document: Value =
                serde_json::from_slice(&bytes).map_err(|_| ReviveStoreError::InvalidDocument)?;
            if !document_accounts(&document).iter().all(|item| {
                recovered_oauth_tokens(item)
                    .is_some_and(|t| accounts.iter().any(|a| identity_matches(a, &t)))
            }) {
                self.write_state(&digest, "waiting", Some("batch_requires_all_accounts"))?;
                continue;
            }
            self.write_state(&digest, "running", None)?;
            match self.revive_export(&bytes, &accounts).await {
                Ok(applied) if applied == accounts.len() as u64 => {
                    summary.applied += applied;
                    self.write_state(&digest, "recovered", None)?;
                }
                Ok(applied) if applied > 0 => {
                    summary.applied += applied;
                    summary.failed += 1;
                    self.write_state(&digest, "failed", Some("partial_recovery"))?;
                }
                Ok(_) => {
                    summary.failed += 1;
                    self.write_state(&digest, "failed", Some("account_changed"))?;
                }
                Err(error) => {
                    tracing::warn!(error=%error, "OpenAI signed-export revive failed");
                    summary.failed += 1;
                    self.write_state(&digest, "failed", Some(error_class(&error)))?;
                }
            }
        }
        Ok(summary)
    }

    async fn revive_export(
        &self,
        bytes: &[u8],
        accounts: &[ProviderAccount],
    ) -> Result<u64, CodexReviveError> {
        let recovered = self.client.recover_signed_export(bytes).await?;
        let tokens: Vec<_> = document_accounts(&recovered)
            .iter()
            .filter_map(recovered_oauth_tokens)
            .collect();
        if tokens.is_empty() {
            return Err(CodexReviveError::NoRecoveredAccount);
        }
        if tokens
            .iter()
            .any(|token| !accounts.iter().any(|a| identity_matches(a, token)))
        {
            return Err(CodexReviveError::IdentityMismatch);
        }
        let mut applied = 0;
        for token in tokens {
            let Some(account) = accounts.iter().find(|a| identity_matches(a, &token)) else {
                return Err(CodexReviveError::IdentityMismatch);
            };
            applied += u64::from(self.apply_tokens(account, token).await?);
        }
        Ok(applied)
    }

    async fn apply_tokens(
        &self,
        snapshot: &ProviderAccount,
        tokens: RecoveredOAuthTokens,
    ) -> Result<bool, CodexReviveError> {
        let loaded = self
            .repository
            .store()
            .load_current_credential(snapshot.id())
            .await
            .map_err(|_| CodexReviveError::Repository)?;
        // 外部恢复期间管理员可能换凭据、取消勾选或禁用。仍按开始时的世代做 CAS。
        if loaded.account.revision() != snapshot.revision()
            || !eligible(&loaded.account)
            || !self.opted_in(&loaded.account).await?
        {
            return Ok(false);
        }
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
            Ok(_) => Ok(true),
            Err(CredentialRepositoryError::RevisionConflict) => Ok(false),
            Err(_) => Err(CodexReviveError::Repository),
        }
    }

    fn attempt_state(&self, digest: &str) -> Option<ReviveAttemptState> {
        fs::read(self.store.state_path(digest))
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
    }

    fn due_for_attempt(&self, digest: &str) -> bool {
        self.attempt_state(digest)
            .and_then(|s| s.next_attempt_unix)
            .is_none_or(|next| Utc::now().timestamp() >= next)
    }

    fn write_state(
        &self,
        digest: &str,
        status: &str,
        error_class: Option<&str>,
    ) -> Result<(), CodexReviveError> {
        let now = Utc::now().timestamp();
        let state = ReviveAttemptState {
            status: status.to_owned(),
            last_attempt_unix: Some(now),
            next_attempt_unix: matches!(status, "failed" | "running")
                .then_some(now + FAILURE_COOLDOWN),
            error_class: error_class.map(str::to_owned),
        };
        let raw = serde_json::to_vec(&state).map_err(|_| ReviveStoreError::InvalidIndex)?;
        super::store::atomic_write(&self.store.state_path(digest), &raw)?;
        Ok(())
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
    last_attempt_unix: Option<i64>,
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

fn identity_matches(account: &ProviderAccount, token: &RecoveredOAuthTokens) -> bool {
    account.upstream_user_id() == Some(token.user_id.as_str())
        && account.upstream_account_id() == token.workspace_id.as_deref()
}

fn error_class(error: &CodexReviveError) -> &'static str {
    match error {
        CodexReviveError::Client(ReviveClientError::RateLimited) => "rate_limited",
        CodexReviveError::Client(ReviveClientError::Expired) => "snapshot_expired",
        CodexReviveError::Client(ReviveClientError::Rejected) => "rejected",
        CodexReviveError::Client(ReviveClientError::Timeout) => "timeout",
        CodexReviveError::IdentityMismatch => "identity_mismatch",
        CodexReviveError::NoRecoveredAccount => "not_recovered",
        _ => "failed",
    }
}
