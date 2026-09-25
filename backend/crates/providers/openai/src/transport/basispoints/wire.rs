//! BPS 线上体的基础构件：canonical-JSON 指纹、effort 归一、message 构造。

use serde_json::{Value, map::Map};
use sha2::{Digest, Sha256};

/// `fp(v)` = sha256(canonical_json(v))[:16] 的十六进制（32 个字符）。
///
/// Go 侧用 `json.Marshal`（键排序、无空白、精确数字）。cpr 独立运行，不与 Go
/// 服务共享状态，因此只要稳定即可：这里用递归排序键的 canonical JSON。
pub(crate) fn fingerprint(value: &Value) -> String {
    let mut buf = Vec::with_capacity(64);
    write_canonical(&mut buf, value);
    let digest = Sha256::digest(&buf);
    hex::encode(&digest[..16])
}

/// 递归写出排序键、无空白的 canonical JSON。数字原样透传（`arbitrary_precision`
/// 下 `Number` 的 `to_string()` 保真，不会把大整数压成 f64）。
fn write_canonical(out: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(number) => out.extend_from_slice(number.to_string().as_bytes()),
        Value::String(text) => write_json_string(out, text),
        Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_canonical(out, item);
            }
            out.push(b']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push(b'{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_json_string(out, key);
                out.push(b':');
                write_canonical(out, &map[key]);
            }
            out.push(b'}');
        }
    }
}

/// 写出 JSON 字符串（含首尾引号）。用数字字节码表达转义，源码中不含反斜杠字面量，
/// 避免任何转义层破坏。
fn write_json_string(out: &mut Vec<u8>, text: &str) {
    const QUOTE: u8 = 0x22;
    const BACKSLASH: u8 = 0x5c;
    out.push(QUOTE);
    for ch in text.chars() {
        let code = ch as u32;
        match code {
            0x22 => {
                out.push(BACKSLASH);
                out.push(QUOTE);
            }
            0x5c => {
                out.push(BACKSLASH);
                out.push(BACKSLASH);
            }
            0x08 => {
                out.push(BACKSLASH);
                out.push(b'b');
            }
            0x0c => {
                out.push(BACKSLASH);
                out.push(b'f');
            }
            0x0a => {
                out.push(BACKSLASH);
                out.push(b'n');
            }
            0x0d => {
                out.push(BACKSLASH);
                out.push(b'r');
            }
            0x09 => {
                out.push(BACKSLASH);
                out.push(b't');
            }
            c if c < 0x20 => {
                out.push(BACKSLASH);
                out.push(b'u');
                for shift in [12u32, 8, 4, 0] {
                    let nibble = ((c >> shift) & 0xf) as u8;
                    out.push(if nibble < 10 {
                        b'0' + nibble
                    } else {
                        b'a' + nibble - 10
                    });
                }
            }
            _ => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(QUOTE);
}

/// effort 归一失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizeEffortError {
    pub(crate) value: String,
}

/// BPS effort 归一表：
/// 空/medium→medium；low→low；high→high；
/// xhigh/x-high/extra-high/extra_high/max/ultra→xhigh；
/// none/minimal→low；其余报错。
pub(crate) fn normalize_effort(effort: &str) -> Result<&'static str, NormalizeEffortError> {
    match effort.trim().to_ascii_lowercase().as_str() {
        "" | "medium" => Ok("medium"),
        "low" => Ok("low"),
        "high" => Ok("high"),
        "xhigh" | "x-high" | "extra-high" | "extra_high" | "max" | "ultra" => Ok("xhigh"),
        "none" | "minimal" => Ok("low"),
        _ => Err(NormalizeEffortError {
            value: effort.to_owned(),
        }),
    }
}

/// 构造 BPS input 的 message 项：
/// `{"type":"message","role":r,"content":[{"type":"input_text","text":t}]}`。
pub(crate) fn message(role: &str, content: &str) -> Value {
    let mut part = Map::new();
    part.insert("type".to_owned(), Value::from("input_text"));
    part.insert("text".to_owned(), Value::from(content));
    let mut item = Map::new();
    item.insert("type".to_owned(), Value::from("message"));
    item.insert("role".to_owned(), Value::from(role));
    item.insert("content".to_owned(), Value::Array(vec![Value::Object(part)]));
    Value::Object(item)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fingerprint_is_stable_and_key_order_independent() {
        let a = json!({"b": 1, "a": [true, null, "x"]});
        let b = json!({"a": [true, null, "x"], "b": 1});
        assert_eq!(fingerprint(&a), fingerprint(&b));
        assert_eq!(fingerprint(&a).len(), 32);
    }

    #[test]
    fn fingerprint_escapes_control_and_quotes() {
        let quote = json!("a\"b");
        let newline = json!("a\nb");
        assert_ne!(fingerprint(&quote), fingerprint(&json!("ab")));
        assert_ne!(fingerprint(&newline), fingerprint(&json!("ab")));
    }

    #[test]
    fn fingerprint_preserves_large_integers() {
        let a: Value = serde_json::from_str(r#"{"n":9007199254740993}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"n":9007199254740992}"#).unwrap();
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }

    #[test]
    fn effort_table() {
        for (input, want) in [
            ("", "medium"),
            ("medium", "medium"),
            ("LOW", "low"),
            ("High", "high"),
            ("xhigh", "xhigh"),
            ("x-high", "xhigh"),
            ("extra_high", "xhigh"),
            ("max", "xhigh"),
            ("ultra", "xhigh"),
            ("none", "low"),
            ("minimal", "low"),
        ] {
            assert_eq!(normalize_effort(input).unwrap(), want, "effort {input}");
        }
        assert!(normalize_effort("banana").is_err());
    }

    #[test]
    fn message_shape() {
        let m = message("user", "hi");
        assert_eq!(m["type"], json!("message"));
        assert_eq!(m["role"], json!("user"));
        assert_eq!(m["content"][0]["type"], json!("input_text"));
        assert_eq!(m["content"][0]["text"], json!("hi"));
    }
}
