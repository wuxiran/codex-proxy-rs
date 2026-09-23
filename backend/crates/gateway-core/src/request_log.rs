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
///
/// 请求侧字段在 lease 组装时写入；响应侧字段（`set_cookie`/`ticket_out`/`ticket_len`/
/// `service_tier`）在上游响应回来后由 [`update_response`] 按 `id` 回填。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestLogRecord {
    /// 关联 id（网关 request id），仅用于请求↔响应两阶段回填，不对外序列化。
    #[serde(skip)]
    pub id: String,
    /// 记录时刻（Unix 毫秒），前端本地格式化。
    pub at_ms: i64,
    /// 请求模型，如 gpt-6-sol。
    pub model: String,
    /// cookie 决策：reuse(沿用自己的) / inject(从统一池借) / none(无 cf_bm)。
    pub cookie_action: String,
    /// 出口指纹短标识（不泄露代理原文）。
    pub egress: String,
    /// 本次所用 `__cf_bm` cookie 值的短哈希桶（如 `cfbm-88`）。
    /// ⚠️ 仅是 cookie 值的指纹，**不是** GPT 网关节点号（`chat.gateway.unified-N`）——
    /// cpr 连固定 `chatgpt.com/backend-api`，拿不到真节点号。别当节点/满血依据。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unified: Option<String>,
    /// 使用的 turn-state 票短指纹（非原文），如 #hPdTPIqq。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket_in: Option<String>,
    /// 响应侧：上游是否下发了 `__cf_bm`（Set-Cookie）。None=尚未观测/无响应。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_cookie: Option<bool>,
    /// 响应侧：上游返回的 turn-state 票短指纹（非原文）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket_out: Option<String>,
    /// 响应侧：上游返回票据的字节长度（票长）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket_len: Option<usize>,
    /// 响应侧：上游回的 service_tier（default/flex/priority）。现网常态 default，
    /// **仅诊断**，不作满血判据（满血信号已被上游堵，被动判不了）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    /// 响应侧：上游实际服务的模型（`openai-model` 头 / body `response.model`）。
    /// 与请求模型**分叉**（present 且 != 请求模型）= 猫腻信号（掺假/relay/降级上报）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub served_model: Option<String>,
    /// 响应侧：**未过滤**的全部 Set-Cookie 摘要（每项 `name@domain#值指纹`，非原文）。
    /// 用于实测上游到底下发了哪些 cookie（含 cpr 平时按白名单丢掉的），
    /// 好确认「网关节点信息是否藏在某张 cookie 里」。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resp_cookies: Option<Vec<String>>,
}

/// 响应侧回填补丁：只填 `Some(..)` 的字段，`None` 保持不动。
#[derive(Default)]
pub struct ResponsePatch {
    pub set_cookie: Option<bool>,
    pub ticket_out: Option<String>,
    pub ticket_len: Option<usize>,
    pub service_tier: Option<String>,
    pub served_model: Option<String>,
    pub resp_cookies: Option<Vec<String>>,
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

/// 按 `id` 回填响应侧字段（最近匹配优先）。id 空或未命中则静默跳过。
/// 只覆盖补丁里 `Some(..)` 的字段，避免第二次回填抹掉第一次的结果。
pub fn update_response(id: &str, patch: ResponsePatch) {
    if id.is_empty() {
        return;
    }
    if let Ok(mut buf) = buffer().lock()
        && let Some(rec) = buf.iter_mut().rev().find(|r| r.id == id)
    {
        if patch.set_cookie.is_some() {
            rec.set_cookie = patch.set_cookie;
        }
        if patch.ticket_out.is_some() {
            rec.ticket_out = patch.ticket_out;
        }
        if patch.ticket_len.is_some() {
            rec.ticket_len = patch.ticket_len;
        }
        if patch.service_tier.is_some() {
            rec.service_tier = patch.service_tier;
        }
        if patch.served_model.is_some() {
            rec.served_model = patch.served_model;
        }
        if patch.resp_cookies.is_some() {
            rec.resp_cookies = patch.resp_cookies;
        }
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
                id: format!("req-{i}"),
                at_ms: i as i64,
                model: "gpt-6-sol".into(),
                cookie_action: "reuse".into(),
                egress: "e".into(),
                unified: None,
                ticket_in: None,
                set_cookie: None,
                ticket_out: None,
                ticket_len: None,
                service_tier: None,
                served_model: None,
                resp_cookies: None,
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

    #[test]
    fn update_response_backfills_by_id_without_clobbering() {
        let id = "corr-xyz";
        record(RequestLogRecord {
            id: id.into(),
            at_ms: now_ms(),
            model: "gpt-6-sol".into(),
            cookie_action: "inject".into(),
            egress: "egr-1".into(),
            unified: Some("cfbm-1".into()),
            ticket_in: Some("#aaaaaa".into()),
            set_cookie: None,
            ticket_out: None,
            ticket_len: None,
            service_tier: None,
            served_model: None,
            resp_cookies: None,
        });
        // 第一次回填 cookie/票，第二次只回填档位+实际模型，前者都应保留。
        update_response(
            id,
            ResponsePatch {
                set_cookie: Some(true),
                ticket_out: Some("#bbbbbb".into()),
                ticket_len: Some(780),
                ..Default::default()
            },
        );
        update_response(
            id,
            ResponsePatch {
                service_tier: Some("priority".into()),
                served_model: Some("gpt-6-luna".into()),
                ..Default::default()
            },
        );
        let found = recent(1000)
            .into_iter()
            .find(|r| r.id == id)
            .expect("record present");
        assert_eq!(found.set_cookie, Some(true));
        assert_eq!(found.ticket_len, Some(780));
        assert_eq!(found.ticket_out.as_deref(), Some("#bbbbbb"));
        assert_eq!(found.service_tier.as_deref(), Some("priority"));
        assert_eq!(found.served_model.as_deref(), Some("gpt-6-luna"));
        // 未命中 id 不应 panic。
        update_response(
            "nope",
            ResponsePatch {
                service_tier: Some("x".into()),
                ..Default::default()
            },
        );
    }
}
