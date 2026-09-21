//! 购买记录与每日跑出金额。购买记录不依赖账号行，账号删除后仍可核算。

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use gateway_admin::{
    model::cost_accounting::{
        AccountLiveState, AccountPurchase, DailyAccountUsage, SetAccountPurchase,
    },
    ports::{
        cost_accounting::CostAccountingStore,
        store::{AdminStoreError, AdminStoreResult},
    },
};
use sqlx::{PgPool, Row as _, postgres::PgRow};

use crate::{StoreError, StoreResult, admin_store_error, postgres_unavailable};

const ENTITY: &str = "account purchase";
const PURCHASE_COLUMNS: &str = "p.account_ref, p.name_snapshot, p.email_snapshot, \
     round(p.price * 100)::bigint as price_cents, p.purchased_at, p.retired_at, p.note";

#[derive(Clone)]
pub struct PgCostAccountingRepository {
    pool: PgPool,
}

impl PgCostAccountingRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn store_error(error: StoreError) -> AdminStoreError {
    admin_store_error(ENTITY, error)
}

fn unavailable() -> StoreError {
    postgres_unavailable("account cost accounting operation")
}

fn invalid() -> StoreError {
    StoreError::InvalidData {
        entity: ENTITY,
        message: "invalid account purchase record".to_owned(),
    }
}

fn purchase(row: &PgRow) -> StoreResult<AccountPurchase> {
    Ok(AccountPurchase {
        account_ref: row.try_get("account_ref").map_err(|_| invalid())?,
        name: row.try_get("name_snapshot").map_err(|_| invalid())?,
        email: row.try_get("email_snapshot").map_err(|_| invalid())?,
        price_cents: row.try_get("price_cents").map_err(|_| invalid())?,
        purchased_at: row.try_get("purchased_at").map_err(|_| invalid())?,
        retired_at: row.try_get("retired_at").map_err(|_| invalid())?,
        note: row.try_get("note").map_err(|_| invalid())?,
    })
}

fn count(row: &PgRow, column: &str) -> StoreResult<u64> {
    u64::try_from(row.try_get::<i64, _>(column).map_err(|_| invalid())?).map_err(|_| invalid())
}

#[async_trait]
impl CostAccountingStore for PgCostAccountingRepository {
    async fn set_purchase(&self, command: SetAccountPurchase) -> AdminStoreResult<AccountPurchase> {
        let set_price = command.price_cents.is_some();
        let price_cents = command.price_cents.flatten();
        let set_note = command.note.is_some();
        let note = command.note.flatten();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| store_error(unavailable()))?;
        // 已有记录时只改请求携带的字段；账号可能已删除，所以不能依赖账号行。
        let updated = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update account_purchases p set
               price = case when $2 then $3::numeric / 100 else p.price end,
               purchased_at = coalesce($4, p.purchased_at),
               note = case when $5 then $6 else p.note end,
               updated_at = now()
             where p.account_ref = $1
             returning {PURCHASE_COLUMNS}"
        )))
        .bind(&command.account_id)
        .bind(set_price)
        .bind(price_cents)
        .bind(command.purchased_at)
        .bind(set_note)
        .bind(&note)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| store_error(unavailable()))?;
        let row = match updated {
            Some(row) => row,
            None => sqlx::query(sqlx::AssertSqlSafe(format!(
                "with inserted as (
                   insert into account_purchases
                     (account_ref, name_snapshot, email_snapshot, price, purchased_at, note)
                   select a.id, a.name, a.email, $2::numeric / 100, coalesce($3, a.created_at), $4
                     from provider_accounts a where a.id = $1
                   returning *
                 )
                 select {PURCHASE_COLUMNS} from inserted p"
            )))
            .bind(&command.account_id)
            .bind(price_cents)
            .bind(command.purchased_at)
            .bind(&note)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|_| store_error(unavailable()))?
            .ok_or_else(|| {
                store_error(StoreError::NotFound {
                    entity: ENTITY,
                    id: command.account_id.clone(),
                })
            })?,
        };
        let purchase = purchase(&row).map_err(store_error)?;
        transaction
            .commit()
            .await
            .map_err(|_| store_error(unavailable()))?;
        Ok(purchase)
    }

    async fn set_retired(
        &self,
        account_ids: &[String],
        retired_at: Option<DateTime<Utc>>,
    ) -> AdminStoreResult<Vec<String>> {
        // 恢复上线不需要为从未记录过的账号新建空记录。
        let rows = sqlx::query(
            "with existing as (
               update account_purchases set retired_at = $2, updated_at = now()
                where account_ref = any($1) returning account_ref
             ), created as (
               insert into account_purchases
                 (account_ref, name_snapshot, email_snapshot, purchased_at, retired_at)
               select a.id, a.name, a.email, a.created_at, $2
                 from provider_accounts a
                where a.id = any($1) and $2 is not null
                  and not exists (select 1 from account_purchases p where p.account_ref = a.id)
               returning account_ref
             )
             select account_ref from existing union select account_ref from created",
        )
        .bind(account_ids)
        .bind(retired_at)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| store_error(unavailable()))?;
        rows.iter()
            .map(|row| {
                row.try_get("account_ref")
                    .map_err(|_| store_error(invalid()))
            })
            .collect()
    }

    async fn purchases(
        &self,
    ) -> AdminStoreResult<Vec<(AccountPurchase, Option<AccountLiveState>)>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {PURCHASE_COLUMNS}, a.id is not null as account_exists, a.enabled,
                    a.credential_state = 'ready' as credential_ready,
                    a.quota_access_state = 'exhausted' as quota_exhausted, a.plan_type
               from account_purchases p
               left join provider_accounts a on a.id = p.account_ref
              order by p.purchased_at desc, p.account_ref"
        )))
        .fetch_all(&self.pool)
        .await
        .map_err(|_| store_error(unavailable()))?;
        rows.iter()
            .map(|row| -> StoreResult<_> {
                let exists: bool = row.try_get("account_exists").map_err(|_| invalid())?;
                let live = exists
                    .then(|| -> StoreResult<_> {
                        Ok(AccountLiveState {
                            enabled: row.try_get("enabled").map_err(|_| invalid())?,
                            credential_ready: row
                                .try_get("credential_ready")
                                .map_err(|_| invalid())?,
                            quota_exhausted: row
                                .try_get("quota_exhausted")
                                .map_err(|_| invalid())?,
                            plan_type: row.try_get("plan_type").map_err(|_| invalid())?,
                        })
                    })
                    .transpose()?;
                Ok((purchase(row)?, live))
            })
            .collect::<StoreResult<_>>()
            .map_err(store_error)
    }

    async fn refresh_daily(&self, from: NaiveDate, to: NaiveDate) -> AdminStoreResult<()> {
        // 保留期边界当天的日志可能已被清理了一部分，重算会把金额算小；
        // 因此只重算完整落在保留期内的日期，更早的日期保持上一次落表的结果。
        sqlx::query(
            "with bounds as (
               select greatest($1::date,
                        (now() at time zone 'Asia/Shanghai')::date
                          - (select usage_retention_days from runtime_settings where id = 1)::int + 2
                      ) as from_day,
                      $2::date as to_day
             ), daily as (
               select (mr.started_at at time zone 'Asia/Shanghai')::date as day,
                      mr.provider_account_ref as account_ref,
                      coalesce(sum(coalesce(
                        case when mr.calculated_cost_currency = 'USD' then mr.calculated_cost_amount end,
                        case when mr.cost_currency = 'USD' then mr.cost_amount end,
                        0)), 0) as usage_usd,
                      count(*)::bigint as request_count,
                      coalesce(sum(coalesce(mr.total_tokens, 0)), 0)::bigint as total_tokens
                 from model_requests mr
                 join account_purchases p on p.account_ref = mr.provider_account_ref
                cross join bounds
                where mr.outcome = 'succeeded'
                  and mr.started_at >= (bounds.from_day::timestamp at time zone 'Asia/Shanghai')
                  and mr.started_at < ((bounds.to_day + 1)::timestamp at time zone 'Asia/Shanghai')
                group by 1, 2
             )
             insert into account_cost_daily (day, account_ref, usage_usd, request_count, total_tokens, computed_at)
             select day, account_ref, usage_usd, request_count, total_tokens, now() from daily
             on conflict (day, account_ref) do update set
               usage_usd = excluded.usage_usd,
               request_count = excluded.request_count,
               total_tokens = excluded.total_tokens,
               computed_at = excluded.computed_at",
        )
        .bind(from)
        .bind(to)
        .execute(&self.pool)
        .await
        .map_err(|_| store_error(unavailable()))?;
        Ok(())
    }

    async fn daily_usage(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> AdminStoreResult<Vec<DailyAccountUsage>> {
        let rows = sqlx::query(
            "select day, account_ref, round(usage_usd * 1000000)::bigint as usage_micros,
                    request_count, total_tokens
               from account_cost_daily
              where day between $1 and $2
              order by day, account_ref",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| store_error(unavailable()))?;
        rows.iter()
            .map(|row| -> StoreResult<_> {
                Ok(DailyAccountUsage {
                    day: row.try_get("day").map_err(|_| invalid())?,
                    account_ref: row.try_get("account_ref").map_err(|_| invalid())?,
                    usage_micros: row.try_get("usage_micros").map_err(|_| invalid())?,
                    request_count: count(row, "request_count")?,
                    total_tokens: count(row, "total_tokens")?,
                })
            })
            .collect::<StoreResult<_>>()
            .map_err(store_error)
    }
}
