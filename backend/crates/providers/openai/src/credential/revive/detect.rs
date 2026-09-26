//! 识别已签名号池文档，并从中投影用于对账的上游用户身份。

use serde_json::Value;

use super::super::types::parse_chatgpt_jwt_claims;

const SIGNATURE_KEYS: [&str; 5] = [
    "signature",
    "sign",
    "sig",
    "file_signature",
    "server_signature",
];

/// 文档是否带有非空签名字段。缺少签名的 CPR 原生导出不能提交 revive-api。
#[must_use]
pub fn looks_signed(value: &Value) -> bool {
    signature_text(value).is_some()
}

fn signature_text(value: &Value) -> Option<&str> {
    for key in SIGNATURE_KEYS {
        if let Some(text) = nonempty_str(value.get(key)) {
            return Some(text);
        }
        if let Some(text) = value
            .get("data")
            .and_then(|data| nonempty_str(data.get(key)))
        {
            return Some(text);
        }
        if let Some(text) = value
            .get("x_revive_manifest")
            .and_then(|manifest| nonempty_str(manifest.get(key)))
        {
            return Some(text);
        }
    }
    None
}

fn nonempty_str(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// 从签名文档的账号列表提取 chatgpt_user_id。解析失败的条目跳过。
#[must_use]
pub fn document_user_ids(value: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    for account in document_accounts(value) {
        if let Some(user_id) = account_user_id(account)
            && !ids.iter().any(|existing| existing == &user_id)
        {
            ids.push(user_id);
        }
    }
    ids
}

#[must_use]
pub fn document_accounts(value: &Value) -> &[Value] {
    value
        .get("data")
        .and_then(|data| data.get("accounts"))
        .or_else(|| value.get("accounts"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn account_user_id(account: &Value) -> Option<String> {
    let credentials = account.get("credentials").unwrap_or(account);
    let token = nonempty_str(credentials.get("access_token"))
        .or_else(|| nonempty_str(credentials.get("accessToken")))?;
    parse_chatgpt_jwt_claims(token)
        .ok()
        .and_then(|metadata| metadata.chatgpt_user_id)
}

/// 从恢复结果条目读取 OAuth token。字段缺失则跳过该账号。
#[must_use]
pub fn recovered_oauth_tokens(account: &Value) -> Option<RecoveredOAuthTokens> {
    let credentials = account.get("credentials").unwrap_or(account);
    let access_token = nonempty_str(credentials.get("access_token"))
        .or_else(|| nonempty_str(credentials.get("accessToken")))?
        .to_owned();
    let user_id = parse_chatgpt_jwt_claims(&access_token)
        .ok()
        .and_then(|metadata| metadata.chatgpt_user_id)?;
    Some(RecoveredOAuthTokens {
        user_id,
        access_token,
        refresh_token: nonempty_str(credentials.get("refresh_token"))
            .or_else(|| nonempty_str(credentials.get("refreshToken")))
            .map(str::to_owned),
        id_token: nonempty_str(credentials.get("id_token"))
            .or_else(|| nonempty_str(credentials.get("idToken")))
            .map(str::to_owned),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredOAuthTokens {
    pub user_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
}
