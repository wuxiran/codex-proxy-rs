//! Prepare：把客户端 Responses 请求转成 BPS 线上体，并返回 Bridge 供流转换复用（§1）。

use serde_json::{Value, json, map::Map};

use super::catalog::{Catalog, collect_tools, describe_catalog};
use super::envelope::translate_call;
use super::error::PrepareError;
use super::history::translate_history;
use super::replay::ReplayCache;
use super::util::{obj, str_field};
use super::wire::{fingerprint, message, normalize_effort};

fn invalid(message: impl Into<String>) -> PrepareError {
    PrepareError::invalid(message)
}

const PROTOCOL_NO_TOOLS: &str = "This request comes from an external Responses client. Return assistant text. Do not call Excel, Office, workbook or connector tools.";

const PROTOCOL_TOOLS_PREFIX: &str = r#"This request comes from an external Responses client. Use only the client tools in the catalog below. There is no live Excel workbook for this request. The proxy intercepts run_officejs as a transport and never executes Office code. To call one client tool, call native run_officejs using the transport matching its catalog type. FUNCTION: code must contain one serialized JSON object {"name":"CATALOG_NAME","arguments":{...}}. Arguments is an object, not an extra JSON string. FUNCTION_CODE: when a catalog function explicitly specifies this transport, set summary to exactly codex2api.function_code/CATALOG_NAME, put its exact code argument directly in native code, and serialize all other arguments as one JSON object in extended_summary ({} if none). Never put code in extended_summary. This replaces the FUNCTION envelope for that tool, so do not JSON-wrap, fence or re-escape the code text. CUSTOM: set summary to exactly codex2api.custom/CATALOG_NAME and put the exact raw tool input directly in code. Do not wrap custom input in another JSON object or add Markdown fences. For example, custom functions.exec uses summary=codex2api.custom/functions.exec and code containing its raw JavaScript; custom functions.apply_patch uses its exact patch text. The marker is mandatory for raw input. CATALOG_NAME includes its exact namespace. Outer arguments also include extended_summary, destructive=false and references=[]. For ordinary FUNCTION transport, use a descriptive summary; FUNCTION_CODE uses its exact marker and metadata JSON instead. Never nest run_officejs inside code. Serialize outer native arguments with proper JSON escaping. For FUNCTION envelopes also escape all quotes, backslashes, newline, carriage return and tab characters within JSON string values. Call one client tool at a time, including update_plan through this transport. After receiving its result continue the task; do not repeat completed calls. Tool results replayed under run_officejs are the named client tool's results. When a tool is needed, emit its call in this response instead of only announcing it. Do not call other native tools or claim that shell, filesystem or workspace access is unavailable when a suitable catalog tool exists. If no tool is needed, answer as assistant text. Client tool catalog:"#;

const PROTOCOL_TOOLS_SUFFIX: &str = "End of catalog. Invoke native run_officejs once. Follow each tool's specified transport: FUNCTION uses a JSON envelope; FUNCTION_CODE uses raw code plus metadata JSON in extended_summary; CUSTOM uses its exact marker and raw input. No Office code is executed by the proxy.";

const STRUCTURED_NOTE: &str = "The client requires a structured final answer. Your final assistant answer must be exactly one JSON value, with no Markdown fences or surrounding prose. Tool calls and refusals remain separate protocol items; use the client tool transport as needed before the final answer. The gateway validates the final JSON before returning it to the client.";

/// 换行符（源码不写反斜杠字面量）。
fn newline() -> char {
    char::from(10)
}

/// prepare 的产物之一：供流转换复用的桥接上下文。
pub(crate) struct Bridge {
    requested_effort: String,
    effort: &'static str,
    warnings: Vec<String>,
    scope: String,
    catalog: Catalog,
    structured: bool,
}

impl Bridge {
    pub(crate) fn effort(&self) -> &'static str {
        self.effort
    }

    pub(crate) fn requested_effort(&self) -> &str {
        &self.requested_effort
    }

    pub(crate) fn is_structured(&self) -> bool {
        self.structured
    }

    pub(crate) fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub(crate) fn scope(&self) -> &str {
        &self.scope
    }

    /// translateResponse：翻译 output 里的工具项，并写入 reasoning.effort 与 parallel_tool_calls=false。
    pub(crate) fn translate_response(
        &self,
        response: &mut Value,
        replay: &mut ReplayCache,
    ) -> Result<(), PrepareError> {
        let Value::Object(map) = response else {
            return Ok(());
        };
        if let Some(Value::Array(output)) = map.get_mut("output") {
            for item in output.iter_mut() {
                if is_tool(item) {
                    let translated = translate_call(&self.catalog, &self.scope, replay, item)?;
                    *item = translated;
                }
            }
        }
        map.insert(
            "reasoning".to_owned(),
            obj(vec![("effort", Value::from(self.effort))]),
        );
        map.insert("parallel_tool_calls".to_owned(), Value::from(false));
        Ok(())
    }
}

/// 某项是否为工具调用（function_call / custom_tool_call）。
pub(crate) fn is_tool(item: &Value) -> bool {
    matches!(
        str_field(item, "type"),
        "function_call" | "custom_tool_call"
    )
}

/// 把客户端请求体转成 BPS 线上体 + Bridge。
pub(crate) fn prepare(
    body: &Value,
    scope: &str,
    replay: &mut ReplayCache,
) -> Result<(Value, Bridge), PrepareError> {
    if !body.is_object() {
        return Err(invalid("invalid Basispoints request JSON"));
    }
    let model = str_field(body, "model").trim().to_owned();
    if model.is_empty() {
        return Err(invalid("basispoints requires a model"));
    }
    if !str_field(body, "previous_response_id").is_empty() {
        return Err(invalid(
            "basispoints requires expanded history instead of previous_response_id",
        ));
    }

    let mut requested = str_field(body, "reasoning_effort").to_owned();
    if let Some(Value::Object(reasoning)) = body.get("reasoning") {
        requested = reasoning
            .get("effort")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let mode = reasoning.get("mode").and_then(Value::as_str).unwrap_or("");
        if !mode.is_empty() && mode != "standard" {
            return Err(invalid(format!(
                "basispoints does not support reasoning mode {mode:?}"
            )));
        }
    }
    let effort = normalize_effort(&requested).map_err(|error| {
        invalid(format!(
            "basispoints reasoning effort {:?} is unsupported",
            error.value
        ))
    })?;

    let structured = match body
        .get("text")
        .and_then(|text| text.get("format"))
        .and_then(|format| format.get("type"))
        .and_then(Value::as_str)
    {
        Some("json_schema") => return Err(PrepareError::RouteNative("structured json_schema")),
        Some("json_object") => true,
        _ => false,
    };

    let choice = body.get("tool_choice");
    let choice_str = choice.and_then(Value::as_str).unwrap_or("");
    if let Some(choice) = choice
        && !choice.is_null()
        && choice_str != "auto"
        && choice_str != "none"
    {
        return Err(invalid(
            "basispoints supports tool_choice auto or none only",
        ));
    }

    let mut catalog = Catalog::default();
    if choice_str != "none" {
        collect_tools(&mut catalog, body.get("tools"), "")?;
        if let Some(Value::Array(items)) = body.get("input") {
            for item in items {
                if str_field(item, "type") == "additional_tools" {
                    collect_tools(&mut catalog, item.get("tools"), "")?;
                }
            }
        }
    }

    let input: Vec<Value> = match body.get("input") {
        Some(Value::String(text)) => vec![message("user", text)],
        Some(Value::Array(items)) => items.clone(),
        _ => {
            return Err(invalid(
                "basispoints input must be text or a Responses item array",
            ));
        }
    };

    let translated = translate_history(&catalog, scope, replay, &input)?;

    let mut prologue: Vec<Value> = Vec::with_capacity(2);
    let instructions = str_field(body, "instructions");
    if !instructions.is_empty() {
        prologue.push(message("developer", instructions));
    }

    let mut warnings: Vec<String> = Vec::new();
    let mut protocol = if catalog.is_empty() {
        PROTOCOL_NO_TOOLS.to_owned()
    } else {
        let mut text = String::with_capacity(PROTOCOL_TOOLS_PREFIX.len() + 512);
        text.push_str(PROTOCOL_TOOLS_PREFIX);
        text.push(newline());
        text.push_str(&describe_catalog(&catalog.entries));
        text.push(newline());
        text.push_str(PROTOCOL_TOOLS_SUFFIX);
        text
    };
    if !catalog.unsupported.is_empty() {
        let mut kinds = catalog.unsupported.clone();
        kinds.sort();
        let warning = format!(
            "Hosted tools unavailable through Basispoints: {}",
            kinds.join(", ")
        );
        warnings.push(warning.clone());
        protocol.push(newline());
        protocol.push_str(&warning);
        protocol.push_str(". These declarations were omitted. Do not claim to have used them. If the task requires one, explain the limitation or use a suitable declared client tool.");
    }
    if structured {
        protocol.push(newline());
        protocol.push_str(STRUCTURED_NOTE);
    }
    prologue.push(message("developer", &protocol));

    // metadata
    let cache_key = str_field(body, "prompt_cache_key");
    let conversation = if !cache_key.is_empty() {
        cache_key.to_owned()
    } else if !input.is_empty() {
        fingerprint(&input[0])
    } else {
        String::new()
    };
    let mut turn_end = if input.is_empty() { 0 } else { 1 };
    let mut iteration = 1i64;
    for i in (0..input.len()).rev() {
        if str_field(&input[i], "role") == "user" {
            turn_end = i + 1;
            break;
        }
        if str_field(&input[i], "type").ends_with("_call_output") {
            iteration += 1;
        }
    }
    let task_id = fingerprint(&json!([scope, conversation]));
    let turn_id = fingerprint(&Value::Array(vec![
        Value::from(scope),
        Value::Array(input[..turn_end].to_vec()),
    ]));
    let metadata = obj(vec![
        ("task_id", Value::from(task_id)),
        ("turn_id", Value::from(turn_id)),
        ("agent_iteration", Value::from(iteration.to_string())),
    ]);

    let mut wire_input = prologue;
    wire_input.extend(translated);

    let context_management = match body.get("context_management") {
        Some(array @ Value::Array(_)) => array.clone(),
        _ => json!([{"type": "compaction", "compact_threshold": 200000}]),
    };

    let mut output = Map::new();
    output.insert("model".to_owned(), Value::from(model));
    output.insert("model_selection".to_owned(), Value::from("explicit"));
    output.insert("stream".to_owned(), Value::from(true));
    output.insert("store".to_owned(), Value::from(false));
    output.insert("input".to_owned(), Value::Array(wire_input));
    output.insert("reasoning_effort".to_owned(), Value::from(effort));
    output.insert("context_management".to_owned(), context_management);
    output.insert("metadata".to_owned(), metadata);
    if !cache_key.is_empty() {
        let key = fingerprint(&json!([scope, cache_key]));
        output.insert(
            "prompt_cache_key".to_owned(),
            Value::from(format!("bps-{key}")),
        );
    }

    let bridge = Bridge {
        requested_effort: requested,
        effort,
        warnings,
        scope: scope.to_owned(),
        catalog,
        structured,
    };
    Ok((Value::Object(output), bridge))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_input_produces_whitelist_body() {
        let body = json!({"model": "gpt-5.6", "input": "hello", "reasoning_effort": "high"});
        let mut replay = ReplayCache::new();
        let (wire, bridge) = prepare(&body, "acct:1/key:k/thread:t", &mut replay).unwrap();
        assert_eq!(wire["model"], json!("gpt-5.6"));
        assert_eq!(wire["model_selection"], json!("explicit"));
        assert_eq!(wire["stream"], json!(true));
        assert_eq!(wire["store"], json!(false));
        assert_eq!(wire["reasoning_effort"], json!("high"));
        assert_eq!(bridge.effort(), "high");
        // dropped fields
        assert!(wire.get("tools").is_none());
        assert!(wire.get("temperature").is_none());
        // prologue: developer(protocol) then the user message
        let input = wire["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], json!("developer"));
        assert_eq!(input[1]["role"], json!("user"));
        assert_eq!(input[1]["content"][0]["text"], json!("hello"));
        // metadata present
        assert_eq!(wire["metadata"]["agent_iteration"], json!("1"));
        assert!(wire["metadata"]["task_id"].as_str().unwrap().len() == 32);
        // context_management default
        assert_eq!(
            wire["context_management"][0]["compact_threshold"],
            json!(200000)
        );
    }

    #[test]
    fn effort_max_maps_to_xhigh_and_reasoning_object_wins() {
        let body = json!({"model": "m", "input": "x", "reasoning_effort": "low", "reasoning": {"effort": "max"}});
        let mut replay = ReplayCache::new();
        let (wire, _) = prepare(&body, "s", &mut replay).unwrap();
        assert_eq!(wire["reasoning_effort"], json!("xhigh"));
    }

    #[test]
    fn previous_response_id_and_bad_tool_choice_rejected() {
        let mut replay = ReplayCache::new();
        let a = prepare(
            &json!({"model": "m", "input": "x", "previous_response_id": "r"}),
            "s",
            &mut replay,
        );
        assert!(matches!(a, Err(PrepareError::Invalid(_))));
        let b = prepare(
            &json!({"model": "m", "input": "x", "tool_choice": {"type": "function"}}),
            "s",
            &mut replay,
        );
        assert!(matches!(b, Err(PrepareError::Invalid(_))));
    }

    #[test]
    fn json_schema_routes_native() {
        let body = json!({"model": "m", "input": "x", "text": {"format": {"type": "json_schema", "name": "s", "schema": {}}}});
        let mut replay = ReplayCache::new();
        assert!(matches!(
            prepare(&body, "s", &mut replay),
            Err(PrepareError::RouteNative(_))
        ));
    }

    #[test]
    fn tools_add_catalog_and_prologue_protocol() {
        let body = json!({
            "model": "m", "input": "hi",
            "tools": [{"type": "function", "name": "shell", "parameters": {"type": "object"}}, {"type": "web_search"}]
        });
        let mut replay = ReplayCache::new();
        let (wire, bridge) = prepare(&body, "s", &mut replay).unwrap();
        let protocol = wire["input"][0]["content"][0]["text"].as_str().unwrap();
        assert!(protocol.contains("Client tool catalog:"));
        assert!(protocol.contains("Client tool \"shell\""));
        assert!(protocol.contains("Hosted tools unavailable through Basispoints: web_search"));
        assert_eq!(bridge.warnings().len(), 1);
    }

    #[test]
    fn instructions_become_leading_developer_message() {
        let body = json!({"model": "m", "input": "hi", "instructions": "be terse"});
        let mut replay = ReplayCache::new();
        let (wire, _) = prepare(&body, "s", &mut replay).unwrap();
        let input = wire["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], json!("developer"));
        assert_eq!(input[0]["content"][0]["text"], json!("be terse"));
        assert_eq!(input[1]["role"], json!("developer")); // protocol
    }

    #[test]
    fn translate_response_translates_tools_and_sets_flags() {
        let body = json!({"model": "m", "input": "hi", "tools": [{"type": "function", "name": "shell", "parameters": {"type": "object"}}]});
        let mut replay = ReplayCache::new();
        let (_, bridge) = prepare(&body, "s", &mut replay).unwrap();
        let mut response = json!({
            "output": [
                {"type": "function_call", "name": "run_officejs", "call_id": "c1", "arguments": {"code": "{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}}"}},
                {"type": "message", "role": "assistant", "content": []}
            ]
        });
        bridge
            .translate_response(&mut response, &mut replay)
            .unwrap();
        assert_eq!(response["output"][0]["type"], json!("function_call"));
        assert_eq!(response["output"][0]["name"], json!("shell"));
        assert_eq!(response["parallel_tool_calls"], json!(false));
        assert_eq!(response["reasoning"]["effort"], json!("medium"));
    }
}
