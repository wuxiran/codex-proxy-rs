//! 业务请求实际用的路由 cookie 对（`__cflb` / `__oailb`）：记下它指向的节点标签，
//! 以及换模型后按指纹条件删除它。
//!
//! 节点标签取自 cookie 里的 `unified-N`，是上游路由凭据自己的声明，不是对实际执行节点的
//! 独立验证。指纹是两枚 cookie 值的摘要，只用来判断「凭据里现存的还是不是我用过的那一对」。

use chrono::{DateTime, Utc};
use gateway_core::account::ProviderAccount;
use secrecy::ExposeSecret as _;
use sha2::{Digest as _, Sha256};

use crate::credential::{
    CodexCookie, CodexCredentialRepository, CredentialRepositoryError, RuntimeCodexCookie,
};
use crate::turn_state_mint::{gateway_label, jwt_claims};

const CFLB: &str = "__cflb";
const OAILB: &str = "__oailb";

/// 一次请求用到或上游新发的一对路由 cookie；不含原值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoutePairRef {
    pub(crate) fingerprint: String,
    pub(crate) gateway: Option<String>,
}

impl RoutePairRef {
    pub(crate) fn of(cflb: &str, oailb: &str) -> Option<Self> {
        if cflb.is_empty() || oailb.is_empty() {
            return None;
        }
        let mut digest = Sha256::new();
        digest.update(cflb.as_bytes());
        digest.update([0]);
        digest.update(oailb.as_bytes());
        Some(Self {
            fingerprint: hex::encode(digest.finalize()),
            gateway: gateway_label(cflb, oailb),
        })
    }

    /// 请求带出去的那一对；缺任何一枚都不算一对。
    pub(crate) fn sent(cookies: &[RuntimeCodexCookie]) -> Option<Self> {
        let value = |name: &str| {
            cookies
                .iter()
                .find(|cookie| cookie.name == name)
                .map(|cookie| cookie.value.expose_secret())
        };
        Self::of(value(CFLB)?, value(OAILB)?)
    }

    /// WebSocket 握手实际用的一对：握手响应新发的优先，否则是握手请求 Cookie 头里带的。
    pub(crate) fn handshake(
        request_headers: &[(String, String)],
        set_cookie_headers: &[String],
    ) -> Option<Self> {
        Self::issued(set_cookie_headers).or_else(|| {
            let cookie = request_headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("cookie"))
                .map(|(_, value)| value.as_str())?;
            let value = |name: &str| {
                cookie.split(';').find_map(|part| {
                    let (key, value) = part.trim().split_once('=')?;
                    (key == name).then_some(value)
                })
            };
            Self::of(value(CFLB)?, value(OAILB)?)
        })
    }

    /// 上游在这次响应里新发的一对。
    pub(crate) fn issued(set_cookie_headers: &[String]) -> Option<Self> {
        let value = |name: &str| {
            set_cookie_headers.iter().find_map(|header| {
                let (key, rest) = header.split_once('=')?;
                key.trim()
                    .eq_ignore_ascii_case(name)
                    .then(|| rest.split(';').next().unwrap_or("").trim())
            })
        };
        Self::of(value(CFLB)?, value(OAILB)?)
    }

    pub(crate) fn stored(cookies: &[CodexCookie]) -> Option<Self> {
        let value = |name: &str| {
            cookies
                .iter()
                .find(|cookie| cookie.name == name)
                .map(|cookie| cookie.value.as_str())
        };
        Self::of(value(CFLB)?, value(OAILB)?)
    }
}

/// 被动捕获的 cookie 存多久：`__oailb` 是 JWT，以它自己的 `exp` 为准（网关按这个执行，
/// 比 Set-Cookie 声明的更长）；读不出 `exp` 或别的 cookie 沿用 Set-Cookie 的声明。
/// 撤销（空值或已过期的声明）由调用方先行处理，不走这里。
pub(crate) fn captured_expiry(
    name: &str,
    value: &str,
    declared: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    if name != OAILB {
        return declared;
    }
    jwt_claims(value)
        .and_then(|claims| claims.get("exp")?.as_i64())
        .and_then(|exp| DateTime::<Utc>::from_timestamp(exp, 0))
        .or(declared)
}

/// 凭据里现存的 pair 仍是 `fingerprint` 那一对时删除它，返回是否删了。
/// 别的请求或打票已经换上新 pair 时不动；写入冲突后重读一次并重新比对。
pub(crate) async fn drop_if_current(
    repository: &CodexCredentialRepository,
    account: &ProviderAccount,
    fingerprint: &str,
) -> bool {
    // 调用方手里的账号可能已经落后于凭据版本：每次都按最新版本读。
    for attempt in 0..2 {
        let Ok(Some(current)) = repository.store().get_account(account.id()).await else {
            return false;
        };
        let Ok(mut data) = repository.load_complete_data(&current).await else {
            return false;
        };
        let Some(cookies) = data.cookies_mut() else {
            return false;
        };
        if RoutePairRef::stored(cookies).is_none_or(|pair| pair.fingerprint != fingerprint) {
            return false;
        }
        cookies.retain(|cookie| cookie.name != CFLB && cookie.name != OAILB);
        match repository.compare_and_swap_data(&current, data).await {
            Ok(_) => return true,
            Err(CredentialRepositoryError::RevisionConflict) if attempt == 0 => {}
            Err(_) => return false,
        }
    }
    false
}
