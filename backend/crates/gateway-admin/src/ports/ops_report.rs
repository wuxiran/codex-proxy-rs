//! 经营日报的数据来源：CPR 自身库与只读的 sub2api 库；SQL 由 Store 实现。

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::model::ops_report::OpsDayFacts;
use crate::ports::store::AdminStoreResult;

#[async_trait]
pub trait OpsReportSource: Send + Sync {
    /// 是否配置了 sub2api 只读数据源。
    fn sub2api_configured(&self) -> bool;

    /// 汇总 `[start, end)` 区间的经营数据；sub2api 未配置时 `sub2api` 为 `None`。
    async fn collect(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> AdminStoreResult<OpsDayFacts>;
}
