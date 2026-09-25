//! 客户端工具目录：collect_tools / describe_catalog / describe_schema。

use std::collections::HashMap;

use serde_json::{Value, map::Map};

use super::error::PrepareError;
use super::util::{name_has_forbidden_chars, str_field};
use super::wire::fingerprint;

/// 工具种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolKind {
    Function,
    Custom,
}

impl ToolKind {
    /// 客户端项的 type 前缀（function_call / custom_tool_call）。
    pub(crate) fn call_type(self) -> &'static str {
        match self {
            Self::Function => "function_call",
            Self::Custom => "custom_tool_call",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Custom => "custom",
        }
    }
}

/// 目录里的一个工具。
#[derive(Debug, Clone)]
pub(crate) struct CatalogTool {
    pub(crate) name: String,
    pub(crate) namespace: String,
    pub(crate) kind: ToolKind,
    pub(crate) parameters: Option<Value>,
    pub(crate) definition: String,
}

/// 客户端工具目录。
#[derive(Debug, Default)]
pub(crate) struct Catalog {
    /// 限定名 → 工具。
    pub(crate) tools: HashMap<String, CatalogTool>,
    /// describe 用的条目对象，保持插入顺序。
    pub(crate) entries: Vec<Value>,
    /// 被丢弃的 hosted 工具种类（去重、告警时排序）。
    pub(crate) unsupported: Vec<String>,
}

impl Catalog {
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn get(&self, qualified: &str) -> Option<&CatalogTool> {
        self.tools.get(qualified)
    }
}

fn is_unsupported_hosted_tool(kind: &str) -> bool {
    matches!(
        kind,
        "web_search"
            | "web_search_preview"
            | "web_search_preview_2025_03_11"
            | "web_search_2025_08_26"
            | "tool_search"
            | "image_generation"
            | "file_search"
            | "code_interpreter"
            | "computer"
            | "computer_use_preview"
            | "mcp"
    )
}

/// 递归收集工具（namespace 展开），追加进 catalog。
pub(crate) fn collect_tools(
    catalog: &mut Catalog,
    value: Option<&Value>,
    namespace: &str,
) -> Result<(), PrepareError> {
    let Some(Value::Array(items)) = value else {
        return Ok(());
    };
    for raw in items {
        let Value::Object(item) = raw else {
            return Err(PrepareError::invalid("invalid Basispoints client tool"));
        };
        let kind = str_field(raw, "type");
        let name = str_field(raw, "name");
        if kind == "namespace" {
            if name.is_empty() {
                return Err(PrepareError::invalid(
                    "basispoints client namespaces require a name",
                ));
            }
            let nested_namespace = if namespace.is_empty() {
                name.to_owned()
            } else {
                format!("{namespace}.{name}")
            };
            collect_tools(catalog, item.get("tools"), &nested_namespace)?;
            continue;
        }
        if is_unsupported_hosted_tool(kind) {
            if !catalog.unsupported.iter().any(|k| k == kind) {
                catalog.unsupported.push(kind.to_owned());
            }
            continue;
        }
        let tool_kind = match kind {
            "function" => ToolKind::Function,
            "custom" => ToolKind::Custom,
            _ => {
                return Err(PrepareError::invalid(format!(
                    "basispoints does not support hosted tool {kind:?}; use client function or custom tools"
                )));
            }
        };
        if name.is_empty() {
            return Err(PrepareError::invalid(
                "basispoints client tools require a name",
            ));
        }
        let qualified = if namespace.is_empty() {
            name.to_owned()
        } else {
            format!("{namespace}.{name}")
        };
        let mut entry = Map::new();
        entry.insert("type".to_owned(), Value::from(kind));
        entry.insert("name".to_owned(), Value::from(qualified.clone()));
        for field in ["description", "format", "parameters"] {
            if let Some(v) = item.get(field) {
                entry.insert(field.to_owned(), v.clone());
            }
        }
        if tool_kind == ToolKind::Function && !entry.contains_key("parameters") {
            if let Some(schema) = item.get("inputSchema").or_else(|| item.get("input_schema")) {
                entry.insert("parameters".to_owned(), schema.clone());
            }
        }
        let definition = fingerprint(raw);
        if let Some(previous) = catalog.tools.get(&qualified) {
            if previous.definition != definition
                || previous.namespace != namespace
                || previous.name != name
            {
                return Err(PrepareError::invalid(format!(
                    "conflicting duplicate Basispoints client tool {qualified:?}"
                )));
            }
            continue;
        }
        let parameters = entry
            .get("parameters")
            .filter(|value| value.is_object())
            .cloned();
        catalog.tools.insert(
            qualified.clone(),
            CatalogTool {
                name: name.to_owned(),
                namespace: namespace.to_owned(),
                kind: tool_kind,
                parameters,
                definition,
            },
        );
        catalog.entries.push(Value::Object(entry));
    }
    Ok(())
}

/// 某函数是否走 FUNCTION_CODE 传输：函数、名字合法、schema 为 object 且 properties.code.type==string。
pub(crate) fn supports_function_code_transport(
    name: &str,
    is_function: bool,
    parameters: Option<&Value>,
) -> bool {
    if !is_function || name.is_empty() || name_has_forbidden_chars(name) {
        return false;
    }
    let Some(schema) = parameters.and_then(Value::as_object) else {
        return false;
    };
    let code_type = schema
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|properties| properties.get("code"))
        .and_then(Value::as_object)
        .and_then(|code| code.get("type"))
        .and_then(Value::as_str);
    schema.get("type").and_then(Value::as_str) == Some("object") && code_type == Some("string")
}

/// serde_json 字符串编码（不做 HTML 转义；对齐 Go 的 json.Marshal 仅在此点有差异，纯提示文本）。
pub(crate) fn quoted(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned())
}

fn quoted_str(text: &str) -> String {
    quoted(&Value::from(text))
}

/// 生成目录说明文本（§1.8）。
pub(crate) fn describe_catalog(entries: &[Value]) -> String {
    let mut lines = Vec::with_capacity(entries.len());
    for entry in entries {
        let name = str_field(entry, "name");
        let kind = str_field(entry, "type");
        let mut line = format!("Client tool {} ({}).", quoted_str(name), kind);
        let description = str_field(entry, "description");
        if !description.is_empty() {
            line.push(' ');
            line.push_str(description);
        }
        if kind == "custom" {
            line.push_str(&format!(
                " Set run_officejs summary to {} and pass its exact raw text directly in code.",
                quoted_str(&format!("codex2api.custom/{name}"))
            ));
            if let Some(format) = entry.get("format") {
                line.push_str(&format!(" Input format: {}.", quoted(format)));
            }
        } else {
            let parameters = entry.get("parameters");
            if supports_function_code_transport(name, kind == "function", parameters) {
                line.push_str(&format!(
                    " Use FUNCTION_CODE transport: set run_officejs summary to {}. Put the exact code argument directly in native code. Put all other supplied arguments in one JSON object in extended_summary, using only fields declared in the contract; use {{}} when there are none. Do not include code in that object.",
                    quoted_str(&format!("codex2api.function_code/{name}"))
                ));
            } else {
                line.push_str(" Pass a JSON object in the envelope's arguments field.");
            }
            line.push_str(" Argument contract: ");
            line.push_str(&describe_schema(parameters, 0));
        }
        lines.push(line);
    }
    lines.join("\n\n")
}

/// 生成参数 schema 描述（深度 ≤8）。
pub(crate) fn describe_schema(value: Option<&Value>, depth: usize) -> String {
    let schema = match value {
        Some(Value::Object(map)) if depth < 8 => map,
        None => return "Use the arguments described by the tool.".to_owned(),
        Some(other) => return quoted(other),
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(kind) = schema.get("type") {
        parts.push(format!("Value type: {}.", quoted(kind)));
    }
    let description = schema.get("description").and_then(Value::as_str).unwrap_or("");
    if !description.is_empty() {
        parts.push(description.to_owned());
    }
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        let mut names: Vec<&String> = properties.keys().collect();
        names.sort_unstable();
        for name in names {
            let presence = if required.contains(&name.as_str()) {
                "required"
            } else {
                "optional"
            };
            parts.push(format!(
                "Field {} ({}): {}",
                quoted_str(name),
                presence,
                describe_schema(properties.get(name), depth + 1)
            ));
        }
    }
    if let Some(items) = schema.get("items") {
        parts.push(format!(
            "Each array item: {}",
            describe_schema(Some(items), depth + 1)
        ));
    }
    let mut constraints = Map::new();
    for (key, value) in schema {
        if !matches!(key.as_str(), "type" | "description" | "properties" | "items") {
            constraints.insert(key.clone(), value.clone());
        }
    }
    if !constraints.is_empty() {
        parts.push(format!(
            "Additional constraints: {}.",
            quoted(&Value::Object(constraints))
        ));
    }
    if parts.is_empty() {
        return "Any JSON value.".to_owned();
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn collects_functions_customs_and_namespaces() {
        let tools = json!([
            {"type": "function", "name": "shell", "parameters": {"type": "object", "properties": {"cmd": {"type": "string"}}, "required": ["cmd"]}},
            {"type": "custom", "name": "exec", "format": "raw"},
            {"type": "namespace", "name": "fs", "tools": [
                {"type": "function", "name": "read", "parameters": {"type": "object"}}
            ]},
            {"type": "web_search"}
        ]);
        let mut catalog = Catalog::default();
        collect_tools(&mut catalog, Some(&tools), "").unwrap();
        assert!(catalog.get("shell").is_some());
        assert!(catalog.get("exec").is_some());
        assert_eq!(catalog.get("fs.read").map(|t| t.name.as_str()), Some("read"));
        assert_eq!(catalog.get("fs.read").map(|t| t.namespace.as_str()), Some("fs"));
        assert_eq!(catalog.unsupported, vec!["web_search".to_owned()]);
        assert_eq!(catalog.entries.len(), 3);
    }

    #[test]
    fn duplicate_same_definition_dedups_but_conflicting_errors() {
        let same = json!([
            {"type": "function", "name": "a", "parameters": {"type": "object"}},
            {"type": "function", "name": "a", "parameters": {"type": "object"}}
        ]);
        let mut catalog = Catalog::default();
        collect_tools(&mut catalog, Some(&same), "").unwrap();
        assert_eq!(catalog.entries.len(), 1);

        let conflicting = json!([
            {"type": "function", "name": "a", "parameters": {"type": "object"}},
            {"type": "function", "name": "a", "parameters": {"type": "string"}}
        ]);
        let mut catalog2 = Catalog::default();
        assert!(collect_tools(&mut catalog2, Some(&conflicting), "").is_err());
    }

    #[test]
    fn hosted_forced_type_errors() {
        let bad = json!([{"type": "reasoning_tool", "name": "x"}]);
        let mut catalog = Catalog::default();
        assert!(collect_tools(&mut catalog, Some(&bad), "").is_err());
    }

    #[test]
    fn function_code_detection() {
        let params = json!({"type": "object", "properties": {"code": {"type": "string"}}});
        assert!(supports_function_code_transport("apply_patch", true, Some(&params)));
        assert!(!supports_function_code_transport("a/b", true, Some(&params)));
        let no_code = json!({"type": "object", "properties": {"x": {"type": "string"}}});
        assert!(!supports_function_code_transport("f", true, Some(&no_code)));
    }

    #[test]
    fn describe_schema_shapes() {
        assert_eq!(
            describe_schema(None, 0),
            "Use the arguments described by the tool."
        );
        let schema = json!({"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]});
        let text = describe_schema(Some(&schema), 0);
        assert!(text.contains("Value type: \"object\"."));
        assert!(text.contains("Field \"a\" (required):"));
    }

    #[test]
    fn describe_catalog_mentions_transports() {
        let entries = vec![
            json!({"type": "custom", "name": "exec"}),
            json!({"type": "function", "name": "apply_patch", "parameters": {"type": "object", "properties": {"code": {"type": "string"}}}}),
            json!({"type": "function", "name": "plain", "parameters": {"type": "object"}}),
        ];
        let text = describe_catalog(&entries);
        assert!(text.contains("codex2api.custom/exec"));
        assert!(text.contains("codex2api.function_code/apply_patch"));
        assert!(text.contains("Pass a JSON object in the envelope's arguments field."));
    }
}
