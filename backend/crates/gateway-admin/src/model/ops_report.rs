//! 经营日报：CPR 号池投入与 sub2api 消费、收款的每日汇总。
//!
//! 平台 1 元 = 1 刀，sub2api 的「刀」即客户人民币。英雄套餐订阅按 1/5 折算（调整后消费）。

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// 一次采集得到的单日原始数据（区间为北京时间自然日）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpsDayFacts {
    /// 当天入库的 CPR 账号 ID。
    pub new_account_ids: Vec<String>,
    /// 当天买入的账号成本（买入时间缺省按入库时间）。
    pub purchases: Vec<OpsPurchase>,
    pub cpr: CprDayFacts,
    /// sub2api 未配置时为 `None`。
    pub sub2api: Option<Sub2apiDayFacts>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsPurchase {
    pub account_id: String,
    pub amount: f64,
    /// `CNY` 或 `USD`。
    pub currency: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CprDayFacts {
    pub requests: i64,
    pub failed_requests: i64,
    /// 按模型官方价格计算的美元金额。
    pub official_usd: f64,
}

/// sub2api 单日消费；金额单位为平台刀（= 人民币）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sub2apiDayFacts {
    pub active_users: i64,
    pub requests: i64,
    /// 标准价（total_cost）。
    pub standard: f64,
    /// 实际扣费（actual_cost）。
    pub actual: f64,
    /// 调整后：英雄套餐订阅 ÷5，其余按实际扣费。
    pub adjusted: f64,
    /// 仅 Codex（OpenAI 平台分组，不含生图）。
    pub codex: Sub2apiSlice,
    /// Codex 中由 CPR 上游账号承接的部分。
    pub codex_via_cpr: Sub2apiSlice,
    /// 在线收款（扣除退款）。
    pub payments: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sub2apiSlice {
    pub requests: i64,
    pub standard: f64,
    pub adjusted: f64,
}

/// 落盘的单日快照。新入库账号与买入成本跨刷新累积，账号被删也不会丢掉当天成本。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsDayRecord {
    pub day: NaiveDate,
    #[serde(default)]
    pub new_accounts: BTreeSet<String>,
    #[serde(default)]
    pub purchases: BTreeMap<String, OpsPurchaseAmount>,
    #[serde(default)]
    pub cpr: CprDayFacts,
    #[serde(default)]
    pub sub2api: Option<Sub2apiDayFacts>,
    pub refreshed_at: DateTime<Utc>,
    /// 当天结束后又刷新过一次，数据不再变化。
    #[serde(default)]
    pub finalized: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsPurchaseAmount {
    pub amount: f64,
    pub currency: String,
}

impl OpsDayRecord {
    #[must_use]
    pub fn new(day: NaiveDate, refreshed_at: DateTime<Utc>) -> Self {
        Self {
            day,
            new_accounts: BTreeSet::new(),
            purchases: BTreeMap::new(),
            cpr: CprDayFacts::default(),
            sub2api: None,
            refreshed_at,
            finalized: false,
        }
    }

    /// 合并一次采集：账号与买入累积（同一账号以最新金额为准），其余指标整体替换。
    pub fn merge(&mut self, facts: OpsDayFacts, refreshed_at: DateTime<Utc>, finalized: bool) {
        self.new_accounts.extend(facts.new_account_ids);
        for purchase in facts.purchases {
            self.purchases.insert(
                purchase.account_id,
                OpsPurchaseAmount {
                    amount: purchase.amount,
                    currency: purchase.currency,
                },
            );
        }
        self.cpr = facts.cpr;
        if facts.sub2api.is_some() {
            self.sub2api = facts.sub2api;
        }
        self.refreshed_at = refreshed_at;
        self.finalized = finalized;
    }

    fn purchase_total(&self, currency: &str) -> f64 {
        self.purchases
            .values()
            .filter(|purchase| purchase.currency == currency)
            .map(|purchase| purchase.amount)
            .sum()
    }

    /// 管理端展示行：派生买入合计与毛利。
    #[must_use]
    pub fn view(&self) -> OpsDayView {
        let purchase_cny = round2(self.purchase_total("CNY"));
        let purchase_usd = round2(self.purchase_total("USD"));
        let gross_profit = self
            .sub2api
            .as_ref()
            .map(|sub2api| round2(sub2api.codex_via_cpr.adjusted - purchase_cny));
        OpsDayView {
            day: self.day,
            new_accounts: self.new_accounts.len(),
            purchased_accounts: self.purchases.len(),
            purchase_cny,
            purchase_usd,
            cpr: self.cpr.clone(),
            sub2api: self.sub2api.clone(),
            gross_profit,
            refreshed_at: self.refreshed_at,
            finalized: self.finalized,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsDayView {
    pub day: NaiveDate,
    pub new_accounts: usize,
    pub purchased_accounts: usize,
    pub purchase_cny: f64,
    pub purchase_usd: f64,
    pub cpr: CprDayFacts,
    pub sub2api: Option<Sub2apiDayFacts>,
    /// Codex 经 CPR 的调整后消费 − 当天人民币买入成本；sub2api 未配置时为 `None`。
    pub gross_profit: Option<f64>,
    pub refreshed_at: DateTime<Utc>,
    pub finalized: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsReport {
    /// 是否已配置 sub2api 只读数据源。
    pub sub2api_configured: bool,
    /// 新日期在前。
    pub days: Vec<OpsDayView>,
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
