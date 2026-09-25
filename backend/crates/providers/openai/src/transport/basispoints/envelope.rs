//! run_officejs 信封解析与客户端工具项构造（§4）。
//!
//! 只识别声明的中转传输并把结果映射回客户端工具，绝不执行任何代码。

use serde_json::{Value, map::Map};

use super::MAX_ENVELOPE_BYTES;
use super::catalog::{Catalog, CatalogTool, ToolKind, supports_function_code_transport};
use super::error::PrepareError;
use super::replay::ReplayCache;
use super::util::{decode_one, name_has_forbidden_chars, obj, str_field, to_compact};
use super::wire::fingerprint;

const CUSTOM_PREFIX: &str = "codex2api.custom/";
const FUNCTION_CODE_PREFIX: &str = "codex2api.function_code/";

fn invalid(message: impl Into<String>) -> PrepareError {
    PrepareError::invalid(message)
}

fn fp_str(text: &str) -> String {
    fingerprint(&Value::from(text))
}

/// 标记型传输的判定结果。
enum Marker {
    None,
    Ok(Value),
    Err(PrepareError),
}

// ---------------------------------------------------------------------------
// 顶层：翻译一个原生工具调用
// ---------------------------------------------------------------------------

/// 把上游原生工具调用翻译成客户端工具项，并把原生项写入回放缓存。
pub(crate) fn translate_call(
    catalog: &Catalog,
    scope: &str,
    replay: &mut ReplayCache,
    native: &Value,
) -> Result<Value, PrepareError> {
    let name = str_field(native, "name");
    let kind = str_field(native, "type");
    if kind == "function_call" && (name == "update_plan" || name == "functions.update_plan") {
        return super::plan::translate_native_plan(catalog, scope, replay, native);
    }
    if name != "run_officejs" && name != "functions.run_officejs" {
        return translate_direct_catalog_call(catalog, scope, replay, native);
    }
    let arguments = match native.get("arguments") {
        Some(value @ Value::Object(_)) => value.clone(),
        Some(Value::String(raw)) => decode_one(raw)
            .filter(Value::is_object)
            .ok_or_else(|| invalid("basispoints returned invalid tool transport arguments"))?,
        _ => {
            return Err(invalid(
                "basispoints returned invalid tool transport arguments",
            ));
        }
    };
    if !arguments.is_object() {
        return Err(invalid(
            "basispoints returned empty tool transport arguments",
        ));
    }
    let (envelope, marked_custom) = match custom_transport_envelope(&arguments) {
        Marker::Ok(envelope) => (envelope, true),
        Marker::Err(error) => return Err(error),
        Marker::None => match function_code_transport_envelope(catalog, &arguments) {
            Marker::Ok(envelope) => (envelope, false),
            Marker::Err(error) => return Err(error),
            Marker::None => {
                let code = arguments.get("code").cloned().unwrap_or(Value::Null);
                match decode_transport_envelope(&code) {
                    Ok(envelope) => (envelope, false),
                    Err(error) => match recover_transport_envelope(&code, catalog) {
                        Some(envelope) => (envelope, false),
                        None => return Err(error),
                    },
                }
            }
        },
    };
    let tool_name = envelope_name(&envelope)?;
    let info = catalog
        .get(&tool_name)
        .ok_or_else(|| invalid("basispoints returned a tool outside the client's catalog"))?;
    let result = finish_client_tool_call(native, info, &envelope, marked_custom)?;
    replay.put(scope, str_field(native, "call_id"), native, Some(&result));
    Ok(result)
}

/// 模型直接以客户端工具名（可带 `functions.` 前缀）发起的调用。
fn translate_direct_catalog_call(
    catalog: &Catalog,
    scope: &str,
    replay: &mut ReplayCache,
    native: &Value,
) -> Result<Value, PrepareError> {
    let name = str_field(native, "name");
    let info = catalog
        .get(name)
        .or_else(|| name.strip_prefix("functions.").and_then(|n| catalog.get(n)))
        .ok_or_else(|| {
            invalid("basispoints returned an unsupported native tool; no tool was executed")
        })?;
    let kind = str_field(native, "type");
    let envelope = match info.kind {
        ToolKind::Function => {
            if kind != "function_call" {
                return Err(invalid(format!(
                    "basispoints returned client function tool {:?} as a {kind:?}; no tool was executed",
                    info.name
                )));
            }
            obj(vec![
                ("name", Value::from(info.name.clone())),
                (
                    "arguments",
                    native.get("arguments").cloned().unwrap_or(Value::Null),
                ),
            ])
        }
        ToolKind::Custom => {
            if kind != "custom_tool_call" {
                return Err(invalid(format!(
                    "basispoints returned client custom tool {:?} as a {kind:?}; no tool was executed",
                    info.name
                )));
            }
            let Some(input) = native.get("input").and_then(Value::as_str) else {
                return Err(invalid(
                    "basispoints direct custom tool input must be a string",
                ));
            };
            obj(vec![
                ("name", Value::from(info.name.clone())),
                ("input", Value::from(input)),
            ])
        }
    };
    let mut result = finish_client_tool_call(native, info, &envelope, false)?;
    if info.kind == ToolKind::Function
        && let Some(encrypted) = native.get("encrypted_function_args")
        && !encrypted.is_null()
        && let Value::Object(map) = &mut result
    {
        map.insert("encrypted_function_args".to_owned(), encrypted.clone());
    }
    let wrapped = super::history::rebuild_native_history_call(catalog, &result)?;
    replay.put(scope, str_field(native, "call_id"), &wrapped, Some(&result));
    Ok(result)
}

// ---------------------------------------------------------------------------
// 客户端工具项构造
// ---------------------------------------------------------------------------

/// 由已解析的目录工具与信封构造客户端工具项。不缓存、不执行。
pub(crate) fn finish_client_tool_call(
    native: &Value,
    info: &CatalogTool,
    envelope: &Value,
    marked_custom: bool,
) -> Result<Value, PrepareError> {
    if marked_custom && info.kind != ToolKind::Custom {
        return Err(invalid(
            "basispoints raw transport requires a declared custom tool",
        ));
    }
    let call_id = str_field(native, "call_id");
    if call_id.is_empty() {
        return Err(invalid("basispoints tool call is missing call_id"));
    }
    let mut result = Map::new();
    result.insert("type".to_owned(), Value::from(info.kind.call_type()));
    let item_id = {
        let native_id = str_field(native, "id");
        if native_id.is_empty() {
            format!("fc_{}", fp_str(call_id))
        } else {
            native_id.to_owned()
        }
    };
    result.insert("id".to_owned(), Value::from(item_id));
    result.insert("call_id".to_owned(), Value::from(call_id));
    result.insert("name".to_owned(), Value::from(info.name.clone()));
    result.insert("status".to_owned(), Value::from("completed"));
    if !info.namespace.is_empty() {
        result.insert("namespace".to_owned(), Value::from(info.namespace.clone()));
    }
    match info.kind {
        ToolKind::Custom => {
            let has_input = envelope.get("input").is_some();
            let value = match (envelope.get("input"), envelope.get("args")) {
                (_, Some(_)) if has_input => {
                    return Err(invalid(
                        "basispoints custom tool envelope contains conflicting input fields",
                    ));
                }
                (Some(input), _) => input,
                (None, Some(args)) => args,
                (None, None) => &Value::Null,
            };
            if envelope.get("arguments").is_some() {
                return Err(invalid(
                    "basispoints custom tools require input text, not arguments",
                ));
            }
            let Some(input) = value.as_str() else {
                return Err(invalid("basispoints custom tool input must be a string"));
            };
            result.insert(
                "id".to_owned(),
                Value::from(format!("ctc_{}", fp_str(call_id))),
            );
            result.insert("input".to_owned(), Value::from(input));
        }
        ToolKind::Function => {
            let mut args = envelope_arguments(envelope)?;
            if let Value::String(raw) = &args {
                args = decode_one(raw)
                    .ok_or_else(|| invalid("basispoints function arguments are invalid JSON"))?;
            }
            if !args.is_object() {
                return Err(invalid("basispoints function arguments must be an object"));
            }
            let encoded = to_compact(&args)
                .ok_or_else(|| invalid("basispoints function arguments must be an object"))?;
            result.insert("arguments".to_owned(), Value::from(encoded));
            result.insert(
                "encrypted_function_args".to_owned(),
                Value::Array(Vec::new()),
            );
        }
    }
    Ok(Value::Object(result))
}

// ---------------------------------------------------------------------------
// 信封名字 / 参数
// ---------------------------------------------------------------------------

fn envelope_name(envelope: &Value) -> Result<String, PrepareError> {
    let name = str_field(envelope, "name");
    let alias = str_field(envelope, "tool");
    if !name.is_empty() && !alias.is_empty() && name != alias {
        return Err(invalid(
            "basispoints tool envelope contains conflicting names",
        ));
    }
    Ok(if name.is_empty() {
        alias.to_owned()
    } else {
        name.to_owned()
    })
}

fn envelope_arguments(envelope: &Value) -> Result<Value, PrepareError> {
    let args = envelope.get("arguments");
    let alias = envelope.get("args");
    if args.is_some() && alias.is_some() {
        return Err(invalid(
            "basispoints tool envelope contains conflicting argument fields",
        ));
    }
    Ok(args.or(alias).cloned().unwrap_or(Value::Null))
}

// ---------------------------------------------------------------------------
// 标记型传输
// ---------------------------------------------------------------------------

fn custom_transport_envelope(arguments: &Value) -> Marker {
    let Some(summary) = arguments.get("summary").and_then(Value::as_str) else {
        return Marker::None;
    };
    let Some(name) = summary.strip_prefix(CUSTOM_PREFIX) else {
        return Marker::None;
    };
    if name.is_empty() || name_has_forbidden_chars(name) {
        return Marker::Err(invalid(
            "basispoints raw custom transport requires an exact nonempty catalog tool name in summary",
        ));
    }
    let Some(input) = arguments.get("code").and_then(Value::as_str) else {
        return Marker::Err(invalid(
            "basispoints raw custom transport code must be a string",
        ));
    };
    if input.len() > MAX_ENVELOPE_BYTES {
        return Marker::Err(invalid(
            "basispoints raw custom transport code exceeds the size limit",
        ));
    }
    Marker::Ok(obj(vec![
        ("name", Value::from(name)),
        ("input", Value::from(input)),
    ]))
}

fn function_code_transport_envelope(catalog: &Catalog, arguments: &Value) -> Marker {
    let Some(summary) = arguments.get("summary").and_then(Value::as_str) else {
        return Marker::None;
    };
    let Some(name) = summary.strip_prefix(FUNCTION_CODE_PREFIX) else {
        return Marker::None;
    };
    let capable = catalog.get(name).is_some_and(|info| {
        supports_function_code_transport(
            name,
            info.kind == ToolKind::Function,
            info.parameters.as_ref(),
        )
    });
    if !capable {
        return Marker::Err(invalid(
            "basispoints function code transport requires an exact catalog function with a string code parameter",
        ));
    }
    let code = arguments.get("code").and_then(Value::as_str);
    let metadata = arguments.get("extended_summary").and_then(Value::as_str);
    let (Some(code), Some(metadata)) = (code, metadata) else {
        return Marker::Err(invalid(
            "basispoints function code transport requires string code and JSON arguments in extended_summary",
        ));
    };
    if code.len() > MAX_ENVELOPE_BYTES || metadata.len() > MAX_ENVELOPE_BYTES - code.len() {
        return Marker::Err(invalid(
            "basispoints function code transport exceeds the size limit",
        ));
    }
    let Some(Value::Object(mut args)) = decode_one(metadata) else {
        return Marker::Err(invalid(
            "basispoints function code transport extended_summary must contain one JSON object",
        ));
    };
    if args.contains_key("code") {
        return Marker::Err(invalid(
            "basispoints function code transport must not duplicate code in extended_summary",
        ));
    }
    args.insert("code".to_owned(), Value::from(code));
    let value = Value::Object(args);
    match to_compact(&value) {
        Some(encoded) if encoded.len() <= MAX_ENVELOPE_BYTES => {
            Marker::Ok(obj(vec![("name", Value::from(name)), ("arguments", value)]))
        }
        _ => Marker::Err(invalid(
            "basispoints function code transport arguments exceed the size limit",
        )),
    }
}

/// 历史重建时把 FUNCTION_CODE 参数编成外层 run_officejs 信封。
pub(crate) fn encode_function_code_transport(name: &str, args: &Map<String, Value>) -> Value {
    let mut metadata = Map::new();
    for (key, value) in args {
        if key != "code" {
            metadata.insert(key.clone(), value.clone());
        }
    }
    let encoded = to_compact(&Value::Object(metadata)).unwrap_or_else(|| "{}".to_owned());
    obj(vec![
        (
            "summary",
            Value::from(format!("{FUNCTION_CODE_PREFIX}{name}")),
        ),
        ("code", args.get("code").cloned().unwrap_or(Value::Null)),
        ("extended_summary", Value::from(encoded)),
        ("destructive", Value::from(false)),
        ("references", Value::Array(Vec::new())),
    ])
}

// ---------------------------------------------------------------------------
// FUNCTION 信封解码
// ---------------------------------------------------------------------------

/// 解开常见的模型排版而不执行代码；每层必须是一个完整 JSON 值。
fn decode_transport_code(value: &Value) -> Result<Value, PrepareError> {
    let mut current = value.clone();
    for _ in 0..4 {
        if current.is_object() {
            return Ok(current);
        }
        let Some(raw) = current.as_str() else {
            break;
        };
        if raw.len() > MAX_ENVELOPE_BYTES {
            break;
        }
        let raw = raw.trim();
        if raw.is_empty() {
            break;
        }
        if let Some(decoded) = decode_envelope_value(raw) {
            current = decoded;
            continue;
        }
        if let Some(unwrapped) = strip_markdown_fence(raw) {
            current = Value::from(unwrapped);
            continue;
        }
        if let Some(start) = raw.find('{')
            && start > 0
            && prose_prefix(&raw[..start])
            && let Some(decoded) = decode_envelope_value(&raw[start..])
        {
            current = decoded;
            continue;
        }
        break;
    }
    Err(invalid(format!(
        "basispoints tool transport code must contain one JSON client-tool envelope; OfficeJS and multiple calls are unsupported ({})",
        transport_shape(value)
    )))
}

fn strip_markdown_fence(raw: &str) -> Option<&str> {
    let start = raw.find("```")?;
    if !prose_prefix(&raw[..start]) {
        return None;
    }
    let fenced = &raw[start..];
    let newline = fenced.find(char::from(10))?;
    if !fenced.ends_with("```") {
        return None;
    }
    let language = fenced[3..newline].trim();
    if language.is_empty() || language.eq_ignore_ascii_case("json") {
        Some(fenced[newline + 1..fenced.len() - 3].trim())
    } else {
        None
    }
}

/// 解开最多两层嵌套的 run_officejs 信封。
fn decode_transport_envelope(value: &Value) -> Result<Value, PrepareError> {
    let mut current = value.clone();
    for _ in 0..3 {
        let envelope = decode_transport_code(&current)?;
        let name = envelope_name(&envelope)?;
        if name != "run_officejs" && name != "functions.run_officejs" {
            return Ok(envelope);
        }
        let mut args = envelope_arguments(&envelope)?;
        if let Value::String(raw) = &args {
            args = decode_one(raw).filter(Value::is_object).ok_or_else(|| {
                invalid("basispoints nested transport arguments must be one JSON object")
            })?;
        }
        let Value::Object(outer) = &args else {
            return Err(invalid(
                "basispoints nested transport arguments must be an object",
            ));
        };
        current = outer.get("code").cloned().unwrap_or(Value::Null);
    }
    Err(invalid(
        "basispoints tool transport exceeds two nested wrappers",
    ))
}

/// 只接受一个完整的目录调用（单 JSON 字面量参数）。
fn recover_transport_envelope(value: &Value, catalog: &Catalog) -> Option<Value> {
    let raw = value.as_str()?;
    if raw.len() > MAX_ENVELOPE_BYTES {
        return None;
    }
    let raw = raw.trim();
    let Some((matched_name, arg_start)) = match_catalog_invocation(raw) else {
        let envelope = decode_transport_envelope(&Value::from(raw)).ok()?;
        let name = envelope_name(&envelope).ok()?;
        return catalog.get(&name).map(|_| envelope);
    };
    let (name, info) = match catalog.get(&matched_name) {
        Some(info) => (matched_name.clone(), info),
        None => {
            let trimmed = matched_name.strip_prefix("functions.")?;
            (trimmed.to_owned(), catalog.get(trimmed)?)
        }
    };
    let argument = &raw[arg_start..];
    let mut stream = serde_json::Deserializer::from_str(argument).into_iter::<Value>();
    let literal = stream.next()?.ok()?;
    let offset = stream.byte_offset();
    let tail = argument[offset..].trim_start();
    let rest = tail.strip_prefix(')')?.trim();
    if !rest.is_empty() && rest != ";" {
        return None;
    }
    match info.kind {
        ToolKind::Function if literal.is_object() => Some(obj(vec![
            ("name", Value::from(name)),
            ("arguments", literal),
        ])),
        ToolKind::Custom if literal.is_string() => {
            Some(obj(vec![("name", Value::from(name)), ("input", literal)]))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 底层字符串工具
// ---------------------------------------------------------------------------

fn prose_prefix(prefix: &str) -> bool {
    prefix.len() <= 512
        && !prefix
            .chars()
            .any(|c| matches!(c, '{' | '}' | '[' | ']' | '(' | ')' | ';' | '=' | '`' | '"'))
}

fn decode_envelope_value(raw: &str) -> Option<Value> {
    if let Some(value) = decode_one(raw) {
        return Some(value);
    }
    let fixed = repair_transport_json_strings(raw);
    if fixed != raw
        && let Some(value) = decode_one(&fixed)
    {
        return Some(value);
    }
    None
}

/// 修复字符串内的裸换行/回车/制表与非法反斜杠转义，绝不推断缺失的引号、分隔或值。
/// 全程用数字字节码，源码不含反斜杠字面量。
fn repair_transport_json_strings(raw: &str) -> String {
    const BACKSLASH: u8 = 0x5c;
    const NEWLINE: u8 = 0x0a;
    const CARRIAGE: u8 = 0x0d;
    const TAB: u8 = 0x09;
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut quoted = false;
    let mut i = 0usize;
    while i < bytes.len() {
        let ch = bytes[i];
        if ch == b'"' {
            quoted = !quoted;
        }
        if quoted {
            match ch {
                NEWLINE => {
                    out.push(BACKSLASH);
                    out.push(b'n');
                    i += 1;
                    continue;
                }
                CARRIAGE => {
                    out.push(BACKSLASH);
                    out.push(b'r');
                    i += 1;
                    continue;
                }
                TAB => {
                    out.push(BACKSLASH);
                    out.push(b't');
                    i += 1;
                    continue;
                }
                _ => {}
            }
        }
        if ch != BACKSLASH || !quoted || i + 1 >= bytes.len() {
            out.push(ch);
            i += 1;
            continue;
        }
        let next = bytes[i + 1];
        let mut valid = matches!(
            next,
            b'"' | BACKSLASH | b'/' | b'b' | b'f' | b'n' | b'r' | b't'
        );
        if next == b'u' && i + 5 < bytes.len() {
            valid = bytes[i + 2..i + 6].iter().all(u8::is_ascii_hexdigit);
        }
        out.push(BACKSLASH);
        if valid {
            out.push(next);
            i += 2;
        } else {
            out.push(BACKSLASH);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 报告结构性事实，绝不返回客户端代码/提示/参数。
fn transport_shape(value: &Value) -> String {
    let Some(raw) = value.as_str() else {
        return if value.is_null() {
            "format=missing".to_owned()
        } else {
            "format=non_string".to_owned()
        };
    };
    let trimmed = raw.trim();
    let format = if trimmed.is_empty() {
        "empty"
    } else if trimmed.starts_with("```") {
        "markdown"
    } else if trimmed.starts_with('{') {
        "json_object"
    } else if trimmed.starts_with('[') {
        "json_array"
    } else if trimmed.starts_with('"') {
        "json_string"
    } else {
        "text_or_code"
    };
    let mut detail = format!("format={format}; bytes={}", raw.len());
    if let Err(error) = serde_json::from_str::<Value>(raw) {
        let kind = match error.classify() {
            serde_json::error::Category::Eof => "unexpected_eof",
            serde_json::error::Category::Data => "unexpected_token",
            _ => "syntax",
        };
        detail.push_str(&format!("; json_failure={kind}"));
    }
    detail
}

/// 匹配 `^(?:(?:return\s+)?await\s+|return\s+)?<ident>\s*\(`，返回（名字, `(` 之后的下标）。
fn match_catalog_invocation(raw: &str) -> Option<(String, usize)> {
    let bytes = raw.as_bytes();
    let mut pos = consume_prefix(bytes, 0);
    let start = pos;
    if start >= bytes.len() {
        return None;
    }
    let first = bytes[start];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    pos = start + 1;
    while pos < bytes.len() {
        let c = bytes[pos];
        if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'-' {
            pos += 1;
        } else {
            break;
        }
    }
    let name = raw[start..pos].to_owned();
    while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
        pos += 1;
    }
    if pos < bytes.len() && bytes[pos] == b'(' {
        Some((name, pos + 1))
    } else {
        None
    }
}

fn consume_prefix(bytes: &[u8], pos: usize) -> usize {
    if let Some(next) = consume_await_prefix(bytes, pos) {
        return next;
    }
    if let Some(next) = consume_keyword_ws(bytes, pos, b"return") {
        return next;
    }
    pos
}

fn consume_await_prefix(bytes: &[u8], pos: usize) -> Option<usize> {
    let mut p = pos;
    if let Some(next) = consume_keyword_ws(bytes, p, b"return") {
        p = next;
    }
    consume_keyword_ws(bytes, p, b"await")
}

fn consume_keyword_ws(bytes: &[u8], pos: usize, keyword: &[u8]) -> Option<usize> {
    if pos + keyword.len() > bytes.len() || &bytes[pos..pos + keyword.len()] != keyword {
        return None;
    }
    let mut p = pos + keyword.len();
    if p >= bytes.len() || !bytes[p].is_ascii_whitespace() {
        return None;
    }
    while p < bytes.len() && bytes[p].is_ascii_whitespace() {
        p += 1;
    }
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog_with(tools: Value) -> Catalog {
        let mut catalog = Catalog::default();
        super::super::catalog::collect_tools(&mut catalog, Some(&tools), "").unwrap();
        catalog
    }

    fn native_run_officejs(call_id: &str, arguments: Value) -> Value {
        json!({"type": "function_call", "name": "run_officejs", "call_id": call_id, "arguments": arguments})
    }

    #[test]
    fn function_envelope_via_code_string() {
        let catalog = catalog_with(
            json!([{"type": "function", "name": "shell", "parameters": {"type": "object"}}]),
        );
        let mut replay = ReplayCache::new();
        let native = native_run_officejs(
            "c1",
            json!({"code": "{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}}"}),
        );
        let result = translate_call(&catalog, "scope", &mut replay, &native).unwrap();
        assert_eq!(result["type"], json!("function_call"));
        assert_eq!(result["name"], json!("shell"));
        assert_eq!(result["arguments"], json!("{\"command\":\"pwd\"}"));
        assert_eq!(result["encrypted_function_args"], json!([]));
        assert_eq!(result["id"], json!("fc_".to_owned() + &fp_str("c1")));
    }

    #[test]
    fn custom_marker_transport() {
        let catalog = catalog_with(json!([{"type": "custom", "name": "exec"}]));
        let mut replay = ReplayCache::new();
        let native = native_run_officejs(
            "c2",
            json!({"summary": "codex2api.custom/exec", "code": "console.log(1)"}),
        );
        let result = translate_call(&catalog, "scope", &mut replay, &native).unwrap();
        assert_eq!(result["type"], json!("custom_tool_call"));
        assert_eq!(result["name"], json!("exec"));
        assert_eq!(result["input"], json!("console.log(1)"));
        assert_eq!(result["id"], json!("ctc_".to_owned() + &fp_str("c2")));
    }

    #[test]
    fn function_code_marker_merges_extended_summary() {
        let catalog = catalog_with(json!([
            {"type": "function", "name": "apply_patch", "parameters": {"type": "object", "properties": {"code": {"type": "string"}}}}
        ]));
        let mut replay = ReplayCache::new();
        let native = native_run_officejs(
            "c3",
            json!({"summary": "codex2api.function_code/apply_patch", "code": "PATCH", "extended_summary": "{\"path\":\"a\"}"}),
        );
        let result = translate_call(&catalog, "scope", &mut replay, &native).unwrap();
        let args: Value = serde_json::from_str(result["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["code"], json!("PATCH"));
        assert_eq!(args["path"], json!("a"));
    }

    #[test]
    fn direct_catalog_call_relays_unchanged() {
        let catalog = catalog_with(
            json!([{"type": "function", "name": "shell", "parameters": {"type": "object"}}]),
        );
        let mut replay = ReplayCache::new();
        let native = json!({"type": "function_call", "name": "shell", "call_id": "c4", "arguments": {"command": "ls"}});
        let result = translate_call(&catalog, "scope", &mut replay, &native).unwrap();
        assert_eq!(result["name"], json!("shell"));
        assert_eq!(result["arguments"], json!("{\"command\":\"ls\"}"));
        // a subsequent output lookup finds the wrapped native item
        assert!(replay.get_any("scope", "c4").is_some());
    }

    #[test]
    fn recovery_from_single_invocation() {
        let catalog = catalog_with(
            json!([{"type": "function", "name": "shell", "parameters": {"type": "object"}}]),
        );
        let mut replay = ReplayCache::new();
        let native =
            native_run_officejs("c5", json!({"code": "await shell({\"command\":\"pwd\"});"}));
        let result = translate_call(&catalog, "scope", &mut replay, &native).unwrap();
        assert_eq!(result["name"], json!("shell"));
        assert_eq!(result["arguments"], json!("{\"command\":\"pwd\"}"));
    }

    #[test]
    fn markdown_fence_and_repair() {
        let catalog = catalog_with(
            json!([{"type": "function", "name": "shell", "parameters": {"type": "object"}}]),
        );
        let mut replay = ReplayCache::new();
        let fenced = "```json\n{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}}\n```";
        let native = native_run_officejs("c6", json!({ "code": fenced }));
        let result = translate_call(&catalog, "scope", &mut replay, &native).unwrap();
        assert_eq!(result["name"], json!("shell"));
    }

    #[test]
    fn tool_outside_catalog_errors() {
        let catalog = catalog_with(
            json!([{"type": "function", "name": "shell", "parameters": {"type": "object"}}]),
        );
        let mut replay = ReplayCache::new();
        let native =
            native_run_officejs("c7", json!({"code": "{\"name\":\"rm\",\"arguments\":{}}"}));
        let err = translate_call(&catalog, "scope", &mut replay, &native).unwrap_err();
        assert!(format!("{err}").contains("outside the client's catalog"));
    }

    #[test]
    fn unsupported_native_tool_errors() {
        let catalog = catalog_with(
            json!([{"type": "function", "name": "shell", "parameters": {"type": "object"}}]),
        );
        let mut replay = ReplayCache::new();
        let native =
            json!({"type": "function_call", "name": "rm", "call_id": "c8", "arguments": {}});
        let err = translate_call(&catalog, "scope", &mut replay, &native).unwrap_err();
        assert!(format!("{err}").contains("unsupported native tool"));
    }

    #[test]
    fn invocation_matcher() {
        assert_eq!(
            match_catalog_invocation("shell({})").map(|(n, _)| n),
            Some("shell".to_owned())
        );
        assert_eq!(
            match_catalog_invocation("return await fs.read(\"a\")").map(|(n, _)| n),
            Some("fs.read".to_owned())
        );
        assert!(match_catalog_invocation("1shell()").is_none());
    }
}
