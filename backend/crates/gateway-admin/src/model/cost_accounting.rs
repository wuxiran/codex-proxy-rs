//! 账号购买成本与每日核算。金额只做展示与核算，不参与请求计费或账号调度。
//!
//! 购买价格不区分币种，按管理员口径与美元 1:1 对比；内部用「分」和「百万分之一美元」
//! 的整数表示，避免浮点累计误差，只有最终的比值才是浮点。

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, Utc};

pub const MAX_COST_RANGE_DAYS: i64 = 366;
pub const MAX_RETIRE_BATCH: usize = 200;
/// `numeric(12,2)` 的上限，换算为分。
pub const MAX_PRICE_CENTS: i64 = 999_999_999_999;
const CHINA_OFFSET_SECONDS: i32 = 8 * 60 * 60;
const MICROS_PER_CENT: f64 = 10_000.0;

/// 核算按东八区自然日分日，与概览页的「今日」同一口径。
#[must_use]
pub fn china_day(value: DateTime<Utc>) -> NaiveDate {
    let offset = FixedOffset::east_opt(CHINA_OFFSET_SECONDS).expect("UTC+8 is a valid offset");
    value.with_timezone(&offset).date_naive()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountPurchase {
    pub account_ref: String,
    pub name: String,
    pub email: Option<String>,
    /// 只标记了下线、还没录入价格时为空。
    pub price_cents: Option<i64>,
    pub purchased_at: DateTime<Utc>,
    /// 只由管理员手动设置；额度用满或凭据失效都不会自动下线。
    pub retired_at: Option<DateTime<Utc>>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetAccountPurchase {
    pub account_id: String,
    /// 外层为空保留原价，内层为空清除价格。
    pub price_cents: Option<Option<i64>>,
    pub purchased_at: Option<DateTime<Utc>>,
    pub note: Option<Option<String>>,
}

/// 账号当前状态，仅用于提示「可以下线了」；账号已删除时为空。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountLiveState {
    pub enabled: bool,
    pub credential_ready: bool,
    pub quota_exhausted: bool,
    pub plan_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyAccountUsage {
    pub day: NaiveDate,
    pub account_ref: String,
    pub usage_micros: i64,
    pub request_count: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountCostRow {
    pub purchase: AccountPurchase,
    pub live: Option<AccountLiveState>,
    /// 查询区间内的跑出金额。
    pub usage_micros: i64,
    pub request_count: u64,
    pub total_tokens: u64,
    pub cost_per_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CostDay {
    pub day: NaiveDate,
    /// 当日购入且已录价的账号数。
    pub purchased_count: u32,
    pub spend_cents: i64,
    pub usage_micros: i64,
    pub request_count: u64,
    pub total_tokens: u64,
    pub cost_per_usd: Option<f64>,
    /// 区间起点累计到当日；号通常当天买当天跑完，但跨日的号靠累计值才看得准。
    pub cumulative_cost_per_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CostTotals {
    pub purchased_count: u32,
    pub spend_cents: i64,
    pub usage_micros: i64,
    pub request_count: u64,
    pub total_tokens: u64,
    pub cost_per_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CostReport {
    pub days: Vec<CostDay>,
    pub totals: CostTotals,
}

/// 每 1 美元跑出对应的投入；没有跑出时无法计算，不用 0 或无穷大冒充。
#[must_use]
pub fn cost_per_usd(spend_cents: i64, usage_micros: i64) -> Option<f64> {
    (usage_micros > 0 && spend_cents >= 0)
        .then(|| spend_cents as f64 * MICROS_PER_CENT / usage_micros as f64)
        .filter(|value| value.is_finite())
}

/// 投入记在购买当日，跑出记在实际发生的当日；没有任何数据的日期也保留一行，趋势图不断档。
#[must_use]
pub fn build_cost_report(
    from: NaiveDate,
    to: NaiveDate,
    purchases: &[AccountPurchase],
    usage: &[DailyAccountUsage],
) -> CostReport {
    let mut days = Vec::new();
    let mut day = from;
    let mut totals = CostTotals {
        purchased_count: 0,
        spend_cents: 0,
        usage_micros: 0,
        request_count: 0,
        total_tokens: 0,
        cost_per_usd: None,
    };
    while day <= to {
        let mut row = CostDay {
            day,
            purchased_count: 0,
            spend_cents: 0,
            usage_micros: 0,
            request_count: 0,
            total_tokens: 0,
            cost_per_usd: None,
            cumulative_cost_per_usd: None,
        };
        for purchase in purchases {
            if let Some(price) = purchase.price_cents
                && china_day(purchase.purchased_at) == day
            {
                row.purchased_count += 1;
                row.spend_cents = row.spend_cents.saturating_add(price);
            }
        }
        for item in usage.iter().filter(|item| item.day == day) {
            row.usage_micros = row.usage_micros.saturating_add(item.usage_micros);
            row.request_count = row.request_count.saturating_add(item.request_count);
            row.total_tokens = row.total_tokens.saturating_add(item.total_tokens);
        }
        row.cost_per_usd = cost_per_usd(row.spend_cents, row.usage_micros);
        totals.purchased_count += row.purchased_count;
        totals.spend_cents = totals.spend_cents.saturating_add(row.spend_cents);
        totals.usage_micros = totals.usage_micros.saturating_add(row.usage_micros);
        totals.request_count = totals.request_count.saturating_add(row.request_count);
        totals.total_tokens = totals.total_tokens.saturating_add(row.total_tokens);
        row.cumulative_cost_per_usd = cost_per_usd(totals.spend_cents, totals.usage_micros);
        days.push(row);
        let Some(next) = day.checked_add_signed(Duration::days(1)) else {
            break;
        };
        day = next;
    }
    totals.cost_per_usd = cost_per_usd(totals.spend_cents, totals.usage_micros);
    CostReport { days, totals }
}

/// 单号核算用它在区间内的全部跑出，不按购买日截断：价格是为整个号付的。
#[must_use]
pub fn build_account_rows(
    purchases: Vec<(AccountPurchase, Option<AccountLiveState>)>,
    usage: &[DailyAccountUsage],
) -> Vec<AccountCostRow> {
    purchases
        .into_iter()
        .map(|(purchase, live)| {
            let mut row = AccountCostRow {
                live,
                usage_micros: 0,
                request_count: 0,
                total_tokens: 0,
                cost_per_usd: None,
                purchase,
            };
            for item in usage
                .iter()
                .filter(|item| item.account_ref == row.purchase.account_ref)
            {
                row.usage_micros = row.usage_micros.saturating_add(item.usage_micros);
                row.request_count = row.request_count.saturating_add(item.request_count);
                row.total_tokens = row.total_tokens.saturating_add(item.total_tokens);
            }
            row.cost_per_usd = row
                .purchase
                .price_cents
                .and_then(|price| cost_per_usd(price, row.usage_micros));
            row
        })
        .collect()
}
