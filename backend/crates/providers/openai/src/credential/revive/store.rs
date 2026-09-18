//! 已签名号池原文落在 runtime 数据目录，按内容哈希索引上游用户。

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::detect::{document_accounts, document_user_ids, looks_signed, recovered_oauth_tokens};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReviveStoreError {
    #[error("revive export directory is unavailable")]
    Io,
    #[error("revive export index is invalid")]
    InvalidIndex,
    #[error("signed export integrity check failed; import the original Guanlan file again")]
    InvalidDocument,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct ReviveIndex {
    #[serde(default)]
    users: BTreeMap<String, String>,
    #[serde(default)]
    identities: BTreeMap<String, String>,
}

pub struct ReviveExportStore {
    root: PathBuf,
    index_lock: Mutex<()>,
}

impl std::fmt::Debug for ReviveExportStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReviveExportStore")
            .field("root", &self.root)
            .finish()
    }
}

impl ReviveExportStore {
    pub fn new(root: PathBuf) -> Result<Self, ReviveStoreError> {
        fs::create_dir_all(root.join("exports")).map_err(|_| ReviveStoreError::Io)?;
        fs::create_dir_all(root.join("state")).map_err(|_| ReviveStoreError::Io)?;
        Ok(Self {
            root,
            index_lock: Mutex::new(()),
        })
    }

    /// 仅归档带签名的原文。无签名时静默跳过，不改变索引。
    pub fn record_signed_document(
        &self,
        payload: &Value,
    ) -> Result<Option<String>, ReviveStoreError> {
        if !looks_signed(payload) {
            return Ok(None);
        }
        let (_, bytes) = super::document::validated_bytes(payload)?;
        self.record_raw_document(&bytes)
    }

    pub(crate) fn record_raw_document(
        &self,
        bytes: &[u8],
    ) -> Result<Option<String>, ReviveStoreError> {
        let _guard = self.index_lock.lock().map_err(|_| ReviveStoreError::Io)?;
        let payload: Value =
            serde_json::from_slice(bytes).map_err(|_| ReviveStoreError::InvalidDocument)?;
        super::document::validated_bytes(&payload)?;
        let digest = hex::encode(Sha256::digest(bytes));
        let export_path = self.export_path(&digest);
        if !export_path.exists() {
            atomic_write(&export_path, bytes)?;
        }
        let user_ids = document_user_ids(&payload);
        if !user_ids.is_empty() {
            let mut index = self.load_index()?;
            for user_id in user_ids {
                index.users.insert(user_id, digest.clone());
            }
            for tokens in document_accounts(&payload)
                .iter()
                .filter_map(recovered_oauth_tokens)
            {
                index.identities.insert(
                    identity_key(&tokens.user_id, tokens.workspace_id.as_deref()),
                    digest.clone(),
                );
            }
            self.save_index(&index)?;
        }
        Ok(Some(digest))
    }

    pub fn export_for_user(
        &self,
        user_id: &str,
        workspace_id: Option<&str>,
    ) -> Result<Option<(String, Vec<u8>)>, ReviveStoreError> {
        let index = self.load_index()?;
        let Some(digest) = index
            .identities
            .get(&identity_key(user_id, workspace_id))
            .or_else(|| index.users.get(user_id))
        else {
            return Ok(None);
        };
        let bytes = fs::read(self.export_path(digest)).map_err(|_| ReviveStoreError::Io)?;
        let document: Value =
            serde_json::from_slice(&bytes).map_err(|_| ReviveStoreError::InvalidDocument)?;
        let (_, validated) = super::document::validated_bytes(&document)?;
        Ok(Some((digest.clone(), validated)))
    }

    pub fn state_path(&self, digest: &str) -> PathBuf {
        self.root.join("state").join(format!("{digest}.json"))
    }

    fn export_path(&self, digest: &str) -> PathBuf {
        self.root.join("exports").join(format!("{digest}.json"))
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("index.json")
    }

    fn load_index(&self) -> Result<ReviveIndex, ReviveStoreError> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(ReviveIndex::default());
        }
        let bytes = fs::read(path).map_err(|_| ReviveStoreError::Io)?;
        serde_json::from_slice(&bytes).map_err(|_| ReviveStoreError::InvalidIndex)
    }

    fn save_index(&self, index: &ReviveIndex) -> Result<(), ReviveStoreError> {
        let bytes = serde_json::to_vec_pretty(index).map_err(|_| ReviveStoreError::InvalidIndex)?;
        atomic_write(&self.index_path(), &bytes)
    }
}

pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ReviveStoreError> {
    let tmp = path.with_extension("tmp");
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp).map_err(|_| ReviveStoreError::Io)?;
        file.write_all(bytes).map_err(|_| ReviveStoreError::Io)?;
        file.sync_all().map_err(|_| ReviveStoreError::Io)?;
    }
    fs::rename(tmp, path).map_err(|_| ReviveStoreError::Io)
}

fn identity_key(user: &str, workspace: Option<&str>) -> String {
    let mut hash = Sha256::new();
    hash.update(user.as_bytes());
    hash.update([0]);
    hash.update(workspace.unwrap_or_default().as_bytes());
    hex::encode(hash.finalize())
}
