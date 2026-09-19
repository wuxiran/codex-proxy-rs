//! 账号级 state 自动续期的参数，存放在运行数据目录而不是凭据里。
//!
//! 凭据 schema 是 `deny_unknown_fields`：往里加字段会让旧版本读不了该账号，回滚或
//! 发版排空期间的旧实例就此失去这个账号。续期参数不含任何敏感信息，放在各实例共享的
//! 数据卷上即可；旧版本看不到这个文件，回滚不受影响。

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TurnStateAutoHunt {
    pub(crate) model: String,
    pub(crate) attempts: u8,
    #[serde(default)]
    pub(crate) include_direct: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("turn state auto hunt settings are unavailable")]
pub(crate) struct AutoHuntStoreError;

/// 每次读盘、整文件原子替换：条目只有几十个，换来多实例之间无需同步即一致。
#[derive(Debug, Clone, Default)]
pub(crate) struct AutoHuntStore {
    path: Option<PathBuf>,
}

impl AutoHuntStore {
    pub(crate) fn new(dir: PathBuf) -> Result<Self, AutoHuntStoreError> {
        fs::create_dir_all(&dir).map_err(|_| AutoHuntStoreError)?;
        Ok(Self {
            path: Some(dir.join("auto_hunt.json")),
        })
    }

    pub(crate) fn all(&self) -> BTreeMap<String, TurnStateAutoHunt> {
        self.path
            .as_ref()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub(crate) fn get(&self, account_id: &str) -> Option<TurnStateAutoHunt> {
        self.all().remove(account_id)
    }

    /// `None` 删除该账号的续期参数。
    pub(crate) fn set(
        &self,
        account_id: &str,
        setting: Option<TurnStateAutoHunt>,
    ) -> Result<(), AutoHuntStoreError> {
        let path = self.path.as_ref().ok_or(AutoHuntStoreError)?;
        let mut all = self.all();
        let changed = match setting {
            Some(setting) => all.insert(account_id.to_owned(), setting.clone()) != Some(setting),
            None => all.remove(account_id).is_some(),
        };
        if !changed {
            return Ok(());
        }
        let bytes = serde_json::to_vec_pretty(&all).map_err(|_| AutoHuntStoreError)?;
        let tmp = path.with_extension("tmp");
        {
            let mut file = fs::File::create(&tmp).map_err(|_| AutoHuntStoreError)?;
            file.write_all(&bytes).map_err(|_| AutoHuntStoreError)?;
            file.sync_all().map_err(|_| AutoHuntStoreError)?;
        }
        fs::rename(tmp, path).map_err(|_| AutoHuntStoreError)
    }
}
