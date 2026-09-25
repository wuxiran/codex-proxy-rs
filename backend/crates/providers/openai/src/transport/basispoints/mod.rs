//! Excel / Basispoints (BPS) 上游协议转换。
//!
//! BPS 是 ChatGPT Excel 插件网关（`https://bps.openai.com/basispoints/api/responses`）。
//! 它用账号现成的 ChatGPT OAuth access token，唯一的上游工具是 `run_officejs`，
//! 客户端工具被塞进它的信封再解析回来。本模块只做协议转换（纯函数），
//! 传输、账号选择、计费与错误归属仍由 provider 层负责。
//!
//! 来源：移植自 hloolx/codex2api（MIT），参照 ranxi2001/sub2api 的 Go 实现。
//! 详见 `README.md`。

// TODO(bps P2): 传输接入后移除；协议核心尚未被 provider 调用。
#![allow(dead_code, unused_imports)]

mod wire;

pub(crate) use wire::{NormalizeEffortError, fingerprint, normalize_effort};

/// BPS 上游端点。可在测试中被覆盖。
pub(crate) const BASISPOINTS_RESPONSES_URL: &str =
    "https://bps.openai.com/basispoints/api/responses";

/// SSE 单行 / 单事件上限（分帧与解析共用）。
pub(crate) const MAX_SSE_EVENT_BYTES: usize = 16 * 1024 * 1024;
/// 信封 / code 字符串上限。
pub(crate) const MAX_ENVELOPE_BYTES: usize = 1024 * 1024;
/// 单个响应内最多缓冲的待发工具项数。
pub(crate) const MAX_PENDING_TOOL_ITEMS: usize = 1024;
