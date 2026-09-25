//! 缓存创建计为普通输入的 usage 改写（§5.3，账号开关）。
//!
//! 只把「缓存创建/写入」相关字段清零；input_tokens / total_tokens / 缓存读取 / 输出 /
//! 推理保持不变，大整数保真。

use serde_json::Value;

/// 顶层清零字段。
const TOP_LEVEL_ZERO: &[&str] = &[
    "cache_write_tokens",
    "cache_creation_input_tokens",
    "cache_write_input_tokens",
    "cache_creation_tokens",
];

/// details 子对象里清零字段。
const DETAILS_ZERO: &[&str] = &["cache_write_tokens", "cache_creation_tokens"];

/// cache_creation 子对象里清零字段。
const CACHE_CREATION_ZERO: &[&str] = &["ephemeral_5m_input_tokens", "ephemeral_1h_input_tokens"];

/// 就地把一个 usage 对象里的缓存创建字段清零（若存在且非零）。
pub(crate) fn rewrite_cache_creation_as_input(usage: &mut Value) {
    let Some(map) = usage.as_object_mut() else {
        return;
    };
    for key in TOP_LEVEL_ZERO {
        zero_field(map.get_mut(*key));
    }
    for details in ["input_tokens_details", "prompt_tokens_details"] {
        if let Some(child) = map.get_mut(details).and_then(Value::as_object_mut) {
            for key in DETAILS_ZERO {
                zero_field(child.get_mut(*key));
            }
        }
    }
    if let Some(child) = map.get_mut("cache_creation").and_then(Value::as_object_mut) {
        for key in CACHE_CREATION_ZERO {
            zero_field(child.get_mut(*key));
        }
    }
}

fn zero_field(field: Option<&mut Value>) {
    if let Some(value) = field
        && value.is_number()
        && value.as_f64() != Some(0.0)
    {
        *value = Value::from(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn zeros_cache_creation_only() {
        let mut usage = json!({
            "input_tokens": 1000,
            "total_tokens": 1000,
            "cache_creation_input_tokens": 200,
            "cache_write_tokens": 50,
            "input_tokens_details": {"cache_read_tokens": 100, "cache_creation_tokens": 200},
            "cache_creation": {"ephemeral_5m_input_tokens": 200, "ephemeral_1h_input_tokens": 0},
            "output_tokens": 42
        });
        rewrite_cache_creation_as_input(&mut usage);
        assert_eq!(usage["input_tokens"], json!(1000));
        assert_eq!(usage["total_tokens"], json!(1000));
        assert_eq!(usage["output_tokens"], json!(42));
        assert_eq!(usage["cache_creation_input_tokens"], json!(0));
        assert_eq!(usage["cache_write_tokens"], json!(0));
        assert_eq!(
            usage["input_tokens_details"]["cache_read_tokens"],
            json!(100)
        );
        assert_eq!(
            usage["input_tokens_details"]["cache_creation_tokens"],
            json!(0)
        );
        assert_eq!(
            usage["cache_creation"]["ephemeral_5m_input_tokens"],
            json!(0)
        );
    }

    #[test]
    fn preserves_large_integers_elsewhere() {
        let mut usage: Value =
            serde_json::from_str(r#"{"input_tokens":9007199254740993,"cache_creation_tokens":10}"#)
                .unwrap();
        rewrite_cache_creation_as_input(&mut usage);
        assert_eq!(usage["input_tokens"].to_string(), "9007199254740993");
        assert_eq!(usage["cache_creation_tokens"], json!(0));
    }
}
