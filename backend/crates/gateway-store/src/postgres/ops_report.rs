//! 经营日报数据源：CPR 自身库 + 只读的 sub2api 库。
//!
//! sub2api 连接只用于统计：独立的小连接池、会话级只读、语句超时，不跑 CPR 迁移。
//! 平台 1 元 = 1 刀；英雄套餐订阅（`subscription_plans.name` 含「英雄」）按 1/5 折算。

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_admin::model::ops_report::{
    CprDayFacts, OpsDayFacts, OpsPurchase, Sub2apiDayFacts, Sub2apiSlice,
};
use gateway_admin::ports::ops_report::OpsReportSource;
use gateway_admin::ports::store::AdminStoreResult;
use sqlx::{
    PgPool, Row as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};

use crate::{StoreResult, admin_store_error, postgres_unavailable};

const ENTITY: &str = "ops report";
const SUB2API_MAX_CONNECTIONS: u32 = 2;
const SUB2API_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);
const SUB2API_STATEMENT_TIMEOUT_MS: &str = "60000";

pub struct PgOpsReportSource {
    pool: PgPool,
    sub2api: Option<PgPool>,
}

impl PgOpsReportSource {
    /// `sub2api_database_url` 为空时只统计 CPR 部分。连接是惰性的，sub2api 暂不可达不影响启动。
    ///
    /// # Errors
    ///
    /// sub2api 连接串无法解析时返回错误。
    pub fn new(pool: PgPool, sub2api_database_url: Option<&str>) -> StoreResult<Self> {
        let sub2api = sub2api_database_url
            .filter(|url| !url.trim().is_empty())
            .map(|url| {
                let options = url
                    .parse::<PgConnectOptions>()
                    .map_err(|_| postgres_unavailable("parse sub2api report connection options"))?
                    .application_name("codex-proxy-rs:ops-report")
                    .options([
                        ("default_transaction_read_only", "on"),
                        ("statement_timeout", SUB2API_STATEMENT_TIMEOUT_MS),
                    ]);
                Ok::<_, crate::StoreError>(
                    PgPoolOptions::new()
                        .max_connections(SUB2API_MAX_CONNECTIONS)
                        .min_connections(0)
                        .acquire_timeout(SUB2API_ACQUIRE_TIMEOUT)
                        .idle_timeout(Duration::from_secs(300))
                        .connect_lazy_with(options),
                )
            })
            .transpose()?;
        Ok(Self { pool, sub2api })
    }

    async fn cpr_facts(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<(Vec<String>, Vec<OpsPurchase>, CprDayFacts), sqlx::Error> {
        let new_account_ids = sqlx::query_scalar::<_, String>(
            "select id from provider_accounts
              where created_at >= $1 and created_at < $2
              order by id",
        )
        .bind(start)
        .bind(end)
        .fetch_all(&self.pool)
        .await?;
        let purchases = sqlx::query(
            "select t.provider_account_id, t.purchase_amount::float8 as amount, t.purchase_currency
               from account_tickets t
               join provider_accounts pa on pa.id = t.provider_account_id
              where t.purchase_amount is not null
                and coalesce(t.purchased_at, pa.created_at) >= $1
                and coalesce(t.purchased_at, pa.created_at) < $2",
        )
        .bind(start)
        .bind(end)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|row| {
            Ok(OpsPurchase {
                account_id: row.try_get("provider_account_id")?,
                amount: row.try_get("amount")?,
                currency: row.try_get("purchase_currency")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
        let row = sqlx::query(
            "select count(*) as requests,
                    count(*) filter (where mr.outcome = 'failed') as failed,
                    coalesce(sum(mrb.calculated_cost_amount)
                      filter (where mrb.calculated_cost_currency = 'USD'), 0)::float8 as official_usd
               from model_requests mr
               left join model_request_billing mrb on mrb.model_request_id = mr.id
              where mr.started_at >= $1 and mr.started_at < $2",
        )
        .bind(start)
        .bind(end)
        .fetch_one(&self.pool)
        .await?;
        let cpr = CprDayFacts {
            requests: row.try_get("requests")?,
            failed_requests: row.try_get("failed")?,
            official_usd: row.try_get("official_usd")?,
        };
        Ok((new_account_ids, purchases, cpr))
    }
}

async fn sub2api_facts(
    pool: &PgPool,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Sub2apiDayFacts, sqlx::Error> {
    let row = sqlx::query(
        "with logs as (
            select ul.user_id, ul.total_cost, ul.actual_cost,
                   case when ul.billing_type = 1 and p.name like '%英雄%'
                        then ul.actual_cost / 5 else ul.actual_cost end as adjusted,
                   (g.platform = 'openai' and coalesce(ul.image_count, 0) = 0) as codex,
                   coalesce(a.name ilike '%cpr%', false) as via_cpr
              from usage_logs ul
              left join user_subscriptions s on s.id = ul.subscription_id
              left join subscription_plans p on p.id = s.plan_id
              left join groups g on g.id = ul.group_id
              left join accounts a on a.id = ul.account_id
             where ul.created_at >= $1 and ul.created_at < $2
          )
          select count(distinct user_id) as active_users,
                 count(*) as requests,
                 coalesce(sum(total_cost), 0)::float8 as standard,
                 coalesce(sum(actual_cost), 0)::float8 as actual,
                 coalesce(sum(adjusted), 0)::float8 as adjusted,
                 count(*) filter (where codex) as codex_requests,
                 coalesce(sum(total_cost) filter (where codex), 0)::float8 as codex_standard,
                 coalesce(sum(adjusted) filter (where codex), 0)::float8 as codex_adjusted,
                 count(*) filter (where codex and via_cpr) as cpr_requests,
                 coalesce(sum(total_cost) filter (where codex and via_cpr), 0)::float8 as cpr_standard,
                 coalesce(sum(adjusted) filter (where codex and via_cpr), 0)::float8 as cpr_adjusted
            from logs",
    )
    .bind(start)
    .bind(end)
    .fetch_one(pool)
    .await?;
    let payments = sqlx::query_scalar::<_, f64>(
        "select coalesce(sum(pay_amount - coalesce(refund_amount, 0)), 0)::float8
           from payment_orders
          where status = 'COMPLETED' and paid_at >= $1 and paid_at < $2",
    )
    .bind(start)
    .bind(end)
    .fetch_one(pool)
    .await?;
    Ok(Sub2apiDayFacts {
        active_users: row.try_get("active_users")?,
        requests: row.try_get("requests")?,
        standard: row.try_get("standard")?,
        actual: row.try_get("actual")?,
        adjusted: row.try_get("adjusted")?,
        codex: Sub2apiSlice {
            requests: row.try_get("codex_requests")?,
            standard: row.try_get("codex_standard")?,
            adjusted: row.try_get("codex_adjusted")?,
        },
        codex_via_cpr: Sub2apiSlice {
            requests: row.try_get("cpr_requests")?,
            standard: row.try_get("cpr_standard")?,
            adjusted: row.try_get("cpr_adjusted")?,
        },
        payments,
    })
}

#[async_trait]
impl OpsReportSource for PgOpsReportSource {
    fn sub2api_configured(&self) -> bool {
        self.sub2api.is_some()
    }

    async fn collect(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> AdminStoreResult<OpsDayFacts> {
        let (new_account_ids, purchases, cpr) =
            self.cpr_facts(start, end).await.map_err(|error| {
                tracing::warn!(target: "ops_report", error = %error, "cpr report query failed");
                admin_store_error(ENTITY, postgres_unavailable("load cpr daily report"))
            })?;
        let sub2api = match &self.sub2api {
            Some(pool) => Some(sub2api_facts(pool, start, end).await.map_err(|error| {
                tracing::warn!(target: "ops_report", error = %error, "sub2api report query failed");
                admin_store_error(ENTITY, postgres_unavailable("load sub2api daily report"))
            })?),
            None => None,
        };
        Ok(OpsDayFacts {
            new_account_ids,
            purchases,
            cpr,
            sub2api,
        })
    }
}
