//! fork：账号列表用量投影 SWR 缓存——窗口查询去重与漂移窗口的替换合并。

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, TimeZone as _, Utc};
use gateway_admin::{
    UsageProjection, UsageProjectionCache, dedupe_window_queries,
    model::{
        accounts::{AccountUsage, AccountUsageWindowQuery},
        observability::TimeRange,
    },
    window_key,
};

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + seconds, 0).unwrap()
}

fn weekly(account_id: &str, key: &str, start: DateTime<Utc>) -> AccountUsageWindowQuery {
    AccountUsageWindowQuery {
        account_id: account_id.to_owned(),
        key: key.to_owned(),
        range: TimeRange::new(start, start + Duration::days(7)).unwrap(),
    }
}

fn usage(account_id: &str, request_count: u64) -> AccountUsage {
    AccountUsage {
        billing: Default::default(),
        account_id: account_id.to_owned(),
        request_count,
        success_count: request_count,
        input_tokens: None,
        output_tokens: None,
        cached_input_tokens: None,
        cache_write_input_tokens: None,
        reasoning_output_tokens: None,
        image_input_tokens: None,
        image_output_tokens: None,
        image_request_count: 0,
        image_request_failed_count: 0,
        total_tokens: None,
        cost_coverage: Default::default(),
        costs: Vec::new(),
        last_used_at: None,
        request_buckets: Vec::new(),
        models: Vec::new(),
    }
}

#[test]
fn dedupe_keeps_latest_start_per_account_window() {
    let queries = vec![
        weekly("a", "codex.primary", at(0)),
        weekly("b", "codex.primary", at(0)),
        weekly("a", "codex.primary", at(2)),
        weekly("a", "codex.secondary", at(0)),
        weekly("a", "codex.primary", at(5)),
    ];

    let deduped = dedupe_window_queries(queries);

    assert_eq!(
        deduped,
        vec![
            weekly("a", "codex.primary", at(5)),
            weekly("b", "codex.primary", at(0)),
            weekly("a", "codex.secondary", at(0)),
        ]
    );
}

#[test]
fn merge_replaces_drifted_window_instead_of_accumulating() {
    let cache = UsageProjectionCache::default();
    let old_a = weekly("a", "codex.primary", at(0));
    let b = weekly("b", "codex.primary", at(0));
    cache.store(
        "page".to_owned(),
        UsageProjection {
            rolling_usage: BTreeMap::new(),
            windows: vec![old_a.clone(), b.clone()],
            usage_by_window: BTreeMap::from([
                (window_key(&old_a), usage("a", 1)),
                (window_key(&b), usage("b", 2)),
            ]),
        },
    );

    // reset 边界先后漂移两次：每次补查都应替换而不是追加。
    for drift in [1, 3] {
        let drifted = weekly("a", "codex.primary", at(drift));
        cache.merge_windows(
            "page",
            std::slice::from_ref(&drifted),
            BTreeMap::from([(window_key(&drifted), usage("a", 10))]),
        );
    }

    let (projection, _) = cache.get("page").unwrap();
    let latest = weekly("a", "codex.primary", at(3));
    assert_eq!(projection.windows, vec![latest.clone(), b.clone()]);
    assert_eq!(
        projection
            .usage_by_window
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec![window_key(&latest), window_key(&b)]
    );
    assert_eq!(
        projection.usage_by_window[&window_key(&latest)].request_count,
        10
    );
}
