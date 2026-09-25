//! 上游 SSE → 客户端 Responses SSE 转换（§3）。
//!
//! 按行分帧，套用事件表，工具项缓冲到 response.completed 终态校验后再发；
//! 首个终态后停止；错误合成 response.failed。所有输出事件重编号 sequence_number。

use serde_json::{Value, json};

use super::MAX_SSE_EVENT_BYTES;
use super::replay::ReplayCache;
use super::request::{Bridge, is_tool};
use super::util::str_field;

const NEWLINE: u8 = 0x0a;
const CARRIAGE: u8 = 0x0d;

fn nl() -> char {
    char::from(NEWLINE)
}

/// 拼一个 SSE 事件块字节。
fn sse_block(kind: &str, data_json: &str) -> Vec<u8> {
    let mut text = String::with_capacity(kind.len() + data_json.len() + 16);
    text.push_str("event: ");
    text.push_str(kind);
    text.push(nl());
    text.push_str("data: ");
    text.push_str(data_json);
    text.push(nl());
    text.push(nl());
    text.into_bytes()
}

/// 上游 SSE 流转换器。由 provider 逐块喂入，产出客户端 SSE 事件块。
pub(crate) struct StreamTranslator<'a> {
    bridge: &'a Bridge,
    sequence: u64,
    pending_tools: Vec<(String, String)>,
    current_event: Option<String>,
    current_data: Vec<String>,
    line: Vec<u8>,
    terminal: bool,
    event_bytes: usize,
}

impl<'a> StreamTranslator<'a> {
    pub(crate) fn new(bridge: &'a Bridge) -> Self {
        Self {
            bridge,
            sequence: 0,
            pending_tools: Vec::new(),
            current_event: None,
            current_data: Vec::new(),
            line: Vec::new(),
            terminal: false,
            event_bytes: 0,
        }
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.terminal
    }

    /// 喂入上游字节块，返回若干完整客户端 SSE 事件块。
    pub(crate) fn feed(&mut self, chunk: &[u8], replay: &mut ReplayCache) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for &byte in chunk {
            if self.terminal {
                break;
            }
            if byte == NEWLINE {
                let line = std::mem::take(&mut self.line);
                let line = strip_cr(line);
                self.consume_line(&line, replay, &mut out);
            } else {
                if self.event_bytes > MAX_SSE_EVENT_BYTES {
                    self.protocol_failed(
                        "basispoints upstream SSE event exceeds the size limit",
                        &mut out,
                    );
                    break;
                }
                self.line.push(byte);
                self.event_bytes += 1;
            }
        }
        out
    }

    /// 上游流结束：未见终态则合成 stream_incomplete。
    pub(crate) fn finish(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        if !self.terminal {
            self.terminal = true;
            let data = json!({
                "response": {
                    "status": "failed",
                    "error": {"code": "basispoints_stream_incomplete", "message": "Upstream stream ended before completion"}
                }
            });
            out.push(self.emit("response.failed", data));
        }
        out
    }

    fn consume_line(&mut self, line: &[u8], replay: &mut ReplayCache, out: &mut Vec<Vec<u8>>) {
        if line.is_empty() {
            // 事件边界
            if !self.current_data.is_empty() {
                let name = self.current_event.take().unwrap_or_default();
                let data = self.current_data.join("\n");
                self.current_data.clear();
                self.event_bytes = 0;
                self.handle_event(&name, &data, replay, out);
            } else {
                self.current_event = None;
                self.event_bytes = 0;
            }
            return;
        }
        let text = String::from_utf8_lossy(line);
        if let Some(rest) = text.strip_prefix("event:") {
            self.current_event = Some(rest.trim().to_owned());
        } else if let Some(rest) = text.strip_prefix("data:") {
            self.current_data
                .push(rest.strip_prefix(' ').unwrap_or(rest).to_owned());
        }
        // 其它行（注释 ": keepalive" 等）忽略。
    }

    fn handle_event(
        &mut self,
        name: &str,
        data: &str,
        replay: &mut ReplayCache,
        out: &mut Vec<Vec<u8>>,
    ) {
        if self.terminal || data.trim() == "[DONE]" {
            return;
        }
        let Ok(payload) = serde_json::from_str::<Value>(data) else {
            self.protocol_failed("basispoints upstream SSE payload is not valid JSON", out);
            return;
        };
        let kind = payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(name)
            .to_owned();

        if kind.starts_with("response.function_call_arguments.")
            || kind.starts_with("response.custom_tool_call_input.")
        {
            return;
        }
        if kind == "response.output_item.added" {
            if event_item_is_tool(&payload) {
                return;
            }
            out.push(self.emit(&kind, payload));
            return;
        }
        if kind == "response.output_item.done" {
            if event_item_is_tool(&payload) {
                if self.pending_tools.len() < super::MAX_PENDING_TOOL_ITEMS
                    && let Some(item) = payload.get("item")
                {
                    self.pending_tools.push((
                        str_field(item, "call_id").to_owned(),
                        str_field(item, "id").to_owned(),
                    ));
                }
                return;
            }
            out.push(self.emit(&kind, payload));
            return;
        }
        if kind == "response.completed" {
            self.handle_completed(payload, replay, out);
            return;
        }
        if matches!(
            kind.as_str(),
            "response.incomplete" | "response.failed" | "error"
        ) {
            let mut payload = payload;
            strip_tool_items(&mut payload);
            out.push(self.emit(&kind, payload));
            self.terminal = true;
            return;
        }
        if payload.get("response").is_some() {
            let mut payload = payload;
            strip_tool_items(&mut payload);
            if let Some(response) = payload.get_mut("response").and_then(Value::as_object_mut) {
                response.insert(
                    "reasoning".to_owned(),
                    json!({"effort": self.bridge.effort()}),
                );
            }
            out.push(self.emit(&kind, payload));
            return;
        }
        out.push(self.emit(&kind, payload));
    }

    fn handle_completed(
        &mut self,
        mut payload: Value,
        replay: &mut ReplayCache,
        out: &mut Vec<Vec<u8>>,
    ) {
        self.terminal = true;
        // pending (call_id,id) 必须都在 output 工具项中
        let present: Vec<(String, String)> = payload
            .get("response")
            .and_then(|r| r.get("output"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter(|item| is_tool(item))
                    .map(|item| {
                        (
                            str_field(item, "call_id").to_owned(),
                            str_field(item, "id").to_owned(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        for pending in &self.pending_tools {
            if !present.iter().any(|item| item == pending) {
                self.protocol_failed(
                    "basispoints completed response omitted an original tool item",
                    out,
                );
                return;
            }
        }
        // 翻译 output 工具项 + reasoning/parallel
        if let Some(response) = payload.get_mut("response")
            && let Err(error) = self.bridge.translate_response(response, replay)
        {
            self.protocol_failed(&error.to_string(), out);
            return;
        }
        // 每个翻译后的工具项发四事件（按 id 去重）
        let tool_items: Vec<(usize, Value)> = payload
            .get("response")
            .and_then(|r| r.get("output"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| is_tool(item))
                    .map(|(index, item)| (index, item.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let mut seen_ids: Vec<String> = Vec::new();
        for (index, item) in tool_items {
            let id = str_field(&item, "id").to_owned();
            if seen_ids.contains(&id) {
                continue;
            }
            seen_ids.push(id.clone());
            for event in self.tool_events(index, &item) {
                out.push(event);
            }
        }
        out.push(self.emit("response.completed", payload));
    }

    fn tool_events(&mut self, output_index: usize, item: &Value) -> Vec<Vec<u8>> {
        let is_custom = str_field(item, "type") == "custom_tool_call";
        let (payload_field, delta_kind) = if is_custom {
            ("input", "response.custom_tool_call_input.delta")
        } else {
            ("arguments", "response.function_call_arguments.delta")
        };
        let done_kind = if is_custom {
            "response.custom_tool_call_input.done"
        } else {
            "response.function_call_arguments.done"
        };
        let item_id = str_field(item, "id").to_owned();
        let full = item.get(payload_field).cloned().unwrap_or(Value::from(""));

        // added：payload 字段清空、status=in_progress
        let mut added_item = item.clone();
        if let Value::Object(map) = &mut added_item {
            map.insert(payload_field.to_owned(), Value::from(""));
            map.insert("status".to_owned(), Value::from("in_progress"));
        }
        let mut events = Vec::with_capacity(4);
        events.push(self.emit(
            "response.output_item.added",
            json!({"output_index": output_index, "item": added_item}),
        ));
        events.push(self.emit(
            delta_kind,
            json!({"output_index": output_index, "item_id": item_id, "delta": full}),
        ));
        events.push(self.emit(
            done_kind,
            json!({"output_index": output_index, "item_id": item_id, payload_field: full}),
        ));
        events.push(self.emit(
            "response.output_item.done",
            json!({"output_index": output_index, "item": item}),
        ));
        events
    }

    fn protocol_failed(&mut self, message: &str, out: &mut Vec<Vec<u8>>) {
        self.terminal = true;
        let data = json!({
            "response": {
                "status": "failed",
                "output": [],
                "error": {"code": "basispoints_protocol_error", "message": message}
            }
        });
        out.push(self.emit("response.failed", data));
    }

    fn emit(&mut self, kind: &str, mut data: Value) -> Vec<u8> {
        if let Value::Object(map) = &mut data {
            map.insert("type".to_owned(), Value::from(kind));
            map.insert("sequence_number".to_owned(), Value::from(self.sequence));
        }
        self.sequence += 1;
        let json = serde_json::to_string(&data).unwrap_or_else(|_| "{}".to_owned());
        sse_block(kind, &json)
    }
}

fn strip_cr(mut line: Vec<u8>) -> Vec<u8> {
    if line.last() == Some(&CARRIAGE) {
        line.pop();
    }
    line
}

fn event_item_is_tool(payload: &Value) -> bool {
    payload.get("item").is_some_and(is_tool)
}

fn strip_tool_items(payload: &mut Value) {
    if let Some(output) = payload
        .get_mut("response")
        .and_then(|response| response.get_mut("output"))
        .and_then(Value::as_array_mut)
    {
        output.retain(|item| !is_tool(item));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::basispoints::request::prepare;
    use serde_json::json;

    fn bridge_with_tools() -> (Bridge, ReplayCache) {
        let mut replay = ReplayCache::new();
        let body = json!({"model": "m", "input": "hi", "tools": [{"type": "function", "name": "shell", "parameters": {"type": "object"}}]});
        let (_, bridge) = prepare(&body, "scope", &mut replay).unwrap();
        (bridge, replay)
    }

    fn decode(blocks: &[Vec<u8>]) -> Vec<(String, Value)> {
        blocks
            .iter()
            .map(|block| {
                let text = String::from_utf8_lossy(block);
                let mut event = String::new();
                let mut data = String::new();
                for line in text.lines() {
                    if let Some(rest) = line.strip_prefix("event: ") {
                        event = rest.to_owned();
                    } else if let Some(rest) = line.strip_prefix("data: ") {
                        data = rest.to_owned();
                    }
                }
                (event, serde_json::from_str(&data).unwrap_or(Value::Null))
            })
            .collect()
    }

    #[test]
    fn text_events_pass_through_with_renumbering() {
        let (bridge, mut replay) = bridge_with_tools();
        let mut translator = StreamTranslator::new(&bridge);
        let upstream = "event: response.created\ndata: {\"type\":\"response.created\"}\n\nevent: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n";
        let out = translator.feed(upstream.as_bytes(), &mut replay);
        let events = decode(&out);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, "response.created");
        assert_eq!(events[0].1["sequence_number"], json!(0));
        assert_eq!(events[1].1["sequence_number"], json!(1));
        assert_eq!(events[1].1["delta"], json!("hi"));
    }

    #[test]
    fn tool_call_full_cycle() {
        let (bridge, mut replay) = bridge_with_tools();
        let mut translator = StreamTranslator::new(&bridge);
        // upstream drops the run_officejs tool item events, then completed carries it in output
        let completed = json!({
            "type": "response.completed",
            "response": {"output": [
                {"type": "function_call", "name": "run_officejs", "call_id": "c1", "id": "fc_1", "arguments": "{\"code\":\"{\\\"name\\\":\\\"shell\\\",\\\"arguments\\\":{\\\"command\\\":\\\"pwd\\\"}}\"}"}
            ]}
        });
        let added = json!({"type": "response.output_item.done", "item": {"type": "function_call", "name": "run_officejs", "call_id": "c1", "id": "fc_1"}});
        let mut stream = String::new();
        stream.push_str("data: ");
        stream.push_str(&serde_json::to_string(&added).unwrap());
        stream.push('\n');
        stream.push('\n');
        stream.push_str("data: ");
        stream.push_str(&serde_json::to_string(&completed).unwrap());
        stream.push('\n');
        stream.push('\n');
        let out = translator.feed(stream.as_bytes(), &mut replay);
        let events = decode(&out);
        // 4 tool events + completed
        assert_eq!(events.len(), 5);
        assert_eq!(events[0].0, "response.output_item.added");
        assert_eq!(events[0].1["item"]["name"], json!("shell"));
        assert_eq!(events[0].1["item"]["arguments"], json!(""));
        assert_eq!(events[1].0, "response.function_call_arguments.delta");
        assert_eq!(events[1].1["delta"], json!("{\"command\":\"pwd\"}"));
        assert_eq!(events[2].0, "response.function_call_arguments.done");
        assert_eq!(events[3].0, "response.output_item.done");
        assert_eq!(events[3].1["item"]["name"], json!("shell"));
        assert_eq!(events[4].0, "response.completed");
        assert_eq!(events[4].1["response"]["parallel_tool_calls"], json!(false));
        assert!(translator.is_terminal());
    }

    #[test]
    fn missing_pending_tool_item_fails() {
        let (bridge, mut replay) = bridge_with_tools();
        let mut translator = StreamTranslator::new(&bridge);
        let done = json!({"type": "response.output_item.done", "item": {"type": "function_call", "name": "run_officejs", "call_id": "c9", "id": "fc_9"}});
        let completed = json!({"type": "response.completed", "response": {"output": []}});
        let mut stream = String::new();
        for value in [&done, &completed] {
            stream.push_str("data: ");
            stream.push_str(&serde_json::to_string(value).unwrap());
            stream.push('\n');
            stream.push('\n');
        }
        let out = translator.feed(stream.as_bytes(), &mut replay);
        let events = decode(&out);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "response.failed");
        assert_eq!(
            events[0].1["response"]["error"]["code"],
            json!("basispoints_protocol_error")
        );
    }

    #[test]
    fn eof_before_terminal_synthesizes_incomplete() {
        let (bridge, mut replay) = bridge_with_tools();
        let mut translator = StreamTranslator::new(&bridge);
        let _ = translator.feed(
            b"event: response.created\ndata: {\"type\":\"response.created\"}\n\n",
            &mut replay,
        );
        let out = translator.finish();
        let events = decode(&out);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].1["response"]["error"]["code"],
            json!("basispoints_stream_incomplete")
        );
    }

    #[test]
    fn done_marker_and_keepalive_ignored() {
        let (bridge, mut replay) = bridge_with_tools();
        let mut translator = StreamTranslator::new(&bridge);
        let out = translator.feed(b": keepalive\n\ndata: [DONE]\n\n", &mut replay);
        assert!(out.is_empty());
    }
}
