use std::time::SystemTime;

use turn_state::{
    AccountWidePin, RequestFacts, ServedMatch, Source, TurnStateService, credential_binding,
    served::compare,
};

use crate::plain_token;

const EGRESS: &str = "egress-a";
const MODEL: &str = "gpt-6-astra";

fn facts<'a>(binding: &str, client: &'a str, now: SystemTime) -> RequestFacts<'a> {
    RequestFacts {
        account: "acct",
        binding: binding.to_owned(),
        model: MODEL,
        client,
        egress: EGRESS,
        carried: None,
        now,
    }
}

fn pin<'a>(binding: &str, value: &'a str, now: SystemTime) -> AccountWidePin<'a> {
    AccountWidePin {
        account: "acct",
        binding: binding.to_owned(),
        model: MODEL,
        egress: EGRESS.to_owned(),
        value,
        captured_at: now,
        now,
        source: Source::Hunt,
        ttl: None,
        gateway: None,
    }
}

#[test]
fn either_declaration_disagreeing_is_a_mismatch() {
    assert_eq!(compare(MODEL, None, None), ServedMatch::Unknown);
    assert_eq!(compare(MODEL, Some(MODEL), None), ServedMatch::Match);
    assert_eq!(
        compare(MODEL, None, Some("GPT-6-Astra")),
        ServedMatch::Match
    );
    // 头仍写着请求的模型、正文已经换人。
    assert_eq!(
        compare(MODEL, Some(MODEL), Some("gpt-5.6-luna")),
        ServedMatch::Mismatch
    );
    assert_eq!(
        compare(MODEL, Some("gpt-5.6-luna"), Some(MODEL)),
        ServedMatch::Mismatch
    );
    // 带日期的快照名不折叠成裸名。
    assert_eq!(
        compare(MODEL, None, Some("gpt-6-astra-2026-09-15")),
        ServedMatch::Mismatch
    );
}

#[test]
fn a_mismatched_turn_never_becomes_a_template() {
    let service = TurnStateService::in_memory();
    let now = SystemTime::now();
    let binding = credential_binding("gen", "token");
    let value = plain_token(780);

    let mut attempt = service.begin_request(facts(&binding, "cli", now));
    attempt.observe(Some(&value));
    attempt.served_mismatch(now);
    // 换模型之后才到的票（WebSocket metadata）同样不收。
    attempt.observe(Some(&value));
    attempt.completed(now);
    drop(attempt);

    assert!(service.status("acct", &binding, now).is_empty());
}

#[test]
fn a_mismatch_drops_the_template_it_reused_from_memory_and_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let service = TurnStateService::open(dir.path()).expect("open");
    let now = SystemTime::now();
    let binding = credential_binding("gen", "token");
    let value = plain_token(780);
    service
        .pin_account_wide(pin(&binding, &value, now))
        .expect("pin");

    let mut attempt = service.begin_request(facts(&binding, "cli", now));
    assert_eq!(attempt.value(), Some(value.as_str()));
    attempt.served_mismatch(now);
    drop(attempt);

    assert!(service.status("acct", &binding, now).is_empty());
    let reopened = TurnStateService::open(dir.path()).expect("reopen");
    assert!(reopened.status("acct", &binding, now).is_empty());
}

#[test]
fn a_late_mismatch_keeps_a_template_that_was_replaced_meanwhile() {
    let dir = tempfile::tempdir().expect("tempdir");
    let service = TurnStateService::open(dir.path()).expect("open");
    let now = SystemTime::now();
    let binding = credential_binding("gen", "token");
    let old = plain_token(780);
    let new = "b".repeat(780);
    service
        .pin_account_wide(pin(&binding, &old, now))
        .expect("pin old");

    let mut slow = service.begin_request(facts(&binding, "cli", now));
    assert_eq!(slow.value(), Some(old.as_str()));
    // 慢请求还没返回，续期已经把模板换成了新的。
    service.clear_bucket("acct", Some(MODEL));
    service
        .pin_account_wide(pin(&binding, &new, now))
        .expect("pin new");
    slow.served_mismatch(now);
    drop(slow);

    let next = service.begin_request(facts(&binding, "cli", now));
    assert_eq!(next.value(), Some(new.as_str()));
}

#[test]
fn served_results_are_tallied_without_a_pin() {
    let service = TurnStateService::in_memory();
    let now = SystemTime::now();
    service.record_served("acct", MODEL, ServedMatch::Match, now);
    service.record_served("acct", MODEL, ServedMatch::Mismatch, now);
    service.record_served("acct", MODEL, ServedMatch::Unknown, now);

    let snapshot = service.observations(now);
    let tally = &snapshot.buckets[&format!("acct/{MODEL}")];
    assert_eq!(
        (
            tally.served_match,
            tally.served_mismatch,
            tally.served_unknown
        ),
        (1, 1, 1)
    );
    assert!(tally.last_served_mismatch_at > 0);
    // 模型对照不是 state 观测：不进 state 的计数和事件环。
    assert_eq!(
        tally.silent + tally.normal + tally.degraded + tally.unknown,
        0
    );
    assert!(snapshot.events.is_empty());
}

#[test]
fn shortening_the_ttl_caps_templates_that_were_stored_under_the_old_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let service = TurnStateService::open(dir.path()).expect("open");
    let now = SystemTime::now();
    let binding = credential_binding("gen", "token");
    let value = plain_token(780);
    service
        .update_settings(turn_state::Settings {
            ttl_seconds: 3600,
            ..turn_state::Settings::default()
        })
        .expect("long ttl");
    service
        .pin_account_wide(pin(&binding, &value, now))
        .expect("pin");
    let later = now + std::time::Duration::from_secs(600);
    assert!(
        service
            .begin_request(facts(&binding, "cli", later))
            .value()
            .is_some()
    );

    service
        .update_settings(turn_state::Settings::default())
        .expect("240s ttl");
    // 设置调短后，按旧设置写下的票在「签发 + 240 秒」之后不再可用，展示的到期时刻同步变短。
    let soon = now + std::time::Duration::from_secs(60);
    let status = service.status("acct", &binding, soon);
    assert_eq!(status.len(), 1);
    assert!(status[0].expires_at <= now + std::time::Duration::from_secs(240));
    assert!(
        service
            .begin_request(facts(&binding, "cli", later))
            .value()
            .is_none()
    );
}

#[test]
fn a_request_without_a_template_leaves_a_demand_for_on_demand_refill() {
    let service = TurnStateService::in_memory();
    let now = SystemTime::now();
    let binding = credential_binding("gen", "token");
    let window = std::time::Duration::from_secs(240);
    assert!(!service.demanded_within("acct", MODEL, now, window));

    drop(service.begin_request(facts(&binding, "cli", now)));
    assert!(service.demanded_within("acct", MODEL, now, window));
    assert!(!service.demanded_within("acct", "gpt-5.5", now, window));
    // 闲置超过窗口的账号不再补。
    let idle = now + std::time::Duration::from_secs(241);
    assert!(!service.demanded_within("acct", MODEL, idle, window));

    // 有模板的请求不留缺票记录。
    let value = plain_token(780);
    service
        .pin_account_wide(pin(&binding, &value, idle))
        .expect("pin");
    drop(service.begin_request(facts(&binding, "cli", idle)));
    assert!(!service.demanded_within("acct", MODEL, idle, window));
}

fn carrying<'a>(binding: &str, carried: &'a str, now: SystemTime) -> RequestFacts<'a> {
    RequestFacts {
        carried: Some(carried),
        ..facts(binding, "cli", now)
    }
}

#[test]
fn a_mismatch_drops_the_template_even_when_the_client_already_carried_it() {
    let service = TurnStateService::in_memory();
    let now = SystemTime::now();
    let binding = credential_binding("gen", "token");
    let value = plain_token(780);
    service
        .pin_account_wide(pin(&binding, &value, now))
        .expect("pin");

    // 客户端回放的就是桶里这张：请求不用改写，但这一轮用的仍是它。
    let mut attempt = service.begin_request(carrying(&binding, &value, now));
    assert_eq!(attempt.value(), None);
    attempt.served_mismatch(now);
    drop(attempt);

    assert!(
        service
            .begin_request(facts(&binding, "cli", now))
            .value()
            .is_none(),
        "the next request without a state must not get the bad template"
    );
}

#[test]
fn a_mismatch_leaves_an_unrelated_carried_state_alone() {
    let service = TurnStateService::in_memory();
    let now = SystemTime::now();
    let binding = credential_binding("gen", "token");
    let value = plain_token(780);
    service
        .pin_account_wide(pin(&binding, &value, now))
        .expect("pin");

    // 客户端带的是别的票（fill-missing 不替换）：桶里这张没参与这一轮，不该被连坐。
    let other = "o".repeat(780);
    let mut attempt = service.begin_request(carrying(&binding, &other, now));
    assert_eq!(attempt.value(), None);
    attempt.served_mismatch(now);
    drop(attempt);

    assert_eq!(
        service.begin_request(facts(&binding, "cli", now)).value(),
        Some(value.as_str())
    );
}

#[test]
fn dry_run_does_not_touch_stored_templates_on_a_mismatch() {
    let service = TurnStateService::in_memory();
    let now = SystemTime::now();
    let binding = credential_binding("gen", "token");
    let value = plain_token(780);
    service
        .pin_account_wide(pin(&binding, &value, now))
        .expect("pin");
    service
        .update_settings(turn_state::Settings {
            dry_run: true,
            ..turn_state::Settings::default()
        })
        .expect("dry run");

    let mut attempt = service.begin_request(carrying(&binding, &value, now));
    attempt.observe(Some(&"n".repeat(780)));
    attempt.served_mismatch(now);
    attempt.completed(now);
    drop(attempt);

    let status = service.status("acct", &binding, now);
    assert_eq!(
        status.len(),
        1,
        "dry run keeps the template and stores nothing new"
    );
    assert!(status[0].account_wide);
}

#[test]
fn a_template_whose_file_could_not_be_removed_is_not_readmitted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let service = TurnStateService::open(dir.path()).expect("open");
    let now = SystemTime::now();
    let binding = credential_binding("gen", "token");
    let value = plain_token(780);
    service
        .pin_account_wide(pin(&binding, &value, now))
        .expect("pin");

    // 让目录锁拿不到：锁文件的位置被一个目录占住，磁盘上的桶文件因此删不掉。
    let lock = dir.path().join("turn_state.lock");
    std::fs::remove_file(&lock).expect("remove lock file");
    std::fs::create_dir(&lock).expect("block the lock");
    let mut attempt = service.begin_request(facts(&binding, "cli", now));
    assert_eq!(attempt.value(), Some(value.as_str()));
    attempt.served_mismatch(now);
    drop(attempt);

    let bucket = dir
        .path()
        .join("buckets")
        .join("acct")
        .join("gpt-6-astra.json");
    assert!(bucket.exists(), "the file is still there");
    for _ in 0..2 {
        assert!(
            service
                .begin_request(facts(&binding, "cli", now))
                .value()
                .is_none(),
            "a revoked template must not come back from disk"
        );
    }

    // 锁恢复之后，下一次刷新把遗留文件删掉。
    std::fs::remove_dir(&lock).expect("unblock the lock");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert!(
        service
            .begin_request(facts(&binding, "cli", now))
            .value()
            .is_none()
    );
    assert!(
        !bucket.exists(),
        "the leftover file is removed once the lock works"
    );
}
