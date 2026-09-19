//! 已签名号池的 401 自动复活：归档导入原文，失效后提交 revive-api 并写回 AT/RT。

mod client;
mod detect;
mod service;
mod store;

pub use service::{CodexReviveCycleSummary, CodexReviveError, CodexReviveService};
