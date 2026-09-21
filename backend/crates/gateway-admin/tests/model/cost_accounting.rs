use chrono::{DateTime, NaiveDate, Utc};
use gateway_admin::model::cost_accounting::*;

fn at(value: &str) -> DateTime<Utc> {
    value.parse().unwrap()
}

fn day(value: &str) -> NaiveDate {
    value.parse().unwrap()
}

fn purchase(id: &str, price_cents: Option<i64>, purchased_at: &str) -> AccountPurchase {
    AccountPurchase {
        account_ref: id.to_owned(),
        name: id.to_owned(),
        email: None,
        price_cents,
        purchased_at: at(purchased_at),
        retired_at: None,
        note: None,
    }
}

fn usage(date: &str, id: &str, usd: i64) -> DailyAccountUsage {
    DailyAccountUsage {
        day: day(date),
        account_ref: id.to_owned(),
        usage_micros: usd * 1_000_000,
        request_count: 10,
        total_tokens: 1_000,
    }
}

#[test]
fn days_are_cut_at_utc_plus_eight() {
    assert_eq!(china_day(at("2026-09-20T15:59:59Z")), day("2026-09-20"));
    assert_eq!(china_day(at("2026-09-20T16:00:00Z")), day("2026-09-21"));
}

#[test]
fn spend_lands_on_the_purchase_day_and_usage_on_the_day_it_happened() {
    let purchases = [
        purchase("a", Some(5_000), "2026-09-21T02:00:00Z"),
        // 东八区已是 22 日凌晨。
        purchase("b", Some(11_000), "2026-09-21T17:00:00Z"),
        purchase("unpriced", None, "2026-09-21T02:00:00Z"),
    ];
    let usage = [
        usage("2026-09-21", "a", 400),
        usage("2026-09-22", "a", 100),
        usage("2026-09-22", "b", 500),
    ];
    let report = build_cost_report(day("2026-09-20"), day("2026-09-22"), &purchases, &usage);
    assert_eq!(report.days.len(), 3);
    let [empty, first, second] = &report.days[..] else {
        panic!("three days")
    };
    // 没有数据的日期也保留一行，趋势图不断档；没有跑出就没有比值。
    assert_eq!((empty.spend_cents, empty.usage_micros), (0, 0));
    assert!(empty.cost_per_usd.is_none());
    assert_eq!((first.purchased_count, first.spend_cents), (1, 5_000));
    assert!((first.cost_per_usd.unwrap() - 0.125).abs() < 1e-12);
    assert_eq!((second.purchased_count, second.spend_cents), (1, 11_000));
    assert!((second.cost_per_usd.unwrap() - 110.0 / 600.0).abs() < 1e-12);
    // 累计值把跨日的号摊平：160 投入 / 1000 跑出。
    assert!((second.cumulative_cost_per_usd.unwrap() - 0.16).abs() < 1e-12);
    assert_eq!(report.totals.purchased_count, 2);
    assert_eq!(report.totals.spend_cents, 16_000);
    assert!((report.totals.cost_per_usd.unwrap() - 0.16).abs() < 1e-12);
    assert_eq!(report.totals.request_count, 30);
}

#[test]
fn todays_real_numbers_reconcile_with_the_manual_calculation() {
    // 2026-09-21：10 个号共 524.58，跑出 $4,405.85 → 每 1 刀约 0.119。
    let prices = [5_150, 5_768, 5_768, 11_000, 4_636, 4_636, 11_000, 4_500];
    let purchases: Vec<_> = prices
        .iter()
        .enumerate()
        .map(|(index, price)| {
            purchase(
                &format!("acct_{index}"),
                Some(*price),
                "2026-09-21T03:00:00Z",
            )
        })
        .collect();
    let usage = [DailyAccountUsage {
        day: day("2026-09-21"),
        account_ref: "acct_0".to_owned(),
        usage_micros: 4_405_850_000,
        request_count: 1,
        total_tokens: 1,
    }];
    let report = build_cost_report(day("2026-09-21"), day("2026-09-21"), &purchases, &usage);
    assert_eq!(report.totals.spend_cents, 52_458);
    assert!((report.totals.cost_per_usd.unwrap() - 0.119_064).abs() < 1e-5);
}

#[test]
fn account_rows_use_all_usage_in_range_and_need_a_price_for_a_ratio() {
    let rows = build_account_rows(
        vec![
            (purchase("a", Some(5_000), "2026-09-21T02:00:00Z"), None),
            (purchase("unpriced", None, "2026-09-21T02:00:00Z"), None),
            (purchase("idle", Some(5_000), "2026-09-21T02:00:00Z"), None),
        ],
        &[
            usage("2026-09-21", "a", 400),
            usage("2026-09-22", "a", 100),
            usage("2026-09-22", "unpriced", 50),
        ],
    );
    assert_eq!(rows[0].usage_micros, 500_000_000);
    assert!((rows[0].cost_per_usd.unwrap() - 0.1).abs() < 1e-12);
    assert_eq!(rows[1].usage_micros, 50_000_000);
    assert!(rows[1].cost_per_usd.is_none());
    assert!(rows[2].cost_per_usd.is_none());
}

#[test]
fn ratio_is_never_invented_from_zero_or_negative_inputs() {
    assert!(cost_per_usd(5_000, 0).is_none());
    assert!(cost_per_usd(-1, 1_000_000).is_none());
    assert_eq!(cost_per_usd(0, 1_000_000), Some(0.0));
}
