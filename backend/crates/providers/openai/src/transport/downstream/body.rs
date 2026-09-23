//! 下游客户端模型请求正文的 Codex 协议兼容。

use serde_json::{Map, Value, json};

/// 非官方客户端及跨客户端历史回填的兼容入口；调用方必须已选定 Codex/OAuth 上游。
/// 只处理已验证被拒绝的字段，不根据客户端品牌推断整份历史是否合法。
pub(crate) fn normalize_non_codex_request_body(body: &mut Map<String, Value>) {
    let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };
    for item in input {
        let Some(item) = item.as_object_mut() else {
            continue;
        };
        if item.get("type").and_then(Value::as_str) != Some("reasoning") {
            continue;
        }
        // SDK 输出项的 status 不属于 Codex reasoning 输入合同；其他项的 status 可能合法。
        item.shift_remove("status");
        // Codex 只接受空 content 数组；仅在加密历史仍可回填时去掉冗余明文，
        // 没有加密内容的历史不自动丢弃，保留给上游明确拒绝。
        if item
            .get("encrypted_content")
            .and_then(Value::as_str)
            .is_some_and(|content| !content.trim().is_empty())
            && item
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(|content| !content.is_empty())
        {
            item.shift_remove("content");
        }
    }
}

/// 补齐 Codex 请求缺省字段并适配已确认不兼容的请求形状，不递归清洗业务正文。
///
/// 兼容基准是 Codex Core/Desktop 的模型请求，不是公开 OpenAI Responses API。
/// 未知字段继续透传，不能因官方请求结构中没有某个字段就将其列入过滤规则。
pub(in crate::transport) fn normalize_codex_request_body(body: &mut Map<String, Value>) {
    // 官方 Core/Desktop 显式发送 store=false；仅为缺字段的下游请求补齐，保留显式值。
    body.entry("store").or_insert(Value::Bool(false));

    // 公开 Responses API 允许 `input` 为字符串（等价于一条 user 文本消息），
    // 而 Codex 后端只接受条目数组，否则返回 400 "Input must be a list"。
    // 这里按官方 ResponseItem::Message 的形状展开；其他非数组类型不猜测语义，交给上游判定。
    if let Some(Value::String(text)) = body.get_mut("input") {
        let text = std::mem::take(text);
        body.insert(
            "input".to_owned(),
            json!([{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": text}],
            }]),
        );
    }

    // Codex 上游拒绝显式 message 的 system role；沿用官方客户端的 developer
    // role 承载指令，只转换已确认的消息形状，保留内容与其他字段。
    if let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) {
        for item in input {
            let Some(item) = item.as_object_mut() else {
                continue;
            };
            if item.get("type").and_then(Value::as_str) == Some("message") {
                match item.get("role").and_then(Value::as_str) {
                    Some("system") => {
                        item.insert("role".to_owned(), Value::String("developer".to_owned()));
                    }
                    // Codex 后端要求 message 项必须带 role，缺失会返回 400
                    // "Missing required parameter: 'input[N].role'"。补默认 user。
                    None => {
                        item.insert("role".to_owned(), Value::String("user".to_owned()));
                    }
                    _ => {}
                }
            }
        }
    }

    // Codex 上游对 tools 形状有硬约束，以下均为已确认会 400 的形状，按兼容层职责就地清洗：
    // 保留命名空间由上游内建（browser/container/python），下游不得在其中声明工具。
    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
        const RESERVED_NAMESPACES: [&str; 3] = ["browser", "container", "python"];
        tools.retain(|tool| {
            let Some(tool) = tool.as_object() else {
                return true; // 非对象结构原样保留，交上游判定
            };
            if tool.get("type").and_then(Value::as_str) == Some("namespace") {
                // 空 namespace（子 tools 为空/缺失）触发 "empty array" 400；命名空间名撞保留区也丢弃。
                let non_empty = tool
                    .get("tools")
                    .and_then(Value::as_array)
                    .is_some_and(|children| !children.is_empty());
                let name = tool.get("name").and_then(Value::as_str).unwrap_or("");
                return non_empty && !RESERVED_NAMESPACES.contains(&name);
            }
            // 函数工具名形如 `browser.xxx` 撞保留命名空间，触发 reserved 400，丢弃。
            if let Some(name) = tool.get("name").and_then(Value::as_str) {
                if let Some((prefix, _)) = name.split_once('.') {
                    if RESERVED_NAMESPACES.contains(&prefix) {
                        return false;
                    }
                }
            }
            true
        });
    }

    for field in [
        // Pi 普通 Responses 适配将 maxTokens 映射为 max_output_tokens，
        // temperature 则原样写入；Pi 的 Codex 适配也可能发送 temperature。
        "max_output_tokens",
        "temperature",
        // Pi 开启长缓存时发送 24h；保留有效的 prompt_cache_key，
        // 只剥离 Codex Responses 明确拒绝的缓存保留时长参数。
        "prompt_cache_retention",
    ] {
        body.remove(field);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalize(value: Value) -> Map<String, Value> {
        let mut body = value.as_object().unwrap().clone();
        normalize_codex_request_body(&mut body);
        body
    }

    #[test]
    fn drops_empty_namespace_tool() {
        let body = normalize(json!({
            "tools": [
                {"type": "function", "name": "shell"},
                {"type": "namespace", "name": "functions", "tools": []},
            ]
        }));
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "shell");
    }

    #[test]
    fn drops_reserved_namespace_function_and_namespace() {
        let body = normalize(json!({
            "tools": [
                {"type": "function", "name": "browser.open_url"},
                {"type": "namespace", "name": "browser", "tools": [{"type": "function", "name": "x"}]},
                {"type": "function", "name": "shell_command"},
            ]
        }));
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "shell_command");
    }

    #[test]
    fn fills_missing_role_on_message_item() {
        let body = normalize(json!({
            "input": [
                {"type": "message", "content": [{"type": "input_text", "text": "hi"}]},
                {"type": "message", "role": "assistant", "content": []},
            ]
        }));
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[1]["role"], "assistant");
    }

    #[test]
    fn keeps_valid_tools_and_namespaces_untouched() {
        let body = normalize(json!({
            "tools": [
                {"type": "function", "name": "update_plan"},
                {"type": "namespace", "name": "functions", "tools": [{"type": "function", "name": "exec"}]},
            ]
        }));
        assert_eq!(body["tools"].as_array().unwrap().len(), 2);
    }
}
