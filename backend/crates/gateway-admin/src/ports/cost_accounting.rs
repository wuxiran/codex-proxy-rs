use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};

use super::store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult};
use crate::model::cost_accounting::{
    AccountLiveState, AccountPurchase, DailyAccountUsage, SetAccountPurchase,
};

#[async_trait]
pub trait CostAccountingStore: Send + Sync {
    /// 写入或更新购买记录；首次写入时用账号当前的名称、邮箱和加入时间建立快照。
    /// 账号不存在且此前没有记录时返回 NotFound。
    async fn set_purchase(&self, command: SetAccountPurchase) -> AdminStoreResult<AccountPurchase>;
    /// 手动标记下线或恢复；没有购买记录的账号会得到一条无价格的记录。返回实际变更的账号。
    async fn set_retired(
        &self,
        account_ids: &[String],
        retired_at: Option<DateTime<Utc>>,
    ) -> AdminStoreResult<Vec<String>>;
    /// 全部购买记录及账号现状；账号已删除时现状为空。
    async fn purchases(&self)
    -> AdminStoreResult<Vec<(AccountPurchase, Option<AccountLiveState>)>>;
    /// 用请求日志重算区间内仍在保留期的日期并落表；保留期之外的日期不触碰。
    async fn refresh_daily(&self, from: NaiveDate, to: NaiveDate) -> AdminStoreResult<()>;
    async fn daily_usage(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> AdminStoreResult<Vec<DailyAccountUsage>>;
}

/// 未接入存储的组合（部分测试装配）使用；任何调用都明确失败，而不是返回空数据冒充正常。
pub struct UnconfiguredCostAccounting;

fn unconfigured<T>() -> AdminStoreResult<T> {
    Err(AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        "cost accounting",
        "cost accounting store is not configured",
    ))
}

#[async_trait]
impl CostAccountingStore for UnconfiguredCostAccounting {
    async fn set_purchase(&self, _: SetAccountPurchase) -> AdminStoreResult<AccountPurchase> {
        unconfigured()
    }
    async fn set_retired(
        &self,
        _: &[String],
        _: Option<DateTime<Utc>>,
    ) -> AdminStoreResult<Vec<String>> {
        unconfigured()
    }
    async fn purchases(
        &self,
    ) -> AdminStoreResult<Vec<(AccountPurchase, Option<AccountLiveState>)>> {
        unconfigured()
    }
    async fn refresh_daily(&self, _: NaiveDate, _: NaiveDate) -> AdminStoreResult<()> {
        unconfigured()
    }
    async fn daily_usage(
        &self,
        _: NaiveDate,
        _: NaiveDate,
    ) -> AdminStoreResult<Vec<DailyAccountUsage>> {
        unconfigured()
    }
}
