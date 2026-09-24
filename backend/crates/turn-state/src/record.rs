//! 一个桶的模板记录：既是内存里的 pin，也是 `<dir>/buckets/<账号>/<模型>.json` 的内容。

use std::{
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::fernet::IssuedAtSource;

/// 模板是怎么来的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// 管理员遍历代理命中后钉住。
    Hunt,
    /// 业务流量的上游响应被动捕获（客户端级）。
    Passive,
    /// 到期前自动续期钉住。
    Renewal,
    /// 云端打票（relay 铸票）钉住。
    Mint,
}

impl Source {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hunt => "hunt",
            Self::Passive => "passive",
            Self::Renewal => "renewal",
            Self::Mint => "mint",
        }
    }
}

/// 毫秒时间戳的 serde 表示；内存里保持 `SystemTime` 精度，磁盘上用整数。
mod millis {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(value: &SystemTime, serializer: S) -> Result<S::Ok, S::Error> {
        let millis = value
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        millis.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SystemTime, D::Error> {
        let millis = u64::deserialize(deserializer)?;
        UNIX_EPOCH
            .checked_add(Duration::from_millis(millis))
            .ok_or_else(|| serde::de::Error::custom("timestamp out of range"))
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BucketRecord {
    pub account: String,
    pub model: String,
    /// 票据原值；只在这个字段里存在，`Debug` 不展开。
    pub value: String,
    pub len: usize,
    #[serde(with = "millis")]
    pub issued_at: SystemTime,
    #[serde(default)]
    pub issued_at_source: IssuedAtSource,
    #[serde(with = "millis")]
    pub captured_at: SystemTime,
    #[serde(with = "millis")]
    pub expires_at: SystemTime,
    /// 凭据绑定摘要；令牌刷新后不再匹配。
    pub binding: String,
    /// 账号级模板只对观测到它的出口生效；客户端级模板恒为 `None`。
    pub egress: Option<String>,
    /// `None` = 账号级，对该账号该模型的全部客户端生效。
    pub client: Option<String>,
    pub source: Source,
    #[serde(default)]
    pub hits: u64,
    /// 票所属的网关节点名（`unified-N`，从路由 cookie 对解出）；只有云端打票的票带。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
}

impl BucketRecord {
    pub fn active(&self, now: SystemTime) -> bool {
        now < self.expires_at
    }

    pub fn account_wide(&self) -> bool {
        self.client.is_none()
    }

    /// 文件内容必须与它所在的路径一致；不一致就是跨桶污染，读取方应丢弃。
    pub fn matches_path(&self, account: &str, model: &str) -> bool {
        self.account == account && self.model == model
    }

    pub fn consistent(&self) -> bool {
        self.len == self.value.len() && crate::classify::printable_ascii(&self.value)
    }

    pub fn remaining(&self, now: SystemTime) -> Duration {
        self.expires_at
            .duration_since(now)
            .unwrap_or(Duration::ZERO)
    }
}

impl fmt::Debug for BucketRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BucketRecord")
            .field("account", &self.account)
            .field("model", &self.model)
            .field("value", &"<redacted>")
            .field("len", &self.len)
            .field("issued_at", &self.issued_at)
            .field("expires_at", &self.expires_at)
            .field("client", &self.client.as_ref().map(|_| "<set>"))
            .field("source", &self.source)
            .field("hits", &self.hits)
            .finish_non_exhaustive()
    }
}

pub fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
