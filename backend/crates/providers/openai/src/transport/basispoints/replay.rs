//! 工具回放缓存：保留原生 `run_officejs` 项，供下一回合把客户端工具结果映射回上游。
//!
//! 进程内 LRU，key = `scope\0call_id`，值 = 原生项 + 客户端调用指纹。
//! 上限：≤1024 条、≤16 MiB、单条 >1 MiB 不入。

use std::collections::HashMap;

use serde_json::Value;

use super::util::{decode_one, str_field, to_compact};
use super::wire::fingerprint;

const MAX_ENTRIES: usize = 1024;
const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;
const MAX_ITEM_BYTES: usize = 1024 * 1024;
const KEY_SEP: char = '\u{0}';

struct Entry {
    raw: Vec<u8>,
    call_fingerprint: String,
    order: u64,
}

/// 进程内工具回放缓存。
#[derive(Default)]
pub(crate) struct ReplayCache {
    entries: HashMap<String, Entry>,
    total_bytes: usize,
    next_order: u64,
}

fn key(scope: &str, call_id: &str) -> String {
    let mut composed = String::with_capacity(scope.len() + 1 + call_id.len());
    composed.push_str(scope);
    composed.push(KEY_SEP);
    composed.push_str(call_id);
    composed
}

impl ReplayCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 写入原生项；同 key 覆盖。`client_call` 若给出，记录其指纹用于强匹配。
    pub(crate) fn put(
        &mut self,
        scope: &str,
        call_id: &str,
        item: &Value,
        client_call: Option<&Value>,
    ) {
        if call_id.is_empty() {
            return;
        }
        let Some(serialized) = to_compact(item) else {
            return;
        };
        let raw = serialized.into_bytes();
        if raw.len() > MAX_ITEM_BYTES {
            return;
        }
        let composed = key(scope, call_id);
        if let Some(previous) = self.entries.remove(&composed) {
            self.total_bytes -= previous.raw.len();
        }
        let call_fingerprint = client_call.map(client_call_fingerprint).unwrap_or_default();
        let order = self.next_order;
        self.next_order += 1;
        self.total_bytes += raw.len();
        self.entries.insert(
            composed,
            Entry {
                raw,
                call_fingerprint,
                order,
            },
        );
        self.evict();
    }

    /// 取原生项，忽略指纹（output-only 查找）。
    pub(crate) fn get_any(&mut self, scope: &str, call_id: &str) -> Option<Value> {
        self.get(scope, call_id, None)
    }

    /// 取原生项，要求客户端调用指纹相符（call 查找）。
    pub(crate) fn get_for_call(
        &mut self,
        scope: &str,
        call_id: &str,
        client_call: &Value,
    ) -> Option<Value> {
        let signature = client_call_fingerprint(client_call);
        if signature.is_empty() {
            return None;
        }
        self.get(scope, call_id, Some(&signature))
    }

    fn get(
        &mut self,
        scope: &str,
        call_id: &str,
        require_signature: Option<&str>,
    ) -> Option<Value> {
        let composed = key(scope, call_id);
        let order = self.next_order;
        let entry = self.entries.get_mut(&composed)?;
        if let Some(signature) = require_signature
            && entry.call_fingerprint != signature
        {
            return None;
        }
        entry.order = order;
        self.next_order += 1;
        decode_one(std::str::from_utf8(&entry.raw).ok()?)
    }

    fn evict(&mut self) {
        while self.entries.len() > MAX_ENTRIES || self.total_bytes > MAX_TOTAL_BYTES {
            let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.order)
                .map(|(composed, _)| composed.clone())
            else {
                break;
            };
            if let Some(entry) = self.entries.remove(&victim) {
                self.total_bytes -= entry.raw.len();
            }
        }
    }
}

/// 客户端调用指纹：覆盖 type/call_id/name/namespace + 解析后的 arguments 对象（键序无关）
/// 或精确 input 串。不可指纹化时返回空串。
pub(crate) fn client_call_fingerprint(item: &Value) -> String {
    let kind = str_field(item, "type");
    let id = str_field(item, "call_id");
    let name = str_field(item, "name");
    if id.is_empty() || name.is_empty() || id.trim() != id || name.trim() != name {
        return String::new();
    }
    let namespace = match item.get("namespace") {
        None => String::new(),
        Some(Value::String(text)) if text.trim() == text => text.clone(),
        Some(_) => return String::new(),
    };
    let mut canonical = serde_json::Map::new();
    canonical.insert("type".to_owned(), Value::from(kind));
    canonical.insert("call_id".to_owned(), Value::from(id));
    canonical.insert("name".to_owned(), Value::from(name));
    canonical.insert("namespace".to_owned(), Value::from(namespace));
    match kind {
        "function_call" => {
            let arguments = match item.get("arguments") {
                Some(Value::String(raw)) => match decode_one(raw) {
                    Some(value) => value,
                    None => return String::new(),
                },
                Some(other) => other.clone(),
                None => Value::Null,
            };
            if !arguments.is_object() {
                return String::new();
            }
            canonical.insert("arguments".to_owned(), arguments);
        }
        "custom_tool_call" => match item.get("input") {
            Some(Value::String(input)) => {
                canonical.insert("input".to_owned(), Value::from(input.clone()));
            }
            _ => return String::new(),
        },
        _ => return String::new(),
    }
    fingerprint(&Value::Object(canonical))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(call_id: &str, args: Value) -> Value {
        json!({"type": "function_call", "call_id": call_id, "name": "shell", "arguments": args})
    }

    #[test]
    fn put_get_roundtrip_and_signature_match() {
        let mut cache = ReplayCache::new();
        let native =
            json!({"type": "function_call", "id": "fc_x", "call_id": "c1", "name": "run_officejs"});
        let client = call("c1", json!({"command": "pwd"}));
        cache.put("scope", "c1", &native, Some(&client));

        let client2 = json!({"type": "function_call", "name": "shell", "call_id": "c1", "arguments": {"command": "pwd"}});
        assert_eq!(
            cache.get_for_call("scope", "c1", &client2),
            Some(native.clone())
        );
        assert_eq!(cache.get_any("scope", "c1"), Some(native));
        assert_eq!(
            cache.get_for_call("scope", "c1", &call("c1", json!({"command": "ls"}))),
            None
        );
    }

    #[test]
    fn arguments_as_string_are_parsed_for_fingerprint() {
        let obj_args = call("c2", json!({"a": 1}));
        let str_args = json!({"type": "function_call", "call_id": "c2", "name": "shell", "arguments": "{\"a\":1}"});
        assert_eq!(
            client_call_fingerprint(&obj_args),
            client_call_fingerprint(&str_args)
        );
    }

    #[test]
    fn scope_isolates_call_ids() {
        let mut cache = ReplayCache::new();
        let native = json!({"type": "function_call", "call_id": "c1"});
        cache.put("acct:1", "c1", &native, None);
        assert!(cache.get_any("acct:2", "c1").is_none());
        assert!(cache.get_any("acct:1", "c1").is_some());
    }

    #[test]
    fn empty_call_id_is_not_cached() {
        let mut cache = ReplayCache::new();
        cache.put("scope", "", &json!({"x": 1}), None);
        assert!(cache.get_any("scope", "").is_none());
    }
}
