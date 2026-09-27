//! 经营日报快照合并与派生口径。

use chrono::{NaiveDate, Utc};
use gateway_admin::model::ops_report::{
    CprDayFacts, OpsDayFacts, OpsDayRecord, OpsPurchase, OpsPurchaseAmount, OpsPurchaseTicket,
    Sub2apiDayFacts, Sub2apiSlice, sync_purchases,
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

fn cny(amount: f64) -> OpsPurchaseAmount {
    OpsPurchaseAmount {
        amount,
        currency: "CNY".into(),
    }
}

fn ticket(account_id: &str, day: NaiveDate, amount: Option<f64>) -> OpsPurchaseTicket {
    OpsPurchaseTicket {
        account_id: account_id.into(),
        day,
        amount: amount.map(cny),
    }
}

#[test]
fn sync_purchases_follows_tickets_into_finalized_days() {
    let now = Utc::now();
    let earlier = day().pred_opt().unwrap();
    let mut records = vec![
        OpsDayRecord::new(day(), now),
        OpsDayRecord::new(earlier, now),
    ];
    for record in &mut records {
        record.finalized = true;
    }
    records[0].purchases.insert("moved".into(), cny(55.0));
    records[0].purchases.insert("cleared".into(), cny(20.0));
    records[0].purchases.insert("deleted".into(), cny(30.0));

    let tickets = vec![
        // 定稿后才补录的成本。
        ticket("late", day(), Some(55.0)),
        // 买入时间改到了前一天。
        ticket("moved", earlier, Some(55.0)),
        // 金额被清掉。
        ticket("cleared", day(), None),
        // 归属日早于快照范围：不计入任何一天。
        ticket(
            "ancient",
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            Some(99.0),
        ),
    ];
    assert!(sync_purchases(&mut records, &tickets));

    let today = records[0].view();
    assert_eq!(today.purchased_accounts, 2);
    assert_eq!(today.purchase_cny, 85.0);
    assert!(records[0].purchases.contains_key("late"));
    assert!(records[0].purchases.contains_key("deleted"));
    let previous = records[1].view();
    assert_eq!(previous.purchased_accounts, 1);
    assert_eq!(previous.purchase_cny, 55.0);

    assert!(!sync_purchases(&mut records, &tickets));
}
