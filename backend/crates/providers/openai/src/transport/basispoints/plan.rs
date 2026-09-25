//! 原生 update_plan 适配（§4.1 ①）。只接受声明的 client plan 工具，绝不执行计划。

use serde_json::{Value, map::Map};

use super::catalog::{Catalog, CatalogTool, ToolKind};
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

pub(crate) fn translate_native_plan(
    catalog: &Catalog,
    scope: &str,
    replay: &mut ReplayCache,
    native: &Value,
) -> Result<Value, PrepareError> {
    let name = str_field(native, "name");
    if str_field(native, "type") != "function_call"
        || (name != "update_plan" && name != "functions.update_plan")
    {
        return Err(invalid(
            "basispoints native plan adapter requires an update_plan function call",
        ));
    }
    let mut selected: Option<&CatalogTool> = None;
    let mut matches = 0;
    for key in ["update_plan", "functions.update_plan"] {
        if let Some(candidate) = catalog.get(key) {
            selected = Some(candidate);
            matches += 1;
        }
    }
    let Some(selected) = selected else {
        return Err(invalid(
            "basispoints native update_plan requires one unambiguous client function declaration",
        ));
    };
    if matches != 1 || selected.kind != ToolKind::Function {
        return Err(invalid(
            "basispoints native update_plan requires one unambiguous client function declaration",
        ));
    }
    let parameters = selected.parameters.as_ref();
    let has_plan_property = parameters
        .and_then(Value::as_object)
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object)
        .is_some_and(|properties| properties.contains_key("plan"));
    let is_object = parameters
        .and_then(Value::as_object)
        .and_then(|schema| schema.get("type"))
        .and_then(Value::as_str)
        == Some("object");
    if !is_object || !has_plan_property {
        return Err(invalid(
            "basispoints native update_plan requires an explicit client plan argument schema",
        ));
    }
    let arguments = match native.get("arguments") {
        Some(Value::Object(map)) => Value::Object(map.clone()),
        _ => decode_one(str_field(native, "arguments"))
            .filter(Value::is_object)
            .ok_or_else(|| {
                invalid("basispoints native update_plan arguments must be one JSON object")
            })?,
    };
    let Some(steps) = arguments.get("plan").and_then(Value::as_array) else {
        return Err(invalid(
            "basispoints native update_plan requires a plan array",
        ));
    };
    let mut plan = Vec::with_capacity(steps.len());
    for value in steps {
        let Some(step) = value.as_object() else {
            return Err(invalid(
                "basispoints native update_plan entries must be objects",
            ));
        };
        let (description, present) = plan_text_alias(step, &["step", "description", "title"])?;
        if !present || description.trim().is_empty() {
            return Err(invalid(
                "basispoints native update_plan has an ambiguous or missing step description",
            ));
        }
        let status = normalize_native_plan_status(str_field(value, "status"));
        let Some(status) = status else {
            return Err(invalid(
                "basispoints native update_plan has an unsupported step status",
            ));
        };
        plan.push(obj(vec![
            ("step", Value::from(description)),
            ("status", Value::from(status)),
        ]));
    }
    let mut translated = Map::new();
    translated.insert("plan".to_owned(), Value::Array(plan));
    let (explanation, present) = plan_text_alias(
        arguments.as_object().unwrap_or(&Map::new()),
        &["explanation", "summary"],
    )?;
    if present {
        translated.insert("explanation".to_owned(), Value::from(explanation));
    }
    let translated = Value::Object(translated);
    if !plan_schema_accepts(&translated, parameters, 0) {
        return Err(invalid(
            "basispoints native update_plan does not satisfy the declared client argument schema",
        ));
    }
    let call_id = str_field(native, "call_id");
    if call_id.is_empty() || call_id.trim() != call_id {
        return Err(invalid(
            "basispoints native update_plan is missing a valid call_id",
        ));
    }
    let item_id = {
        let native_id = str_field(native, "id");
        if native_id.is_empty() {
            format!("fc_{}", fp_str(call_id))
        } else {
            native_id.to_owned()
        }
    };
    let encoded = to_compact(&translated).unwrap_or_else(|| "{}".to_owned());
    let mut result = Map::new();
    result.insert("type".to_owned(), Value::from("function_call"));
    result.insert("id".to_owned(), Value::from(item_id));
    result.insert("call_id".to_owned(), Value::from(call_id));
    result.insert("name".to_owned(), Value::from(selected.name.clone()));
    result.insert("arguments".to_owned(), Value::from(encoded));
    result.insert("status".to_owned(), Value::from("completed"));
    if !selected.namespace.is_empty() {
        result.insert(
            "namespace".to_owned(),
            Value::from(selected.namespace.clone()),
        );
    }
    let result = Value::Object(result);
    replay.put(scope, call_id, native, Some(&result));
    Ok(result)
}

fn plan_text_alias(
    value: &Map<String, Value>,
    keys: &[&str],
) -> Result<(String, bool), PrepareError> {
    let mut result = String::new();
    let mut present = false;
    for key in keys {
        if let Some(raw) = value.get(*key) {
            let Some(candidate) = raw.as_str() else {
                return Err(invalid("invalid or conflicting plan text fields"));
            };
            if present && result != candidate {
                return Err(invalid("invalid or conflicting plan text fields"));
            }
            result = candidate.to_owned();
            present = true;
        }
    }
    Ok((result, present))
}

fn normalize_native_plan_status(status: &str) -> Option<&'static str> {
    let normalized: String = status
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c == '-' || c == ' ' { '_' } else { c })
        .collect();
    match normalized.as_str() {
        "pending" | "not_started" | "todo" | "planned" | "queued" | "blocked" => Some("pending"),
        "in_progress" | "active" | "started" | "doing" | "current" => Some("in_progress"),
        "completed" | "complete" | "done" | "finished" => Some("completed"),
        _ => None,
    }
}

/// 针对标准 Codex plan schema 的小型 fail-closed 校验器，不是通用 JSON Schema 引擎。
fn plan_schema_accepts(value: &Value, schema: Option<&Value>, depth: usize) -> bool {
    let Some(Value::Object(schema)) = schema else {
        return false;
    };
    if depth > 8 {
        return false;
    }
    for key in schema.keys() {
        if !matches!(
            key.as_str(),
            "type"
                | "properties"
                | "required"
                | "additionalProperties"
                | "items"
                | "enum"
                | "const"
                | "minItems"
                | "maxItems"
                | "minLength"
                | "maxLength"
                | "description"
                | "title"
                | "default"
                | "examples"
                | "$comment"
        ) {
            return false;
        }
    }
    if let Some(raw) = schema.get("properties")
        && !raw.is_object()
    {
        return false;
    }
    if let Some(kind) = schema.get("type")
        && !plan_schema_type_matches(value, kind)
    {
        return false;
    }
    if let Some(constant) = schema.get("const")
        && value != constant
    {
        return false;
    }
    if let Some(choices) = schema.get("enum") {
        let Some(values) = choices.as_array().filter(|values| !values.is_empty()) else {
            return false;
        };
        if !values.iter().any(|choice| choice == value) {
            return false;
        }
    }
    match value {
        Value::Object(map) => {
            let empty = Map::new();
            let properties = schema
                .get("properties")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            if let Some(required) = schema.get("required") {
                let Some(names) = required.as_array() else {
                    return false;
                };
                for name in names {
                    let Some(key) = name.as_str() else {
                        return false;
                    };
                    if !map.contains_key(key) {
                        return false;
                    }
                }
            }
            let additional = schema.get("additionalProperties");
            if let Some(additional) = additional
                && !additional.is_boolean()
            {
                return false;
            }
            let additional_false = additional == Some(&Value::Bool(false));
            for (key, nested) in map {
                if let Some(nested_schema) = properties.get(key) {
                    if !plan_schema_accepts(nested, Some(nested_schema), depth + 1) {
                        return false;
                    }
                } else if additional_false {
                    return false;
                }
            }
        }
        Value::Array(items) => {
            if !plan_schema_length(items.len(), schema, "minItems", "maxItems") {
                return false;
            }
            if let Some(item_schema) = schema.get("items") {
                if !item_schema.is_object() {
                    return false;
                }
                for item in items {
                    if !plan_schema_accepts(item, Some(item_schema), depth + 1) {
                        return false;
                    }
                }
            }
        }
        Value::String(text)
            if !plan_schema_length(text.chars().count(), schema, "minLength", "maxLength") =>
        {
            return false;
        }
        _ => {}
    }
    true
}

fn plan_schema_type_matches(value: &Value, kind: &Value) -> bool {
    if let Some(alternatives) = kind.as_array() {
        return alternatives
            .iter()
            .any(|candidate| plan_schema_type_matches(value, candidate));
    }
    match kind.as_str() {
        Some("object") => value.is_object(),
        Some("array") => value.is_array(),
        Some("string") => value.is_string(),
        Some("null") => value.is_null(),
        _ => false,
    }
}

fn plan_schema_length(
    length: usize,
    schema: &Map<String, Value>,
    minimum: &str,
    maximum: &str,
) -> bool {
    for (key, is_min) in [(minimum, true), (maximum, false)] {
        if let Some(value) = schema.get(key) {
            let Some(limit) = value.as_i64() else {
                return false;
            };
            if limit < 0 {
                return false;
            }
            let length = length as i64;
            if (is_min && length < limit) || (!is_min && length > limit) {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plan_catalog() -> Catalog {
        let mut catalog = Catalog::default();
        let tools = json!([{
            "type": "function", "name": "update_plan",
            "parameters": {"type": "object", "properties": {"plan": {"type": "array"}}}
        }]);
        super::super::catalog::collect_tools(&mut catalog, Some(&tools), "").unwrap();
        catalog
    }

    #[test]
    fn translates_plan_with_status_normalization() {
        let catalog = plan_catalog();
        let mut replay = ReplayCache::new();
        let native = json!({
            "type": "function_call", "name": "update_plan", "call_id": "p1",
            "arguments": {"plan": [
                {"description": "first", "status": "in-progress"},
                {"step": "second", "status": "TODO"}
            ], "explanation": "why"}
        });
        let result = translate_native_plan(&catalog, "scope", &mut replay, &native).unwrap();
        let args: Value = serde_json::from_str(result["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["plan"][0]["step"], json!("first"));
        assert_eq!(args["plan"][0]["status"], json!("in_progress"));
        assert_eq!(args["plan"][1]["status"], json!("pending"));
        assert_eq!(args["explanation"], json!("why"));
        assert_eq!(result["name"], json!("update_plan"));
    }

    #[test]
    fn rejects_unknown_status() {
        let catalog = plan_catalog();
        let mut replay = ReplayCache::new();
        let native = json!({
            "type": "function_call", "name": "update_plan", "call_id": "p2",
            "arguments": {"plan": [{"step": "x", "status": "banana"}]}
        });
        assert!(translate_native_plan(&catalog, "scope", &mut replay, &native).is_err());
    }

    #[test]
    fn status_families() {
        assert_eq!(normalize_native_plan_status("Not Started"), Some("pending"));
        assert_eq!(normalize_native_plan_status("doing"), Some("in_progress"));
        assert_eq!(normalize_native_plan_status("done"), Some("completed"));
        assert_eq!(normalize_native_plan_status("weird"), None);
    }
}
