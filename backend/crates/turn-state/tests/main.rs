mod binding;
mod classify;
mod decision;
mod fernet;
mod fs_util;
mod observe;
mod record;
mod service;
mod settings;
mod store;

/// 一张伪造的 Fernet 形状票据：`0x80` + 大端 Unix 秒 + 填充到指定 base64 长度。
pub(crate) fn fernet_token(issued_secs: u64, len: usize) -> String {
    use base64::Engine as _;
    let mut raw = vec![0x80u8];
    raw.extend_from_slice(&issued_secs.to_be_bytes());
    // base64url 无填充：每 3 字节 4 字符；补足到目标长度。
    let target_bytes = len.div_ceil(4) * 3;
    while raw.len() < target_bytes {
        raw.push(0x41);
    }
    let mut token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&raw);
    token.truncate(len);
    token
}

pub(crate) fn plain_token(len: usize) -> String {
    "a".repeat(len)
}
