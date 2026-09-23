//! 请求日志环形缓冲（进程内、有界、诊断用）。
//!
//! 统一 cookie 库 / turn-state 票 / 满血-降智 的逐请求观测数据写在这里，管理端
//! 「请求日志」菜单读取。只存短标识/短哈希，**绝不存 cookie 或票据原文**。
//! 有硬上限，满了丢最旧，永不撑爆内存。

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;

/// 最多保留多少条最近请求。
const CAP: usize = 300;

/// 一条请求观测记录。字段都是短标识，不含机密。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestLogRecord {
    /// 记录时刻（Unix 毫秒），前端本地格式化。
    pub at_ms: i64,
    /// 请求模型，如 gpt-6-sol。
    pub model: String,
    /// cookie 决策：reuse(沿用自己的) / inject(从统一池借) / none(无 cf_bm)。
    pub cookie_action: String,
    /// 出口指纹短标识（不泄露代理原文）。
    pub egress: String,
    /// 本次使用的统一 cookie 短标识（__cf_bm 的短哈希），如 unified-88。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unified: Option<String>,
    /// 使用的 turn-state 票短指纹（非原文），如 #hPdTPIqq。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket_in: Option<String>,
}

fn buffer() -> &'static Mutex<VecDeque<RequestLogRecord>> {
    static BUF: OnceLock<Mutex<VecDeque<RequestLogRecord>>> = OnceLock::new();
    BUF.get_or_init(|| Mutex::new(VecDeque::with_capacity(CAP)))
}

/// 记录一条；满了丢最旧。锁毒化时静默跳过（诊断数据不阻塞主流程）。
pub fn record(rec: RequestLogRecord) {
    if let Ok(mut buf) = buffer().lock() {
        if buf.len() >= CAP {
            buf.pop_front();
        }
        buf.push_back(rec);
    }
}

/// 取最近 `limit` 条，最新在前。
#[must_use]
pub fn recent(limit: usize) -> Vec<RequestLogRecord> {
    buffer().lock().map_or_else(
        |_| Vec::new(),
        |buf| buf.iter().rev().take(limit).cloned().collect(),
    )
}

/// 当前 Unix 毫秒（记录时刻用）。
#[must_use]
pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// 把一个不透明值折成短显示标识（非可逆、仅用于日志展示，绝不回放原文）。
#[must_use]
pub fn short_label(value: &str, prefix: &str) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut h);
    format!("{prefix}-{}", h.finish() % 1000)
}

/// 把票据原文折成短指纹用于展示（非可逆），如 `#a1f30c`。
#[must_use]
pub fn fingerprint(value: &str) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut h);
    format!("#{:06x}", h.finish() & 0x00ff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_is_bounded_and_newest_first() {
        for i in 0..(CAP + 50) {
            record(RequestLogRecord {
                at_ms: i as i64,
                model: "gpt-6-sol".into(),
                cookie_action: "reuse".into(),
                egress: "e".into(),
                unified: None,
                ticket_in: None,
            });
        }
        let recent = recent(1000);
        assert!(recent.len() <= CAP, "bounded to {CAP}");
        assert!(recent[0].at_ms > recent[1].at_ms, "newest first");
    }

    #[test]
    fn short_label_is_stable() {
        assert_eq!(short_label("abc", "unified"), short_label("abc", "unified"));
    }
}
