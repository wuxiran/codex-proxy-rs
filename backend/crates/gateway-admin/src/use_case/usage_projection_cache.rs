//! 账号列表用量投影的 SWR 缓存。
//!
//! 列表里的 24h 滚动用量与额度窗口/终身本地用量都是对 `model_requests` 做
//! O(窗口内请求数) 的 grouping-sets 聚合（现网一天数千请求/号，7 号就要 2 s 以上），
//! 而账号页默认每 30 s 自动刷新。这里只缓存「由请求历史推导」的纯聚合；票据、并发、
//! 状态等管理动作会改的事实不进缓存。策略 stale-while-revalidate：命中即返回，
//! 过期由后台重算，列表永远不等这几条重查询；首次加载或缓存缺失仍内联计算。
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};

use crate::{
    model::{
        accounts::{AccountUsage, AccountUsageWindowQuery, AccountUsageWindowResult},
        observability::TimeRange,
    },
    ports::store::{AccountStore, AdminStoreResult},
};

/// 超过此年龄的投影仍会返回，但触发一次后台重算。
pub(crate) const USAGE_PROJECTION_TTL: Duration = Duration::from_secs(20);
/// 不同分页/筛选各占一项，上限防止无界增长。
const MAX_ENTRIES: usize = 32;

/// 窗口按 (账号, 窗口 key, 窗口起点) 键控：额度窗口翻期后起点变化，自然 miss 后补查。
pub(crate) type WindowKey = (String, String, DateTime<Utc>);

#[derive(Debug, Clone)]
pub(crate) struct UsageProjection {
    pub rolling_usage: BTreeMap<String, AccountUsage>,
    /// 上次列表实际需要的窗口查询；后台刷新照此重算。
    pub windows: Vec<AccountUsageWindowQuery>,
    pub usage_by_window: BTreeMap<WindowKey, AccountUsage>,
}

struct Entry {
    computed_at: Instant,
    projection: Arc<UsageProjection>,
    refreshing: bool,
}

#[derive(Default)]
pub(crate) struct UsageProjectionCache {
    entries: Mutex<BTreeMap<String, Entry>>,
}

pub(crate) fn window_key(query: &AccountUsageWindowQuery) -> WindowKey {
    (
        query.account_id.clone(),
        query.key.clone(),
        query.range.start,
    )
}

/// 同一账号的同一额度窗口只保留一条查询：后出现的（最新观测到的起点）覆盖先出现的，
/// 位置沿用第一次出现的位置。
///
/// 窗口用量 SQL 只按 (账号, 窗口 key) 聚合、不区分起点。上游 reset 边界每次观测都可能
/// 漂移几秒，窗口翻期时起点也会整体后移；同一窗口若带着多个起点一起查询，重叠区间的
/// 请求会被重复累加，账号卡的「按模型价格计费」与「预估额度」随之成倍放大。
pub(crate) fn dedupe_window_queries(
    queries: impl IntoIterator<Item = AccountUsageWindowQuery>,
) -> Vec<AccountUsageWindowQuery> {
    let mut deduped = Vec::<AccountUsageWindowQuery>::new();
    let mut positions = BTreeMap::<(String, String), usize>::new();
    for query in queries {
        let identity = (query.account_id.clone(), query.key.clone());
        if let Some(&position) = positions.get(&identity) {
            deduped[position] = query;
        } else {
            positions.insert(identity, deduped.len());
            deduped.push(query);
        }
    }
    deduped
}

/// 把窗口查询结果按查询列表回填成键控 map（结果只带 account_id + key，起点取自查询）。
pub(crate) fn index_window_results(
    queries: &[AccountUsageWindowQuery],
    results: Vec<AccountUsageWindowResult>,
) -> BTreeMap<WindowKey, AccountUsage> {
    let starts = queries
        .iter()
        .map(|query| {
            (
                (query.account_id.clone(), query.key.clone()),
                query.range.start,
            )
        })
        .collect::<BTreeMap<_, _>>();
    results
        .into_iter()
        .filter_map(|result| {
            let start = *starts.get(&(result.account_id.clone(), result.key.clone()))?;
            Some(((result.account_id, result.key, start), result.usage))
        })
        .collect()
}

pub(crate) fn rolling_range(now: DateTime<Utc>) -> TimeRange {
    TimeRange {
        start: now - chrono::Duration::hours(24),
        end: now,
    }
}

/// 一次算完投影：24h 滚动用量 + 给定窗口的本地用量。
///
/// 额度窗口的 end 是未来的 reset 边界，照旧；终身窗口的 end 是上次的「当前时刻」，
/// 推到现在，否则漏掉新请求。
pub(crate) async fn compute(
    store: &dyn AccountStore,
    account_ids: &[String],
    windows: &[AccountUsageWindowQuery],
) -> AdminStoreResult<UsageProjection> {
    let now = Utc::now();
    let rolling_usage = store
        .load_account_usage(rolling_range(now), account_ids)
        .await?
        .into_iter()
        .map(|usage| (usage.account_id.clone(), usage))
        .collect();
    let windows = dedupe_window_queries(windows.iter().cloned().map(|mut query| {
        if query.range.end < now {
            query.range.end = now;
        }
        query
    }));
    let usage_by_window = if windows.is_empty() {
        BTreeMap::new()
    } else {
        index_window_results(
            &windows,
            store.load_account_usage_by_windows(&windows).await?,
        )
    };
    Ok(UsageProjection {
        rolling_usage,
        windows,
        usage_by_window,
    })
}

impl UsageProjectionCache {
    /// 命中返回投影与是否已过期。
    pub(crate) fn get(&self, key: &str) -> Option<(Arc<UsageProjection>, bool)> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.get(key).map(|entry| {
            (
                Arc::clone(&entry.projection),
                entry.computed_at.elapsed() > USAGE_PROJECTION_TTL,
            )
        })
    }

    pub(crate) fn store(&self, key: String, projection: UsageProjection) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.len() >= MAX_ENTRIES && !entries.contains_key(&key) {
            let oldest = entries
                .iter()
                .min_by_key(|(_, entry)| entry.computed_at)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                entries.remove(&oldest);
            }
        }
        entries.insert(
            key,
            Entry {
                computed_at: Instant::now(),
                projection: Arc::new(projection),
                refreshing: false,
            },
        );
    }

    /// 内联补查到的窗口并入现有投影（不改年龄，避免延长其余数据的陈旧度）。
    ///
    /// 补查意味着该账号窗口的起点变了（reset 边界漂移或翻期），同账号同窗口的旧查询与
    /// 旧结果一并替换；只追加会让后台重算把新旧起点一起查询并重复累加。
    pub(crate) fn merge_windows(
        &self,
        key: &str,
        queries: &[AccountUsageWindowQuery],
        usage: BTreeMap<WindowKey, AccountUsage>,
    ) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = entries.get_mut(key) {
            let mut projection = (*entry.projection).clone();
            let replaced = queries
                .iter()
                .map(|query| (query.account_id.as_str(), query.key.as_str()))
                .collect::<BTreeSet<_>>();
            projection
                .usage_by_window
                .retain(|(account_id, window, _), _| {
                    !replaced.contains(&(account_id.as_str(), window.as_str()))
                });
            projection.usage_by_window.extend(usage);
            projection.windows = dedupe_window_queries(
                std::mem::take(&mut projection.windows)
                    .into_iter()
                    .chain(queries.iter().cloned()),
            );
            entry.projection = Arc::new(projection);
        }
    }

    /// 过期且无在途刷新时标记为刷新中并返回快照；否则 None。
    pub(crate) fn try_begin_refresh(&self, key: &str) -> Option<Arc<UsageProjection>> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.get_mut(key)?;
        if entry.refreshing || entry.computed_at.elapsed() <= USAGE_PROJECTION_TTL {
            return None;
        }
        entry.refreshing = true;
        Some(Arc::clone(&entry.projection))
    }

    fn finish_refresh(&self, key: &str, projection: Option<UsageProjection>) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = entries.get_mut(key) else {
            return;
        };
        entry.refreshing = false;
        if let Some(projection) = projection {
            entry.computed_at = Instant::now();
            entry.projection = Arc::new(projection);
        }
    }

    /// 后台重算，失败只清标记并保留旧投影。
    pub(crate) fn spawn_refresh(
        self: &Arc<Self>,
        store: Arc<dyn AccountStore>,
        key: String,
        account_ids: Vec<String>,
        snapshot: Arc<UsageProjection>,
    ) {
        let cache = Arc::clone(self);
        tokio::spawn(async move {
            match compute(store.as_ref(), &account_ids, &snapshot.windows).await {
                Ok(projection) => cache.finish_refresh(&key, Some(projection)),
                Err(error) => {
                    tracing::warn!(error = %error, "account usage projection refresh failed");
                    cache.finish_refresh(&key, None);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{DateTime, Duration, TimeZone as _, Utc};

    use super::{UsageProjection, UsageProjectionCache, dedupe_window_queries, window_key};
    use crate::model::{
        accounts::{AccountUsage, AccountUsageWindowQuery},
        observability::TimeRange,
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
            cached_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
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
}
