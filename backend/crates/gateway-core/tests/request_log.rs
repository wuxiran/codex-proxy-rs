//! fork：请求日志环形缓冲有界、最新在前，响应侧按 id 回填不覆盖已有字段。

use gateway_core::request_log::{
    CAP, RequestLogRecord, ResponsePatch, now_ms, recent, record, short_label, update_response,
};

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
            served_match: None,
            gateway: None,
            gateway_source: None,
            resp_cookies: None,
            cfbm_ttl: None,
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
        served_match: None,
        gateway: None,
        gateway_source: None,
        resp_cookies: None,
        cfbm_ttl: None,
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
