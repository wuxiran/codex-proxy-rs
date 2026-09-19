//! 账号级 state 自动续期的参数，存放在运行数据目录而不是凭据里。
//!
//! 凭据 schema 是 `deny_unknown_fields`：往里加字段会让旧版本读不了该账号，回滚或
//! 发版排空期间的旧实例就此失去这个账号。续期参数不含任何敏感信息，放在各实例共享的
//! 数据卷上即可；旧版本看不到这个文件，回滚不受影响。

use std::collections::BTreeMap;
use std::fs;
use std::io::{ErrorKind, Write as _};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

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
    dir: Option<PathBuf>,
}

impl AutoHuntStore {
    pub(crate) fn new(dir: PathBuf) -> Result<Self, AutoHuntStoreError> {
        fs::create_dir_all(&dir).map_err(|_| AutoHuntStoreError)?;
        Ok(Self { dir: Some(dir) })
    }

    /// 文件不存在是「还没有任何账号开启续期」；读不出来或内容损坏是错误。
    /// 两者必须分开：把损坏当成空表，下一次保存就会抹掉其它账号的全部设置。
    pub(crate) fn all(&self) -> Result<BTreeMap<String, TurnStateAutoHunt>, AutoHuntStoreError> {
        let Some(dir) = &self.dir else {
            return Ok(BTreeMap::new());
        };
        match fs::read(dir.join("auto_hunt.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| AutoHuntStoreError),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(_) => Err(AutoHuntStoreError),
        }
    }

    pub(crate) fn get(&self, account_id: &str) -> Option<TurnStateAutoHunt> {
        self.all().ok()?.remove(account_id)
    }

    /// `None` 删除该账号的续期参数。
    pub(crate) fn set(
        &self,
        account_id: &str,
        setting: Option<TurnStateAutoHunt>,
    ) -> Result<(), AutoHuntStoreError> {
        let dir = self.dir.as_ref().ok_or(AutoHuntStoreError)?;
        // 各实例共享这个目录：整个「读-改-写」要在跨进程锁里完成，否则两个实例各改一个
        // 账号时后写的会丢掉先写的（也可能把刚关掉的续期带回来）。锁随文件句柄释放。
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("auto_hunt.lock"))
            .map_err(|_| AutoHuntStoreError)?;
        lock.lock().map_err(|_| AutoHuntStoreError)?;
        let mut all = self.all()?;
        let changed = match setting {
            Some(setting) => all.insert(account_id.to_owned(), setting.clone()) != Some(setting),
            None => all.remove(account_id).is_some(),
        };
        if !changed {
            return Ok(());
        }
        let bytes = serde_json::to_vec_pretty(&all).map_err(|_| AutoHuntStoreError)?;
        // 临时文件名各不相同：固定名字会让两个写者截断、改写同一个 inode。
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let tmp = dir.join(format!("auto_hunt.{}.{nonce}.tmp", std::process::id()));
        let written = (|| {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&tmp, dir.join("auto_hunt.json"))
        })();
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written.map_err(|_| AutoHuntStoreError)
    }
}
