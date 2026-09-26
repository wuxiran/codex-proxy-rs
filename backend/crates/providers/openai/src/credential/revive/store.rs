//! 已签名号池原文落在 runtime 数据目录，按内容哈希索引上游用户。

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::detect::{document_user_ids, looks_signed};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReviveStoreError {
    #[error("revive export directory is unavailable")]
    Io,
    #[error("revive export index is invalid")]
    InvalidIndex,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct ReviveIndex {
    #[serde(default)]
    users: BTreeMap<String, String>,
}

pub struct ReviveExportStore {
    root: PathBuf,
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
        Ok(Self { root })
    }

    /// 仅归档带签名的原文。无签名时静默跳过，不改变索引。
    pub fn record_signed_document(
        &self,
        payload: &Value,
    ) -> Result<Option<String>, ReviveStoreError> {
        if !looks_signed(payload) {
            return Ok(None);
        }
        let bytes = serde_json::to_vec(payload).map_err(|_| ReviveStoreError::InvalidIndex)?;
        let digest = hex::encode(Sha256::digest(&bytes));
        let export_path = self.export_path(&digest);
        if !export_path.exists() {
            atomic_write(&export_path, &bytes)?;
        }
        let user_ids = document_user_ids(payload);
        if !user_ids.is_empty() {
            let mut index = self.load_index()?;
            for user_id in user_ids {
                index.users.insert(user_id, digest.clone());
            }
            self.save_index(&index)?;
        }
        Ok(Some(digest))
    }

    pub fn export_for_user(
        &self,
        user_id: &str,
    ) -> Result<Option<(String, Vec<u8>)>, ReviveStoreError> {
        let index = self.load_index()?;
        let Some(digest) = index.users.get(user_id) else {
            return Ok(None);
        };
        let bytes = fs::read(self.export_path(digest)).map_err(|_| ReviveStoreError::Io)?;
        Ok(Some((digest.clone(), bytes)))
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

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ReviveStoreError> {
    let tmp = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&tmp).map_err(|_| ReviveStoreError::Io)?;
        file.write_all(bytes).map_err(|_| ReviveStoreError::Io)?;
        file.sync_all().map_err(|_| ReviveStoreError::Io)?;
    }
    fs::rename(tmp, path).map_err(|error| {
        if error.kind() == io::ErrorKind::PermissionDenied {
            ReviveStoreError::Io
        } else {
            ReviveStoreError::Io
        }
    })
}
