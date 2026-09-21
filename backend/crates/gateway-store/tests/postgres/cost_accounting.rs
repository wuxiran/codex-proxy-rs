use chrono::{DateTime, Duration, NaiveDate, Utc};
use gateway_admin::{
    model::{
        PageSize,
        accounts::AccountListQuery,
        cost_accounting::{SetAccountPurchase, china_day},
    },
    ports::{
        cost_accounting::CostAccountingStore,
        store::{AccountStore, AdminStoreErrorKind},
    },
};
use gateway_store::postgres::{
    PgCostAccountingRepository, PgProviderAccountRepository, ProviderAccountRepository,
};

use super::{TestDatabase, admin_account_store, provider_accounts::account};

fn price(account_id: &str, cents: Option<i64>) -> SetAccountPurchase {
    SetAccountPurchase {
        account_id: account_id.to_owned(),
        price_cents: Some(cents),
        purchased_at: None,
        note: None,
    }
}

async fn log_request(pool: &sqlx::PgPool, account_ref: &str, started_at: DateTime<Utc>, usd: &str) {
    // 不写 provider_account_id：日志靠 provider_account_ref 归属，账号删除后依然成立。
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, config_revision, protocol, operation, endpoint,
           client_transport, requested_model_id, provider_kind,
           provider_account_ref, upstream_model_id, upstream_transport, attempt_count,
           upstream_send_state, downstream_committed_at, outcome, client_status_code,
           upstream_status_code, total_tokens, cost_source, cost_amount, cost_currency,
           calculated_cost_amount, calculated_cost_currency,
           started_at, deadline_at, completed_at, routing_scope
         ) values (
           $1, 'key-cost', 1, 'openai', 'responses', '/v1/responses',
           'http_sse', 'gpt-cost', 'openai', $2, 'gpt-cost', 'http_sse', 1,
           'sent', $3, 'succeeded', 200, 200, 1000,
           'calculated', $4::numeric, 'USD', $4::numeric, 'USD',
           $3, $3 + interval '5 minutes', $3, 'all'
         )",
    )
    .bind(format!("req_{}", uuid::Uuid::now_v7().simple()))
    .bind(account_ref)
    .bind(started_at)
    .bind(usd)
    .execute(pool)
    .await
    .expect("insert request log");
}

fn list_query(hide_retired: bool) -> AccountListQuery {
    AccountListQuery {
        hide_retired,
        page: 1,
        page_size: PageSize::new(50).unwrap(),
        provider_kind: None,
        group_filter: None,
        search: None,
        status: None,
        sort: None,
    }
}

#[tokio::test]
async fn purchases_outlive_accounts_and_daily_usage_is_cut_at_utc_plus_eight() {
    let Some(database) = TestDatabase::create("cost_accounting").await else {
        return;
    };
    let accounts = PgProviderAccountRepository::new(database.pool.clone());
    for id in ["acct_team", "acct_other"] {
        accounts
            .insert_provider_account(account(id, id))
            .await
            .unwrap();
    }
    let store = PgCostAccountingRepository::new(database.pool.clone());

    let missing = store.set_purchase(price("acct_missing", Some(100))).await;
    assert_eq!(missing.unwrap_err().kind(), AdminStoreErrorKind::NotFound);

    let saved = store
        .set_purchase(price("acct_team", Some(5_150)))
        .await
        .unwrap();
    assert_eq!(saved.price_cents, Some(5_150));
    assert_eq!(saved.name, "acct_team");
    // 省略的字段保持不变，显式的空值才清除。
    let noted = store
        .set_purchase(SetAccountPurchase {
            account_id: "acct_team".to_owned(),
            price_cents: None,
            purchased_at: None,
            note: Some(Some("第一批".to_owned())),
        })
        .await
        .unwrap();
    assert_eq!(
        (noted.price_cents, noted.note.as_deref()),
        (Some(5_150), Some("第一批"))
    );

    // 同一个东八区自然日的两端，以及前一天的最后一刻。
    let today = china_day(Utc::now());
    let day_start: DateTime<Utc> =
        today.and_hms_opt(0, 0, 0).unwrap().and_utc() - Duration::hours(8);
    log_request(&database.pool, "acct_team", day_start, "100.25").await;
    log_request(
        &database.pool,
        "acct_team",
        day_start + Duration::hours(23),
        "50",
    )
    .await;
    log_request(
        &database.pool,
        "acct_team",
        day_start - Duration::seconds(1),
        "7",
    )
    .await;
    // 没有购买记录的账号不进入核算。
    log_request(&database.pool, "acct_other", day_start, "300").await;

    let yesterday = today - Duration::days(1);
    store.refresh_daily(yesterday, today).await.unwrap();
    store.refresh_daily(yesterday, today).await.unwrap();
    let usage = store.daily_usage(yesterday, today).await.unwrap();
    let summary: Vec<(NaiveDate, &str, i64, u64)> = usage
        .iter()
        .map(|item| {
            (
                item.day,
                item.account_ref.as_str(),
                item.usage_micros,
                item.request_count,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (yesterday, "acct_team", 7_000_000, 1),
            (today, "acct_team", 150_250_000, 2),
        ]
    );

    // 账号删除后，购买记录与已落表的每日金额都还在；现状为空。
    sqlx::query("delete from provider_accounts where id = 'acct_team'")
        .execute(&database.pool)
        .await
        .unwrap();
    let purchases = store.purchases().await.unwrap();
    assert_eq!(purchases.len(), 1);
    assert!(purchases[0].1.is_none());
    assert_eq!(purchases[0].0.price_cents, Some(5_150));
    assert_eq!(store.daily_usage(today, today).await.unwrap().len(), 1);
    // 号没了仍然可以改价。
    let repriced = store.set_purchase(price("acct_team", None)).await.unwrap();
    assert!(repriced.price_cents.is_none());
}

#[tokio::test]
async fn snapshots_outside_the_retention_window_are_never_recomputed() {
    let Some(database) = TestDatabase::create("cost_retention").await else {
        return;
    };
    let accounts = PgProviderAccountRepository::new(database.pool.clone());
    accounts
        .insert_provider_account(account("acct_old", "acct_old"))
        .await
        .unwrap();
    let store = PgCostAccountingRepository::new(database.pool.clone());
    store
        .set_purchase(price("acct_old", Some(5_000)))
        .await
        .unwrap();
    let old_day = china_day(Utc::now()) - Duration::days(60);
    sqlx::query(
        "insert into account_cost_daily (day, account_ref, usage_usd, request_count, total_tokens)
         values ($1, 'acct_old', 480.5, 3000, 1)",
    )
    .bind(old_day)
    .execute(&database.pool)
    .await
    .unwrap();
    // 保留期外即使还残留日志，也不拿残缺数据覆盖当年的结果。
    log_request(
        &database.pool,
        "acct_old",
        old_day.and_hms_opt(4, 0, 0).unwrap().and_utc(),
        "1",
    )
    .await;
    store.refresh_daily(old_day, old_day).await.unwrap();
    let usage = store.daily_usage(old_day, old_day).await.unwrap();
    assert_eq!(usage[0].usage_micros, 480_500_000);
}

#[tokio::test]
async fn manual_retirement_hides_accounts_from_the_directory_without_touching_them() {
    let Some(database) = TestDatabase::create("cost_retire").await else {
        return;
    };
    let accounts = PgProviderAccountRepository::new(database.pool.clone());
    for id in ["acct_live", "acct_done"] {
        accounts
            .insert_provider_account(account(id, id))
            .await
            .unwrap();
    }
    let store = PgCostAccountingRepository::new(database.pool.clone());
    let admin = admin_account_store(&database.pool);

    // 没有购买记录的账号也能直接下线；未知账号被忽略。
    let changed = store
        .set_retired(
            &["acct_done".to_owned(), "acct_missing".to_owned()],
            Some(Utc::now()),
        )
        .await
        .unwrap();
    assert_eq!(changed, ["acct_done"]);
    let ids = |page: gateway_admin::model::accounts::AccountPage| -> Vec<String> {
        page.items.into_iter().map(|item| item.account.id).collect()
    };
    let visible = admin
        .list_accounts(list_query(true), Default::default())
        .await
        .unwrap();
    assert_eq!(visible.total, 1);
    assert_eq!(ids(visible), ["acct_live"]);
    let all = admin
        .list_accounts(list_query(false), Default::default())
        .await
        .unwrap();
    assert_eq!(all.total, 2);
    // 下线只是标记：账号仍然启用。
    let enabled: bool =
        sqlx::query_scalar("select enabled from provider_accounts where id = 'acct_done'")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert!(enabled);

    // 恢复上线：回到列表；对从未记录的账号恢复不会凭空建记录。
    let restored = store
        .set_retired(&["acct_done".to_owned(), "acct_live".to_owned()], None)
        .await
        .unwrap();
    assert_eq!(restored, ["acct_done"]);
    assert_eq!(
        admin
            .list_accounts(list_query(true), Default::default())
            .await
            .unwrap()
            .total,
        2
    );
    assert_eq!(store.purchases().await.unwrap().len(), 1);
}
