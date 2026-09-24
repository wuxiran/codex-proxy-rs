//! 账号列表用量投影的 SWR 缓存。
//!
//! 列表里的 24h 滚动用量与额度窗口/终身本地用量都是对 `model_requests` 做
//! O(窗口内请求数) 的 grouping-sets 聚合（现网一天数千请求/号，7 号就要 2 s 以上），
//! 而账号页默认每 30 s 自动刷新。这里只缓存「由请求历史推导」的纯聚合；票据、并发、
//! 状态等管理动作会改的事实不进缓存。策略 stale-while-revalidate：命中即返回，
//! 过期由后台重算，列表永远不等这几条重查询；首次加载或缓存缺失仍内联计算。
use std::{
    collections::BTreeMap,
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
    (query.account_id.clone(), query.key.clone(), query.range.start)
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
    let windows = windows
        .iter()
        .cloned()
        .map(|mut query| {
            if query.range.end < now {
                query.range.end = now;
            }
            query
        })
        .collect::<Vec<_>>();
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
    pub(crate) fn merge_windows(
        &self,
        key: &str,
        queries: &[AccountUsageWindowQuery],
        usage: BTreeMap<WindowKey, AccountUsage>,
    ) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = entries.get_mut(key) {
            let mut projection = (*entry.projection).clone();
            projection.windows.extend(queries.iter().cloned());
            projection.usage_by_window.extend(usage);
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
