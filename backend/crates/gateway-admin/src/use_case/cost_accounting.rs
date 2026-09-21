use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, NaiveDate, Utc};

use super::map_store_error;
use crate::{
    model::{AdminError, cost_accounting::*},
    ports::cost_accounting::CostAccountingStore,
};

#[async_trait]
pub trait CostAccountingService: Send + Sync {
    /// 每日投入、跑出与每 1 美元成本；查询前先把保留期内的日期重算落表。
    async fn daily(&self, from: NaiveDate, to: NaiveDate) -> Result<CostReport, AdminError>;
    /// 每个有购买记录的账号在区间内的核算；默认不含已下线账号。
    async fn accounts(
        &self,
        from: NaiveDate,
        to: NaiveDate,
        include_retired: bool,
    ) -> Result<Vec<AccountCostRow>, AdminError>;
    async fn set_purchase(
        &self,
        command: SetAccountPurchase,
    ) -> Result<AccountPurchase, AdminError>;
    /// 下线只是管理员的标记：不停用账号、不改调度，恢复后照常参与。
    async fn set_retired(
        &self,
        account_ids: Vec<String>,
        retired: bool,
    ) -> Result<Vec<String>, AdminError>;
}

pub(crate) struct DefaultCostAccountingService {
    store: Arc<dyn CostAccountingStore>,
}

impl DefaultCostAccountingService {
    pub(crate) fn new(store: Arc<dyn CostAccountingStore>) -> Self {
        Self { store }
    }

    async fn refreshed_usage(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<DailyAccountUsage>, AdminError> {
        validate_range(from, to)?;
        self.store
            .refresh_daily(from, to)
            .await
            .map_err(|error| map_store_error(error, "cost accounting"))?;
        self.store
            .daily_usage(from, to)
            .await
            .map_err(|error| map_store_error(error, "cost accounting"))
    }
}

fn validate_range(from: NaiveDate, to: NaiveDate) -> Result<(), AdminError> {
    if from > to || to - from > Duration::days(MAX_COST_RANGE_DAYS) {
        return Err(AdminError::invalid("核算日期范围不合法，最长 366 天"));
    }
    Ok(())
}

fn validate_account_id(id: &str) -> Result<(), AdminError> {
    if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
        return Err(AdminError::invalid("账号 ID 不合法"));
    }
    Ok(())
}

#[async_trait]
impl CostAccountingService for DefaultCostAccountingService {
    async fn daily(&self, from: NaiveDate, to: NaiveDate) -> Result<CostReport, AdminError> {
        let usage = self.refreshed_usage(from, to).await?;
        let purchases: Vec<_> = self
            .store
            .purchases()
            .await
            .map_err(|error| map_store_error(error, "cost accounting"))?
            .into_iter()
            .map(|(purchase, _)| purchase)
            .collect();
        // 下线不影响历史：已下线账号的投入和跑出照常计入。
        Ok(build_cost_report(from, to, &purchases, &usage))
    }

    async fn accounts(
        &self,
        from: NaiveDate,
        to: NaiveDate,
        include_retired: bool,
    ) -> Result<Vec<AccountCostRow>, AdminError> {
        let usage = self.refreshed_usage(from, to).await?;
        let mut purchases = self
            .store
            .purchases()
            .await
            .map_err(|error| map_store_error(error, "cost accounting"))?;
        if !include_retired {
            purchases.retain(|(purchase, _)| purchase.retired_at.is_none());
        }
        Ok(build_account_rows(purchases, &usage))
    }

    async fn set_purchase(
        &self,
        mut command: SetAccountPurchase,
    ) -> Result<AccountPurchase, AdminError> {
        validate_account_id(&command.account_id)?;
        if let Some(Some(price)) = command.price_cents
            && !(0..=MAX_PRICE_CENTS).contains(&price)
        {
            return Err(AdminError::invalid("购买价格不合法"));
        }
        if command
            .purchased_at
            .is_some_and(|value| value > Utc::now() + Duration::days(1))
        {
            return Err(AdminError::invalid("购买时间不能晚于当前时间"));
        }
        if let Some(note) = command.note.as_mut() {
            *note = note
                .take()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty());
            if note.as_deref().is_some_and(|value| {
                value.chars().count() > 512 || value.chars().any(char::is_control)
            }) {
                return Err(AdminError::invalid("备注最长 512 个字符"));
            }
        }
        self.store.set_purchase(command).await.map_err(|error| {
            if error.kind() == crate::ports::store::AdminStoreErrorKind::NotFound {
                AdminError::not_found("账号不存在")
            } else {
                map_store_error(error, "cost accounting")
            }
        })
    }

    async fn set_retired(
        &self,
        account_ids: Vec<String>,
        retired: bool,
    ) -> Result<Vec<String>, AdminError> {
        if account_ids.is_empty() || account_ids.len() > MAX_RETIRE_BATCH {
            return Err(AdminError::invalid("一次需要选择 1 至 200 个账号"));
        }
        for id in &account_ids {
            validate_account_id(id)?;
        }
        self.store
            .set_retired(&account_ids, retired.then(Utc::now))
            .await
            .map_err(|error| map_store_error(error, "cost accounting"))
    }
}
