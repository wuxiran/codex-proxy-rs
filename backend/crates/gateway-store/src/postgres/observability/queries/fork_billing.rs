//! fork: billing —— 账号用量的完整模型身份与双费用口径（模型价格 / 真实上游费用）
//!
//! 上游账号用量改由 `request_analytics_*` 投影聚合，但投影只按
//! `coalesce(upstream_model_id, requested_model_id)` 分桶，没有 fork 子表
//! `model_request_billing`（迁移 9001）里的 `response_model` / `billing_model` /
//! `calculated_cost_*`。这里在同一事务快照上直接扫 `model_requests ⋈ model_request_billing`
//! 补算：账号级只补 [`AccountBillingAmounts`]；模型级按四元身份重新分组，整段替换上游的模型列表。
//! 只有管理端账号面板（`split_model_identity`）与额度窗口会走到这里，看板最近用量不触发。

use gateway_admin::model::accounts::{AccountBillingAmounts, AccountModelIdentity};

use super::super::*;

/// 一次查询拿到的账号级费用口径与按完整身份拆分的模型用量
#[derive(Default)]
pub(crate) struct ForkAccountBilling {
    pub(crate) accounts: HashMap<String, AccountBillingAmounts>,
    pub(crate) models: HashMap<String, Vec<ProviderAccountModelUsageObservation>>,
}

impl ForkAccountBilling {
    pub(crate) fn take_account(&mut self, account_id: &str) -> AccountBillingAmounts {
        self.accounts.remove(account_id).unwrap_or_default()
    }

    /// 有 fork 模型行时整段替换上游模型列表；没有（范围内无已交付请求）则保留上游结果
    pub(crate) fn take_models(
        &mut self,
        account_id: &str,
        upstream: Vec<ProviderAccountModelUsageObservation>,
    ) -> Vec<ProviderAccountModelUsageObservation> {
        let mut models = self.models.remove(account_id).unwrap_or(upstream);
        models.sort_by(|a, b| {
            b.request_count
                .cmp(&a.request_count)
                .then(a.model.cmp(&b.model))
        });
        models
    }
}

/// 上游投影的模型行没有四元身份；只以聚合名作 key，不猜测请求名或计价名
pub(crate) fn plain_model_identity(model: &str) -> AccountModelIdentity {
    AccountModelIdentity {
        key: model.to_owned(),
        ..Default::default()
    }
}

/// 三种投影通过一组 `GROUPING SETS` 从同一批已交付请求派生：`grouping_* = 1`
/// 表示对应维度未参与该行聚合，调用方据此区分账号行、模型行与模型成本行。
pub(crate) async fn fork_account_billing<'e, E>(
    executor: E,
    range: ObservabilityRange,
    account_ids: &[String],
) -> StoreResult<ForkAccountBilling>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    if account_ids.is_empty() {
        return Ok(ForkAccountBilling::default());
    }
    let completed_usage = completed_usage_fact_predicate("mr");
    let statement = format!(
        "with matched as (
           select mr.provider_account_ref as account_id, mr.id as request_id,
                  coalesce(mr.upstream_model_id, mr.requested_model_id) as model,
                  jsonb_build_array(mr.requested_model_id, mr.upstream_model_id,
                                    mrb.response_model, mrb.billing_model)::text as model_key,
                  mrb.calculated_cost_amount, mrb.calculated_cost_currency,
                  mr.cost_source, mr.cost_amount, mr.cost_currency, mr.outcome,
                  mr.input_tokens, mr.output_tokens, mr.cached_input_tokens,
                  mr.cache_write_input_tokens, mr.reasoning_output_tokens,
                  mr.image_input_tokens, mr.image_output_tokens,
                  mr.image_generation_succeeded, mr.total_tokens, mr.started_at
             from model_requests mr
             left join model_request_billing mrb on mrb.model_request_id = mr.id
            where mr.provider_account_ref = any($1::text[])
              and mr.started_at >= $2 and mr.started_at < $3
              and {completed_usage}
         )
         select account_id, model, model_key, cost_currency,
                grouping(model)::integer as model_grouping,
                grouping(cost_currency)::integer as currency_grouping,
                count(request_id)::bigint as request_count,
                count(request_id) filter (where outcome = 'succeeded')::bigint as success_count,
                sum(input_tokens)::bigint as input_tokens,
                sum(output_tokens)::bigint as output_tokens,
                sum(cached_input_tokens)::bigint as cached_input_tokens,
                sum(cache_write_input_tokens)::bigint as cache_write_input_tokens,
                sum(reasoning_output_tokens)::bigint as reasoning_output_tokens,
                sum(image_input_tokens)::bigint as image_input_tokens,
                sum(image_output_tokens)::bigint as image_output_tokens,
                count(request_id) filter (where image_generation_succeeded is true)::bigint
                  as image_request_count,
                count(request_id) filter (where image_generation_succeeded is false)::bigint
                  as image_request_failed_count,
                -- 与上游窗口口径一致：优先上游给的总量，缺失时回退到输入+输出分量
                sum(coalesce(total_tokens, coalesce(input_tokens, 0) + coalesce(output_tokens, 0)))::bigint as total_tokens,
                count(request_id) filter (where cost_source = 'provider_reported')::bigint
                  as provider_reported_count,
                count(request_id) filter (where cost_source = 'calculated')::bigint
                  as calculated_count,
                count(request_id) filter (where cost_source = 'unavailable')::bigint
                  as unavailable_count,
                max(started_at) as last_used_at,
                sum(cost_amount)::text as amount,
                sum(calculated_cost_amount) filter (where calculated_cost_currency = 'USD')::text
                  as model_price_usd,
                sum(cost_amount) filter (where cost_source = 'provider_reported' and cost_currency = 'USD')::text
                  as upstream_cost_usd,
                count(request_id) filter (where calculated_cost_currency = 'USD')::bigint
                  as model_price_count,
                count(request_id) filter (where cost_source = 'provider_reported' and cost_currency = 'USD')::bigint
                  as upstream_cost_count
           from matched
          group by grouping sets (
            (account_id),
            (account_id, model, model_key),
            (account_id, model, model_key, cost_currency)
          )
          order by account_id, model_grouping desc, currency_grouping desc,
                   model nulls last, cost_currency nulls last"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(statement))
        .bind(account_ids)
        .bind(range.start)
        .bind(range.end)
        .fetch_all(executor)
        .await
        .map_err(|source| postgres_unavailable("load fork account billing", source))?;

    let mut billing = ForkAccountBilling::default();
    let mut model_costs = HashMap::<(String, String), Vec<CurrencyCostTotal>>::new();
    for row in &rows {
        let account_id: String = get(row, "account_id")?;
        let model_grouping: i32 = get(row, "model_grouping")?;
        let currency_grouping: i32 = get(row, "currency_grouping")?;
        match (model_grouping, currency_grouping) {
            (1, 1) => {
                billing
                    .accounts
                    .insert(account_id, account_billing_amounts(row)?);
            }
            (0, 1) if get::<Option<String>>(row, "model")?.is_some() => {
                billing
                    .models
                    .entry(account_id)
                    .or_default()
                    .push(fork_model_from_row(row)?);
            }
            (0, 0)
                if get::<Option<String>>(row, "model")?.is_some()
                    && get::<Option<String>>(row, "cost_currency")?.is_some() =>
            {
                model_costs
                    .entry((account_id, get(row, "model_key")?))
                    .or_default()
                    .push(cost_from_row(row)?);
            }
            (0 | 1, 0 | 1) => {}
            _ => {
                return Err(invalid("fork account billing grouping marker is invalid"));
            }
        }
    }
    for (account_id, models) in &mut billing.models {
        for model in models {
            model.costs = model_costs
                .remove(&(account_id.clone(), model.identity.key.clone()))
                .unwrap_or_default();
        }
    }
    Ok(billing)
}

fn fork_model_from_row(
    row: &sqlx::postgres::PgRow,
) -> StoreResult<ProviderAccountModelUsageObservation> {
    Ok(ProviderAccountModelUsageObservation {
        identity: account_model_identity(row)?,
        billing: account_billing_amounts(row)?,
        model: get(row, "model")?,
        request_count: unsigned(row, "request_count")?,
        success_count: unsigned(row, "success_count")?,
        input_tokens: optional_unsigned(row, "input_tokens")?,
        output_tokens: optional_unsigned(row, "output_tokens")?,
        cached_input_tokens: optional_unsigned(row, "cached_input_tokens")?,
        cache_write_input_tokens: optional_unsigned(row, "cache_write_input_tokens")?,
        reasoning_output_tokens: optional_unsigned(row, "reasoning_output_tokens")?,
        image_input_tokens: optional_unsigned(row, "image_input_tokens")?,
        image_output_tokens: optional_unsigned(row, "image_output_tokens")?,
        image_request_count: unsigned(row, "image_request_count")?,
        image_request_failed_count: unsigned(row, "image_request_failed_count")?,
        total_tokens: optional_unsigned(row, "total_tokens")?,
        cost_coverage: CostCoverage {
            provider_reported_count: unsigned(row, "provider_reported_count")?,
            calculated_count: unsigned(row, "calculated_count")?,
            unavailable_count: unsigned(row, "unavailable_count")?,
        },
        costs: Vec::new(),
        last_used_at: get(row, "last_used_at")?,
    })
}

/// `model_key` 是四元身份的 JSON 数组文本，原样作为 key 保证不同计价模型不被合并
fn account_model_identity(row: &sqlx::postgres::PgRow) -> StoreResult<AccountModelIdentity> {
    let key: String = get(row, "model_key")?;
    let [
        requested_model_id,
        upstream_model_id,
        response_model,
        billing_model,
    ]: [Option<String>; 4] = serde_json::from_str(&key)
        .map_err(|source| invalid("account model identity").with_source(source))?;
    Ok(AccountModelIdentity {
        key,
        requested_model_id,
        upstream_model_id,
        response_model,
        billing_model,
    })
}

fn account_billing_amounts(row: &sqlx::postgres::PgRow) -> StoreResult<AccountBillingAmounts> {
    let amount = |column| -> StoreResult<Option<DecimalAmount>> {
        get::<Option<String>>(row, column)?
            .map(|value| parse_decimal_amount(&value))
            .transpose()
    };
    Ok(AccountBillingAmounts {
        model_price_usd: amount("model_price_usd")?,
        upstream_cost_usd: amount("upstream_cost_usd")?,
        model_price_count: unsigned(row, "model_price_count")?,
        upstream_cost_count: unsigned(row, "upstream_cost_count")?,
    })
}
