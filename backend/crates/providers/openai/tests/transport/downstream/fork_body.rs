//! fork：Codex 上游会 400 的 tools 形状（空 namespace、保留命名空间）与缺 role 的 message 项，
//! 经公开编码入口 `encode_generate_request` 验证兼容层就地清洗/补齐。

use gateway_core::operation::{GenerateRequest, ProtocolPayload};
use provider_openai::encode_generate_request;

use super::super::*;

fn normalized_downstream_body(body: Value) -> Map<String, Value> {
    let payload =
        ProtocolPayload::json_object("openai", body.as_object().expect("request object").clone())
            .expect("OpenAI payload");
    encode_generate_request(
        &GenerateRequest::from_protocol_payload(payload),
        "gpt-test",
        None,
    )
    .expect("encode downstream request")
    .body()
    .clone()
}

#[test]
fn encoder_should_drop_empty_namespace_tool() {
    let body = normalized_downstream_body(json!({
        "model": "gpt-test",
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
fn encoder_should_drop_reserved_namespace_function_and_namespace() {
    let body = normalized_downstream_body(json!({
        "model": "gpt-test",
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
fn encoder_should_fill_missing_role_on_message_item() {
    let body = normalized_downstream_body(json!({
        "model": "gpt-test",
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
fn encoder_should_keep_valid_tools_and_namespaces_untouched() {
    let body = normalized_downstream_body(json!({
        "model": "gpt-test",
        "tools": [
            {"type": "function", "name": "update_plan"},
            {"type": "namespace", "name": "functions", "tools": [{"type": "function", "name": "exec"}]},
        ]
    }));
    assert_eq!(body["tools"].as_array().unwrap().len(), 2);
}
