//! 提交观澜前，把被浏览器改写过的 JSON 数字还原成签名时的样子。
//!
//! CDK 兑换经浏览器 `JSON.parse`/`stringify` 往返，`1.0` 会变成 `1`；而签名清单里的
//! `payload_sha256` 是按原文算的，原样提交会被 revive-api 直接拒绝。这里只做一件事：
//! 某个账号的哈希对不上、且把 `rate_multiplier` 还原成浮点后恰好对上时才改写它。
//! 对不上的文档原样返回，绝不按猜测改动其它字段，也不重签。

use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

/// 返回可提交的字节与被还原的账号数。
pub(super) fn restore_signed_numbers(bytes: &[u8]) -> (Vec<u8>, usize) {
    let Ok(mut document) = serde_json::from_slice::<Value>(bytes) else {
        return (bytes.to_vec(), 0);
    };
    let Some(records) = document
        .get("x_revive_manifest")
        .and_then(|manifest| manifest.get("records"))
        .and_then(Value::as_array)
        .cloned()
    else {
        return (bytes.to_vec(), 0);
    };
    let nested = document
        .get("data")
        .is_some_and(|data| data.get("accounts").is_some());
    let accounts = if nested {
        document
            .get_mut("data")
            .and_then(|data| data.get_mut("accounts"))
    } else {
        document.get_mut("accounts")
    };
    let Some(accounts) = accounts.and_then(Value::as_array_mut) else {
        return (bytes.to_vec(), 0);
    };
    let mut restored = 0;
    for (index, account) in accounts.iter_mut().enumerate() {
        let Some(expected) = records
            .iter()
            .find(|record| record.get("index").and_then(Value::as_u64) == Some(index as u64))
            .and_then(|record| record.get("payload_sha256"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if payload_sha256(account).as_deref() == Some(expected) {
            continue;
        }
        let Some(number) = account.get("rate_multiplier").and_then(Value::as_i64) else {
            continue;
        };
        let Ok(float) = serde_json::from_str::<Value>(&format!("{number}.0")) else {
            continue;
        };
        let mut candidate = account.clone();
        candidate["rate_multiplier"] = float;
        if payload_sha256(&candidate).as_deref() == Some(expected) {
            *account = candidate;
            restored += 1;
        }
    }
    if restored == 0 {
        return (bytes.to_vec(), 0);
    }
    match serde_json::to_vec(&document) {
        Ok(repaired) => (repaired, restored),
        Err(_) => (bytes.to_vec(), 0),
    }
}

/// 观澜的记录哈希：键排序、紧凑、非 ASCII 不转义的 JSON。
fn payload_sha256(account: &Value) -> Option<String> {
    serde_json::to_vec(&sorted(account))
        .ok()
        .map(|bytes| hex::encode(Sha256::digest(bytes)))
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
