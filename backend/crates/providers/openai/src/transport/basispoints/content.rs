//! 历史内容块校验（§2.3）：只允许文本与 https input_image，错误文案不回显内容。

use serde_json::Value;

use super::error::PrepareError;
use super::util::str_field;

/// 校验一个项的 content / output 数组。非数组视为通过（字符串 content 合法）。
pub(crate) fn validate_history_content(
    value: Option<&Value>,
    input_index: usize,
    field: &str,
) -> Result<(), PrepareError> {
    let Some(Value::Array(parts)) = value else {
        return Ok(());
    };
    for (index, part) in parts.iter().enumerate() {
        match str_field(part, "type") {
            "input_text" | "output_text" | "text" | "refusal" => {}
            "input_image" => {
                if let Err(message) = validate_image(part) {
                    return Err(PrepareError::invalid(format!(
                        "{message} (path=input[{input_index}].{field}[{index}])"
                    )));
                }
            }
            _ => {
                return Err(PrepareError::invalid(format!(
                    "basispoints supports text and HTTPS input_image content only (path=input[{input_index}].{field}[{index}]; type={})",
                    content_type_diagnostic(part)
                )));
            }
        }
    }
    Ok(())
}

fn validate_image(part: &Value) -> Result<(), &'static str> {
    let raw = match part.get("image_url") {
        Some(Value::String(text)) if !text.is_empty() => text.as_str(),
        _ => {
            return Err(
                "basispoints input_image requires an HTTPS image_url; file IDs are unsupported",
            );
        }
    };
    if raw.trim().to_ascii_lowercase().starts_with("data:") {
        return Err(
            "basispoints does not accept data:image/base64 image input; provide an HTTPS image URL, or disable Basispoints and start a new conversation to send this image",
        );
    }
    let parsed = url::Url::parse(raw);
    let valid = matches!(&parsed, Ok(url)
        if url.scheme() == "https"
            && url.host_str().is_some_and(|host| !host.is_empty())
            && url.username().is_empty()
            && url.password().is_none()
            && !url.cannot_be_a_base())
        && raw.trim() == raw;
    if !valid {
        return Err(
            "basispoints input_image requires an absolute HTTPS image URL without embedded credentials",
        );
    }
    if !str_field(part, "file_id").is_empty() {
        return Err(
            "basispoints input_image does not support file_id; provide only an HTTPS image_url",
        );
    }
    if let Some(detail) = part.get("detail")
        && !detail.is_null()
    {
        match detail.as_str() {
            Some("auto") | Some("low") | Some("high") => {}
            _ => return Err("basispoints image detail must be auto, low or high"),
        }
    }
    Ok(())
}

/// 只回固定协议标签；未知 type 归 unknown，不回显调用方内容。
fn content_type_diagnostic(part: &Value) -> &'static str {
    let Some(map) = part.as_object() else {
        return "non_object";
    };
    let Some(value) = map.get("type") else {
        return "missing";
    };
    let Some(kind) = value.as_str() else {
        return "non_string";
    };
    match kind {
        "image" => "image",
        "image_url" => "image_url",
        "input_file" => "input_file",
        "file" => "file",
        "document" => "document",
        "input_audio" => "input_audio",
        "output_audio" => "output_audio",
        "audio" => "audio",
        "reasoning_text" => "reasoning_text",
        "summary_text" => "summary_text",
        "tool_use" => "tool_use",
        "tool_result" => "tool_result",
        "thinking" => "thinking",
        "redacted_thinking" => "redacted_thinking",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_parts_pass() {
        let content = json!([{"type": "input_text", "text": "hi"}, {"type": "refusal"}]);
        assert!(validate_history_content(Some(&content), 0, "content").is_ok());
    }

    #[test]
    fn string_content_passes() {
        let content = json!("plain string");
        assert!(validate_history_content(Some(&content), 0, "content").is_ok());
    }

    #[test]
    fn https_image_ok_data_and_fileid_rejected() {
        let ok = json!([{"type": "input_image", "image_url": "https://example.com/a.png", "detail": "high"}]);
        assert!(validate_history_content(Some(&ok), 0, "content").is_ok());

        let data = json!([{"type": "input_image", "image_url": "data:image/png;base64,AAA"}]);
        let err = validate_history_content(Some(&data), 1, "content").unwrap_err();
        assert!(format!("{err}").contains("data:image/base64"));
        assert!(format!("{err}").contains("path=input[1].content[0]"));

        let file = json!([{"type": "input_image", "image_url": "https://example.com/a.png", "file_id": "f1"}]);
        assert!(validate_history_content(Some(&file), 0, "content").is_err());

        let creds =
            json!([{"type": "input_image", "image_url": "https://user:pw@example.com/a.png"}]);
        assert!(validate_history_content(Some(&creds), 0, "content").is_err());
    }

    #[test]
    fn unknown_part_type_diagnostic_is_fixed_label() {
        let audio = json!([{"type": "input_audio", "audio": "secret"}]);
        let err = validate_history_content(Some(&audio), 2, "output").unwrap_err();
        let text = format!("{err}");
        assert!(text.contains("type=input_audio"));
        assert!(!text.contains("secret"));

        let weird = json!([{"type": "totally_private_value"}]);
        let err = validate_history_content(Some(&weird), 0, "content").unwrap_err();
        assert!(format!("{err}").contains("type=unknown"));
    }
}
