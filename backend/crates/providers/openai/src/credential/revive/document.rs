//! 保留观澜签名原件；仅在签名清单里的哈希完全匹配时修复旧 JSON 数字表示。

use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use super::detect::document_accounts;
use super::store::ReviveStoreError;

pub(crate) const RAW_EXPORT_KEY: &str = "guanlan_signed_export";
const MAX_EXPORT_BYTES: usize = 20 * 1024 * 1024;

pub(crate) fn unpack(payload: Value) -> Result<(Value, Option<Vec<u8>>), ReviveStoreError> {
    let Some(raw) = payload.get(RAW_EXPORT_KEY) else {
        return Ok((payload, None));
    };
    let raw = raw.as_str().ok_or(ReviveStoreError::InvalidDocument)?;
    if raw.len() > MAX_EXPORT_BYTES || payload.as_object().is_none_or(|o| o.len() != 1) {
        return Err(ReviveStoreError::InvalidDocument);
    }
    let data = serde_json::from_str(raw).map_err(|_| ReviveStoreError::InvalidDocument)?;
    let (_, validated) = validated_bytes(&data)?;
    // 已经有效的原件保持原始字节；旧前端传来的原件仅允许通过签名哈希恢复。
    let bytes = if validate(&data).is_ok() {
        raw.as_bytes().to_vec()
    } else {
        validated
    };
    let data = serde_json::from_slice(&bytes).map_err(|_| ReviveStoreError::InvalidDocument)?;
    Ok((data, Some(bytes)))
}

pub(crate) fn validated_bytes(payload: &Value) -> Result<(Value, Vec<u8>), ReviveStoreError> {
    let mut data = payload.clone();
    if validate(&data).is_err() {
        // JS 的 JSON.parse/stringify 会将 1.0 变成 1。恢复后仍必须匹配原签名
        // 清单，绝不重签或按猜测接受其他字段变动。
        let records = data
            .get("x_revive_manifest")
            .and_then(|m| m.get("records"))
            .and_then(Value::as_array)
            .cloned()
            .ok_or(ReviveStoreError::InvalidDocument)?;
        let accounts = data
            .get_mut("accounts")
            .and_then(Value::as_array_mut)
            .ok_or(ReviveStoreError::InvalidDocument)?;
        for (index, account) in accounts.iter_mut().enumerate() {
            let expected = records
                .iter()
                .find(|r| r.get("index").and_then(Value::as_u64) == Some(index as u64))
                .and_then(|r| r.get("payload_sha256"))
                .and_then(Value::as_str);
            let current = serde_json::to_vec(&sorted(account))
                .map_err(|_| ReviveStoreError::InvalidDocument)?;
            if expected == Some(hex::encode(Sha256::digest(current)).as_str()) {
                continue;
            }
            if let Some(value) = account.get_mut("rate_multiplier")
                && let Some(number) = value.as_i64()
            {
                *value = serde_json::from_str(&format!("{number}.0"))
                    .map_err(|_| ReviveStoreError::InvalidDocument)?;
            }
        }
        validate(&data)?;
    }
    let bytes = serde_json::to_vec(&data).map_err(|_| ReviveStoreError::InvalidDocument)?;
    Ok((data, bytes))
}

fn validate(data: &Value) -> Result<(), ReviveStoreError> {
    let manifest = data
        .get("x_revive_manifest")
        .ok_or(ReviveStoreError::InvalidDocument)?;
    if manifest.get("version").and_then(Value::as_u64) != Some(1)
        || manifest
            .get("signature")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        || !manifest
            .get("scope")
            .and_then(Value::as_array)
            .is_some_and(|items| items.iter().any(|v| v == "reauth"))
    {
        return Err(ReviveStoreError::InvalidDocument);
    }
    let accounts = document_accounts(data);
    let records = manifest
        .get("records")
        .and_then(Value::as_array)
        .ok_or(ReviveStoreError::InvalidDocument)?;
    if accounts.is_empty() || records.len() != accounts.len() {
        return Err(ReviveStoreError::InvalidDocument);
    }
    for (index, account) in accounts.iter().enumerate() {
        let matches: Vec<_> = records
            .iter()
            .filter(|r| r.get("index").and_then(Value::as_u64) == Some(index as u64))
            .collect();
        if matches.len() != 1 {
            return Err(ReviveStoreError::InvalidDocument);
        }
        let bytes =
            serde_json::to_vec(&sorted(account)).map_err(|_| ReviveStoreError::InvalidDocument)?;
        let digest = hex::encode(Sha256::digest(&bytes));
        if matches[0].get("payload_sha256").and_then(Value::as_str) != Some(digest.as_str()) {
            return Err(ReviveStoreError::InvalidDocument);
        }
    }
    Ok(())
}

fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort();
            let mut result = Map::new();
            for key in keys {
                result.insert(key.clone(), sorted(&object[key]));
            }
            Value::Object(result)
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        value => value.clone(),
    }
}
