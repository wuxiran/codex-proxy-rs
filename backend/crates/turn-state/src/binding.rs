//! 凭据绑定与出口指纹：两者都是不可逆摘要，用于给 pin 定作用域而不保留原文。

use sha2::{Digest as _, Sha256};

/// 凭据刷新或管理员重新捕获后，旧 state 不再具备复用资格；Cookie 更新不改变绑定。
pub fn credential_binding(generation: &str, access_token: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(generation.as_bytes());
    digest.update([0]);
    digest.update(access_token.as_bytes());
    hex::encode(digest.finalize())
}

/// 出口的稳定指纹：代理地址含凭据，不在更多地方保留原文。
pub fn egress_fingerprint(proxy_url: Option<&str>) -> String {
    hex::encode(Sha256::digest(proxy_url.unwrap_or("direct").as_bytes()))
}
