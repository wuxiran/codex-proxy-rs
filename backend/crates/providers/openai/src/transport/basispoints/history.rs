//! 历史翻译（§2）：客户端项 → BPS input，工具调用映射为原生 run_officejs 项。

use std::collections::HashSet;

use serde_json::{Value, map::Map};

use super::catalog::{Catalog, ToolKind, supports_function_code_transport};
use super::content::validate_history_content;
use super::envelope::encode_function_code_transport;
use super::error::PrepareError;
use super::replay::ReplayCache;
use super::util::{decode_one, obj, str_field, to_compact};
use super::wire::fingerprint;

fn invalid(message: impl Into<String>) -> PrepareError {
    PrepareError::invalid(message)
}

fn fp_str(text: &str) -> String {
    fingerprint(&Value::from(text))
}

/// 翻译整段历史；返回上游 input 项列表（compaction_trigger 移到末尾）。
pub(crate) fn translate_history(
    catalog: &Catalog,
    scope: &str,
    replay: &mut ReplayCache,
    input: &[Value],
) -> Result<Vec<Value>, PrepareError> {
    let mut result = Vec::with_capacity(input.len());
    let mut seen: HashSet<String> = HashSet::new();
    let mut trigger: Option<Value> = None;
    for (index, raw) in input.iter().enumerate() {
        let mut item = raw.clone();
        if let Value::Object(map) = &mut item {
            map.remove("internal_chat_message_metadata_passthrough");
        }
        match str_field(&item, "type") {
            "additional_tools" => continue,
            "item_reference" => {
                return Err(invalid(
                    "basispoints requires full history; item_reference is unsupported",
                ));
            }
            "compaction_trigger" => {
                trigger = Some(item);
                continue;
            }
            "reasoning" => {
                let encrypted = str_field(&item, "encrypted_content");
                if !encrypted.is_empty() {
                    result.push(obj(vec![
                        ("type", Value::from("reasoning")),
                        ("summary", Value::Array(Vec::new())),
                        ("encrypted_content", Value::from(encrypted)),
                    ]));
                }
                continue;
            }
            "function_call" | "custom_tool_call" => {
                let id = str_field(&item, "call_id").to_owned();
                item = match replay.get_for_call(scope, &id, &item) {
                    Some(cached) => cached,
                    None => {
                        let rebuilt = rebuild_native_history_call(catalog, &item)?;
                        replay.put(scope, &id, &rebuilt, Some(&item));
                        rebuilt
                    }
                };
                seen.insert(id);
            }
            "function_call_output" | "custom_tool_call_output" => {
                let id = str_field(&item, "call_id").to_owned();
                if !seen.contains(&id) {
                    let native = replay.get_any(scope, &id).ok_or_else(|| {
                        invalid("basispoints original tool item is unavailable for this tool result; start a new conversation")
                    })?;
                    result.push(native);
                    seen.insert(id.clone());
                }
                let output = item.get("output").cloned();
                validate_history_content(output.as_ref(), index, "output")?;
                let item_id = {
                    let current = str_field(&item, "id");
                    if current.is_empty() {
                        format!("fc_{id}")
                    } else if !current.starts_with("fc_") || current.len() > 64 {
                        format!("fc_{}", fp_str(current))
                    } else {
                        current.to_owned()
                    }
                };
                if let Value::Object(map) = &mut item {
                    map.insert("type".to_owned(), Value::from("function_call_output"));
                    map.insert("id".to_owned(), Value::from(item_id));
                }
            }
            "configuration_update" => {
                return Err(invalid(
                    "basispoints does not support configuration_update; start a new request with the desired effort",
                ));
            }
            _ => {}
        }
        let content = item.get("content").cloned();
        validate_history_content(content.as_ref(), index, "content")?;
        result.push(item);
    }
    if let Some(trigger) = trigger {
        result.push(trigger);
    }
    Ok(result)
}

/// 从客户端提供的完整调用重建原生 run_officejs 项（缓存缺失时）。
pub(crate) fn rebuild_native_history_call(
    catalog: &Catalog,
    item: &Value,
) -> Result<Value, PrepareError> {
    let id = str_field(item, "call_id");
    let mut name = str_field(item, "name").to_owned();
    if id.is_empty() || id.trim() != id || name.is_empty() || name.trim() != name {
        return Err(invalid(
            "basispoints history recovery requires a complete tool call with nonempty call_id and name",
        ));
    }
    if let Some(value) = item.get("namespace") {
        let Some(namespace) = value.as_str() else {
            return Err(invalid(
                "basispoints history tool namespace must be a string",
            ));
        };
        if namespace.trim() != namespace {
            return Err(invalid(
                "basispoints history tool namespace must be a string",
            ));
        }
        if !namespace.is_empty() {
            name = format!("{namespace}.{name}");
        }
    }
    let kind = str_field(item, "type");
    let mut envelope = Map::new();
    envelope.insert("name".to_owned(), Value::from(name.clone()));
    match kind {
        "function_call" => {
            let arguments = match item.get("arguments") {
                Some(Value::String(raw)) => decode_one(raw)
                    .filter(Value::is_object)
                    .ok_or_else(|| invalid("basispoints history function arguments must contain one valid JSON object"))?,
                Some(other) => other.clone(),
                None => Value::Null,
            };
            if !arguments.is_object() {
                return Err(invalid(
                    "basispoints history function arguments must be a JSON object",
                ));
            }
            envelope.insert("arguments".to_owned(), arguments);
        }
        "custom_tool_call" => {
            let Some(input) = item.get("input").and_then(Value::as_str) else {
                return Err(invalid(
                    "basispoints history custom tool input must be a string",
                ));
            };
            envelope.insert("input".to_owned(), Value::from(input));
        }
        _ => {
            return Err(invalid(
                "basispoints history recovery requires a function or custom tool call",
            ));
        }
    }
    let code = to_compact(&Value::Object(envelope.clone()))
        .ok_or_else(|| invalid("basispoints history tool arguments cannot be serialized"))?;
    let mut outer = obj(vec![
        ("code", Value::from(code)),
        (
            "summary",
            Value::from("Replay a previously requested client tool"),
        ),
        (
            "extended_summary",
            Value::from(
                "The supplied client history contains this tool call; consume its recorded result without repeating it.",
            ),
        ),
        ("destructive", Value::from(false)),
        ("references", Value::Array(Vec::new())),
    ]);
    if kind == "function_call"
        && let Some(info) = catalog.get(&name)
        && supports_function_code_transport(
            &name,
            info.kind == ToolKind::Function,
            info.parameters.as_ref(),
        )
        && let Some(Value::Object(args)) = envelope.get("arguments")
        && args.get("code").and_then(Value::as_str).is_some()
    {
        outer = encode_function_code_transport(&name, args);
    }
    let arguments_str = to_compact(&outer)
        .ok_or_else(|| invalid("basispoints history transport cannot be serialized"))?;
    let item_id = {
        let current = str_field(item, "id");
        if !current.starts_with("fc_") || current.len() > 64 {
            format!("fc_{}", fp_str(id))
        } else {
            current.to_owned()
        }
    };
    Ok(obj(vec![
        ("type", Value::from("function_call")),
        ("id", Value::from(item_id)),
        ("call_id", Value::from(id)),
        ("name", Value::from("run_officejs")),
        ("arguments", Value::from(arguments_str)),
        ("status", Value::from("completed")),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog() -> Catalog {
        let mut catalog = Catalog::default();
        let tools =
            json!([{"type": "function", "name": "shell", "parameters": {"type": "object"}}]);
        super::super::catalog::collect_tools(&mut catalog, Some(&tools), "").unwrap();
        catalog
    }

    #[test]
    fn passthrough_key_removed_and_message_kept() {
        let input = vec![json!({
            "type": "message", "role": "user",
            "content": [{"type": "input_text", "text": "hi"}],
            "internal_chat_message_metadata_passthrough": {"x": 1}
        })];
        let mut replay = ReplayCache::new();
        let out = translate_history(&catalog(), "scope", &mut replay, &input).unwrap();
        assert_eq!(out.len(), 1);
        assert!(
            out[0]
                .get("internal_chat_message_metadata_passthrough")
                .is_none()
        );
    }

    #[test]
    fn reasoning_keeps_only_encrypted() {
        let input = vec![
            json!({"type": "reasoning", "encrypted_content": "E", "summary": ["s"]}),
            json!({"type": "reasoning"}),
        ];
        let mut replay = ReplayCache::new();
        let out = translate_history(&catalog(), "scope", &mut replay, &input).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["encrypted_content"], json!("E"));
        assert_eq!(out[0]["summary"], json!([]));
    }

    #[test]
    fn compaction_trigger_moves_to_end() {
        let input = vec![
            json!({"type": "compaction_trigger"}),
            json!({"type": "message", "role": "user", "content": "hi"}),
        ];
        let mut replay = ReplayCache::new();
        let out = translate_history(&catalog(), "scope", &mut replay, &input).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(str_field(&out[1], "type"), "compaction_trigger");
    }

    #[test]
    fn call_then_output_roundtrip() {
        let input = vec![
            json!({"type": "function_call", "name": "shell", "call_id": "c1", "arguments": {"command": "pwd"}}),
            json!({"type": "function_call_output", "call_id": "c1", "id": "ctco_x", "output": "ok"}),
        ];
        let mut replay = ReplayCache::new();
        let out = translate_history(&catalog(), "scope", &mut replay, &input).unwrap();
        // native run_officejs, then the normalized output
        assert_eq!(str_field(&out[0], "name"), "run_officejs");
        assert_eq!(str_field(&out[1], "type"), "function_call_output");
        assert!(str_field(&out[1], "id").starts_with("fc_"));
    }

    #[test]
    fn output_without_prior_call_needs_cache() {
        let input =
            vec![json!({"type": "function_call_output", "call_id": "missing", "output": "x"})];
        let mut replay = ReplayCache::new();
        let err = translate_history(&catalog(), "scope", &mut replay, &input).unwrap_err();
        assert!(format!("{err}").contains("original tool item is unavailable"));
    }

    #[test]
    fn item_reference_rejected() {
        let input = vec![json!({"type": "item_reference", "id": "r1"})];
        let mut replay = ReplayCache::new();
        assert!(translate_history(&catalog(), "scope", &mut replay, &input).is_err());
    }
}
