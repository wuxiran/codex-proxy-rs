//! BPS 协议转换的共享小工具：JSON 取字段、严格单值解码、名称字符校验。

use serde::Deserialize;
use serde_json::{Value, map::Map};

/// 取对象字段的字符串值；缺失或非字符串返回空串（对齐 Go 的 `text(item[key])`）。
pub(crate) fn str_field<'a>(item: &'a Value, key: &str) -> &'a str {
    item.get(key).and_then(Value::as_str).unwrap_or("")
}

/// 严格解码：必须是且仅是一个 JSON 值，无尾随数据。大整数保真（arbitrary_precision）。
pub(crate) fn decode_one(raw: &str) -> Option<Value> {
    let mut de = serde_json::Deserializer::from_str(raw);
    let value = Value::deserialize(&mut de).ok()?;
    de.end().ok()?;
    Some(value)
}

/// 紧凑序列化（无空白）。序列化失败返回 None。
pub(crate) fn to_compact(value: &Value) -> Option<String> {
    serde_json::to_string(value).ok()
}

/// 工具名不得含 `/`、反斜杠、空白或控制符（对齐 Go 的 ContainsAny + IsSpace/IsControl）。
pub(crate) fn name_has_forbidden_chars(name: &str) -> bool {
    name.chars()
        .any(|c| c == '/' || (c as u32) == 0x5c || c.is_whitespace() || c.is_control())
}

/// 便捷构造对象。
pub(crate) fn obj(pairs: Vec<(&str, Value)>) -> Value {
    let mut map = Map::new();
    for (key, value) in pairs {
        map.insert(key.to_owned(), value);
    }
    Value::Object(map)
}
