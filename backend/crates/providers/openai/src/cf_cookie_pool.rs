//! 按出口共享的 Cloudflare `__cf_bm` cookie 池。
//!
//! `__cf_bm` 是 Cloudflare 按出口 IP/边缘下发的机器人管理 cookie，与账号无关：
//! 同一出口下不同账号/模型可复用同一张。为避免低流量账号自己没有新鲜 `__cf_bm`
//! 而被 Cloudflare 挑战，这里把每次响应里采到的 `__cf_bm` 按出口指纹存入共享池，
//! 账号自己没有未过期的 `__cf_bm` 时从池里借一张注入。
//!
//! 只池化 CF 族 cookie，绝不碰账号私有的 auth-session cookie。值用 `SecretString`
//! 承载，`Debug` 脱敏，绝不落日志。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use secrecy::SecretString;

/// 从池里借出的 cookie 明细；调用方（selector）据此构造可回放的运行时 cookie。
#[derive(Clone)]
pub(crate) struct BorrowedCf {
    pub(crate) name: String,
    pub(crate) value: SecretString,
    pub(crate) domain: String,
    pub(crate) path: String,
    pub(crate) host_only: bool,
    pub(crate) secure: bool,
    pub(crate) expires_at: Option<DateTime<Utc>>,
}

/// 池化的 cookie 名（仅 Cloudflare 出口相关、与账号无关的）。
pub(crate) const POOLED_COOKIE_NAME: &str = "__cf_bm";
/// 最多缓存多少个出口的 cookie，防止无界增长。
const MAX_EGRESSES: usize = 512;

#[derive(Clone)]
struct PooledCf {
    value: SecretString,
    domain: String,
    path: String,
    host_only: bool,
    secure: bool,
    captured_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
}

impl PooledCf {
    fn active(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_none_or(|expires| expires > now)
    }
}

/// 出口指纹 -> 该出口最新的 `__cf_bm`。进程内共享，不持久化。
#[derive(Clone, Default)]
pub(crate) struct CfCookiePool(Arc<Mutex<BTreeMap<String, PooledCf>>>);

impl CfCookiePool {
    /// 采集：把某出口上刚拿到的 `__cf_bm` 存入池（覆盖旧值）。非 CF 名一律忽略。
    #[expect(clippy::too_many_arguments)]
    pub(crate) fn harvest(
        &self,
        egress_fingerprint: &str,
        name: &str,
        value: SecretString,
        domain: String,
        path: String,
        host_only: bool,
        secure: bool,
        expires_at: Option<DateTime<Utc>>,
    ) {
        if name != POOLED_COOKIE_NAME {
            return;
        }
        let now = Utc::now();
        let Ok(mut pool) = self.0.lock() else {
            return;
        };
        pool.retain(|_, cf| cf.active(now));
        if pool.len() >= MAX_EGRESSES && !pool.contains_key(egress_fingerprint) {
            // 满了且是新出口：淘汰最旧的一条。
            if let Some(oldest) = pool
                .iter()
                .min_by_key(|(_, cf)| cf.captured_at)
                .map(|(k, _)| k.clone())
            {
                pool.remove(&oldest);
            }
        }
        pool.insert(
            egress_fingerprint.to_owned(),
            PooledCf {
                value,
                domain,
                path,
                host_only,
                secure,
                captured_at: now,
                expires_at,
            },
        );
    }

    /// 注入：取某出口上未过期的 `__cf_bm` 明细。
    pub(crate) fn borrow(&self, egress_fingerprint: &str) -> Option<BorrowedCf> {
        let now = Utc::now();
        let pool = self.0.lock().ok()?;
        let cf = pool.get(egress_fingerprint).filter(|cf| cf.active(now))?;
        Some(BorrowedCf {
            name: POOLED_COOKIE_NAME.to_owned(),
            value: cf.value.clone(),
            domain: cf.domain.clone(),
            path: cf.path.clone(),
            host_only: cf.host_only,
            secure: cf.secure,
            expires_at: cf.expires_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret as _;

    fn harvest(
        pool: &CfCookiePool,
        egress: &str,
        name: &str,
        value: &str,
        expires: Option<DateTime<Utc>>,
    ) {
        pool.harvest(
            egress,
            name,
            SecretString::from(value.to_owned()),
            "chatgpt.com".to_owned(),
            "/".to_owned(),
            true,
            true,
            expires,
        );
    }

    #[test]
    fn pools_cf_bm_per_egress_and_ignores_other_names() {
        let pool = CfCookiePool::default();
        harvest(&pool, "egr-a", "__cf_bm", "cf-a", None);
        harvest(
            &pool,
            "egr-a",
            "__Secure-next-auth.session-token",
            "auth",
            None,
        );
        let got = pool.borrow("egr-a").unwrap();
        assert_eq!(got.name, "__cf_bm");
        assert_eq!(got.value.expose_secret(), "cf-a");
        // 非同出口拿不到。
        assert!(pool.borrow("egr-b").is_none());
    }

    #[test]
    fn expired_cookie_is_not_borrowable() {
        let pool = CfCookiePool::default();
        harvest(
            &pool,
            "egr",
            "__cf_bm",
            "old",
            Some(Utc::now() - chrono::Duration::seconds(5)),
        );
        assert!(pool.borrow("egr").is_none());
    }

    #[test]
    fn latest_harvest_wins() {
        let pool = CfCookiePool::default();
        harvest(&pool, "egr", "__cf_bm", "v1", None);
        harvest(&pool, "egr", "__cf_bm", "v2", None);
        assert_eq!(pool.borrow("egr").unwrap().value.expose_secret(), "v2");
    }
}
