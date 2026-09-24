//! 经营日报快照合并与派生口径。

use chrono::{NaiveDate, Utc};
use gateway_admin::model::ops_report::{
    CprDayFacts, OpsDayFacts, OpsDayRecord, OpsPurchase, Sub2apiDayFacts, Sub2apiSlice,
};

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 24).unwrap()
}

#[test]
fn merge_keeps_deleted_accounts_and_their_cost() {
    let now = Utc::now();
    let mut record = OpsDayRecord::new(day(), now);
    record.merge(
        OpsDayFacts {
            new_account_ids: vec!["a".into(), "b".into()],
            purchases: vec![
                OpsPurchase {
                    account_id: "a".into(),
                    amount: 55.0,
                    currency: "CNY".into(),
                },
                OpsPurchase {
                    account_id: "b".into(),
                    amount: 10.0,
                    currency: "USD".into(),
                },
            ],
            cpr: CprDayFacts::default(),
            sub2api: Some(Sub2apiDayFacts {
                codex_via_cpr: Sub2apiSlice {
                    requests: 1,
                    standard: 500.0,
                    adjusted: 100.0,
                },
                ..Sub2apiDayFacts::default()
            }),
        },
        now,
        false,
    );
    // 账号 a 被删：下一轮查不到它，但当天成本仍在；sub2api 暂不可用时保留上次数据。
    record.merge(
        OpsDayFacts {
            new_account_ids: vec!["b".into()],
            purchases: vec![],
            cpr: CprDayFacts::default(),
            sub2api: None,
        },
        now,
        true,
    );
    let view = record.view();
    assert_eq!(view.new_accounts, 2);
    assert_eq!(view.purchase_cny, 55.0);
    assert_eq!(view.purchase_usd, 10.0);
    assert_eq!(view.gross_profit, Some(45.0));
    assert!(view.finalized);
}
