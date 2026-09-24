//! 票据内嵌签发时间：`X-Codex-Turn-State` 观测为 Fernet 令牌（`0x80` 版本字节 +
//! 8 字节大端 Unix 秒 + IV + 密文 + HMAC，base64url 编码）。不验签、不解密，只读时间戳。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// 签发时间的来源；面板据此告诉运维「到期是算出来的还是猜的」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssuedAtSource {
    /// 从票据内嵌时间戳读出。
    Fernet,
    /// 票据不是可解析的 Fernet 令牌，退回到捕获时刻。
    #[default]
    Captured,
}

/// 票据被判定为未来签发；上游不会接受这样的值，宿主不应保存它。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("turn state is stamped in the future")]
pub struct FutureStamped;

/// 已解析出的签发时间及其来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedIssuedAt {
    pub at: SystemTime,
    pub source: IssuedAtSource,
}

/// 读取票据内嵌的签发时间；不是 Fernet 形状时返回 `None`。
pub fn issued_at(token: &str) -> Option<SystemTime> {
    let trimmed = token.trim_end_matches('=');
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(trimmed)
        .ok()?;
    if raw.len() < 9 || raw[0] != 0x80 {
        return None;
    }
    let secs = u64::from_be_bytes(raw[1..9].try_into().ok()?);
    UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

/// 决定一张票的签发时间：优先内嵌时间戳，解析不了退回捕获时刻；
/// 内嵌时间戳超过 `now + skew` 视为伪造或时钟错乱，拒绝。
pub fn resolve_issued_at(
    token: &str,
    captured_at: SystemTime,
    now: SystemTime,
    skew: Duration,
) -> Result<ResolvedIssuedAt, FutureStamped> {
    match issued_at(token) {
        Some(at) => {
            if at > now + skew {
                return Err(FutureStamped);
            }
            Ok(ResolvedIssuedAt {
                at,
                source: IssuedAtSource::Fernet,
            })
        }
        None => Ok(ResolvedIssuedAt {
            at: captured_at,
            source: IssuedAtSource::Captured,
        }),
    }
}
