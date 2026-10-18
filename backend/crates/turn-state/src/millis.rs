//! 毫秒时间戳的 serde 表示；内存里保持 `SystemTime` 精度，磁盘上用整数。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub(crate) fn serialize<S: Serializer>(
    value: &SystemTime,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let millis = value
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    millis.serialize(serializer)
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<SystemTime, D::Error> {
    let millis = u64::deserialize(deserializer)?;
    UNIX_EPOCH
        .checked_add(Duration::from_millis(millis))
        .ok_or_else(|| serde::de::Error::custom("timestamp out of range"))
}
