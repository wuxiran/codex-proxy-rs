//! 经营日报：定时按北京时间自然日采集经营数据，快照落在 runtime 数据目录下的 JSON 文件。
//!
//! 不建表，避免与其它 fork 子表迁移编号冲突。任务加跨实例租约，蓝绿两个槽位不会同时写快照；
//! 今天每轮刷新，过去未定稿的日子（含首次启动时最近 [`BACKFILL_DAYS`] 天）逐轮补齐。

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Days, NaiveDate, TimeZone as _, Utc};
use chrono_tz::Asia::Shanghai;
use gateway_core::task::{ScheduledTask, WorkerCycleContext, WorkerTaskError};

use crate::model::AdminError;
use crate::model::ops_report::{OpsDayRecord, OpsReport};
use crate::ports::ops_report::OpsReportSource;

pub const OPS_REPORT_INTERVAL: Duration = Duration::from_secs(10 * 60);
pub const OPS_REPORT_WORKER_OWNER: &str = "ops-daily-report";
pub const WORKER_INITIAL_BACKOFF: Duration = Duration::from_secs(30);
pub const WORKER_MAXIMUM_BACKOFF: Duration = Duration::from_secs(600);
/// 首次启动回填的天数。
pub const BACKFILL_DAYS: u64 = 30;
/// 每轮最多补的历史天数；sub2api 的 usage_logs 扫描较重，分摊到多轮。
const BACKFILL_PER_CYCLE: usize = 3;
/// 日终后再等一会儿才定稿，让延迟写入的日志落库。
const FINALIZE_GRACE: chrono::Duration = chrono::Duration::minutes(30);
/// 快照最多保留的天数。
const KEEP_DAYS: usize = 400;
const SNAPSHOT_FILE: &str = "daily.json";

pub struct OpsReportService {
    source: Option<Arc<dyn OpsReportSource>>,
    dir: PathBuf,
}

impl OpsReportService {
    #[must_use]
    pub fn new(source: Option<Arc<dyn OpsReportSource>>, dir: PathBuf) -> Self {
        Self { source, dir }
    }

    /// 最近 `days` 天的日报，新日期在前。
    ///
    /// # Errors
    ///
    /// 快照文件无法读取时返回错误。
    pub fn report(&self, days: usize) -> Result<OpsReport, AdminError> {
        let mut records = self.load().map_err(|error| {
            tracing::warn!(target: "ops_report", error = %error, "ops report snapshot unreadable");
            AdminError::new(
                crate::model::AdminErrorKind::Unavailable,
                "日报快照暂不可读",
            )
        })?;
        records.sort_by_key(|record| std::cmp::Reverse(record.day));
        records.truncate(days);
        Ok(OpsReport {
            sub2api_configured: self
                .source
                .as_ref()
                .is_some_and(|source| source.sub2api_configured()),
            days: records.iter().map(OpsDayRecord::view).collect(),
        })
    }

    async fn refresh_cycle(&self, context: &WorkerCycleContext) {
        let Some(source) = &self.source else {
            return;
        };
        let mut records = match self.load() {
            Ok(records) => records,
            Err(error) => {
                tracing::warn!(target: "ops_report", error = %error, "ops report snapshot unreadable");
                return;
            }
        };
        let now = Utc::now();
        let today = now.with_timezone(&Shanghai).date_naive();
        let mut pending = vec![today];
        pending.extend(
            (1..=BACKFILL_DAYS)
                .filter_map(|offset| today.checked_sub_days(Days::new(offset)))
                .filter(|day| {
                    !records
                        .iter()
                        .any(|record| record.day == *day && record.finalized)
                })
                .take(BACKFILL_PER_CYCLE),
        );
        let mut changed = false;
        for day in pending {
            if context.cancellation().is_cancelled() {
                break;
            }
            let Some((start, end)) = day_range(day) else {
                continue;
            };
            let facts = match source.collect(start, end).await {
                Ok(facts) => facts,
                Err(error) => {
                    tracing::warn!(target: "ops_report", day = %day, error_kind = ?error.kind(),
                        "ops report collect failed");
                    continue;
                }
            };
            let refreshed_at = Utc::now();
            let finalized = refreshed_at >= end + FINALIZE_GRACE;
            match records.iter_mut().find(|record| record.day == day) {
                Some(record) => record.merge(facts, refreshed_at, finalized),
                None => {
                    let mut record = OpsDayRecord::new(day, refreshed_at);
                    record.merge(facts, refreshed_at, finalized);
                    records.push(record);
                }
            }
            changed = true;
        }
        if !changed {
            return;
        }
        records.sort_by_key(|record| std::cmp::Reverse(record.day));
        records.truncate(KEEP_DAYS);
        if let Err(error) = self.store(&records) {
            tracing::warn!(target: "ops_report", error = %error, "ops report snapshot write failed");
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.join(SNAPSHOT_FILE)
    }

    fn load(&self) -> std::io::Result<Vec<OpsDayRecord>> {
        match fs::read(self.path()) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    fn store(&self, records: &[OpsDayRecord]) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let bytes = serde_json::to_vec_pretty(records)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        write_atomically(&self.path(), &bytes)
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension("json.tmp");
    let result = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// 北京时间自然日对应的 UTC 区间 `[start, end)`。
#[must_use]
pub fn day_range(day: NaiveDate) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let start = Shanghai
        .from_local_datetime(&day.and_hms_opt(0, 0, 0)?)
        .single()?
        .with_timezone(&Utc);
    Some((start, start + chrono::Duration::days(1)))
}

/// 定时采集任务；实际逻辑在 [`OpsReportService`]，与管理端读取共用同一份快照。
pub struct OpsReportTask(pub Arc<OpsReportService>);

impl ScheduledTask for OpsReportTask {
    fn run_cycle(
        &self,
        context: WorkerCycleContext,
    ) -> futures::future::BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            self.0.refresh_cycle(&context).await;
            Ok(())
        })
    }
}
