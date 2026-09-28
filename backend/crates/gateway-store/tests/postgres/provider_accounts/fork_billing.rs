//! fork：计价身份子表 model_request_billing（fork 迁移 9001）的账号用量聚合。

use super::*;

#[tokio::test]
async fn account_billing_groups_full_identity_and_never_uses_estimate_as_actual() {
    let Some(database) = TestDatabase::create("account_billing_identity").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    repository
        .insert_provider_account(account("acct_billing", "user-billing"))
        .await
        .unwrap();
    let now = Utc::now();
    for (id, model, calculated, actual) in [
        ("req_astra", Some("gpt-6-astra"), "2.00", Some("1.50")),
        ("req_luna", Some("gpt-5.6-luna"), "0.20", None),
        ("req_luna_zero", Some("gpt-5.6-luna"), "0.20", Some("0")),
        ("req_history", None, "0.10", None),
    ] {
        seed_model_request(
            &database.pool,
            ModelRequestSeed {
                request_id: id,
                account_id: "acct_billing",
                provider_kind: "openai",
                model: "gpt-6-astra",
                total_tokens: 100,
                cost_amount: "0",
                started_at: now - TimeDelta::minutes(1),
            },
        )
        .await
        .unwrap();
        // 计价身份/本地成本已迁子表 model_request_billing（fork 9001）；父表只留 cost_source/cost_amount。
        sqlx::query(
            "update model_requests set cost_source = $2, cost_amount = $3::numeric where id = $1",
        )
        .bind(id)
        .bind(if actual.is_some() {
            "provider_reported"
        } else {
            "calculated"
        })
        .bind(actual.unwrap_or(calculated))
        .execute(&database.pool)
        .await
        .unwrap();
        sqlx::query("insert into model_request_billing (model_request_id, response_model, billing_model, calculated_cost_amount, calculated_cost_currency) values ($1, $2, $2, $3::numeric, 'USD') on conflict (model_request_id) do update set response_model = excluded.response_model, billing_model = excluded.billing_model, calculated_cost_amount = excluded.calculated_cost_amount, calculated_cost_currency = excluded.calculated_cost_currency")
            .bind(id).bind(model).bind(calculated)
            .execute(&database.pool).await.unwrap();
    }
    let store = admin_account_store(&database.pool);
    let range = TimeRange {
        start: now - TimeDelta::hours(1),
        end: now,
    };
    let window = store
        .load_account_usage_by_windows(&[AccountUsageWindowQuery {
            account_id: "acct_billing".to_owned(),
            key: "test".to_owned(),
            range,
        }])
        .await
        .unwrap()
        .remove(0)
        .usage;
    let rolling = store
        .load_account_usage(range, &["acct_billing".to_owned()])
        .await
        .unwrap()
        .remove(0);
    for usage in [window, rolling] {
        assert_eq!(usage.models.len(), 3);
        assert_eq!(
            usage.billing.model_price_usd.as_ref().unwrap().as_str(),
            "2.5"
        );
        assert_eq!(
            usage.billing.upstream_cost_usd.as_ref().unwrap().as_str(),
            "1.5"
        );
        assert_eq!(
            (
                usage.billing.model_price_count,
                usage.billing.upstream_cost_count
            ),
            (4, 2)
        );
        let luna = usage
            .models
            .iter()
            .find(|row| row.identity.billing_model.as_deref() == Some("gpt-5.6-luna"))
            .unwrap();
        assert_eq!(luna.request_count, 2);
        assert_eq!(
            luna.identity.requested_model_id.as_deref(),
            Some("gpt-6-astra")
        );
        assert_eq!(
            luna.identity.upstream_model_id.as_deref(),
            Some("gpt-6-astra")
        );
        assert_eq!(
            luna.billing.model_price_usd.as_ref().unwrap().as_str(),
            "0.4"
        );
        assert_eq!(
            luna.billing.upstream_cost_usd.as_ref().unwrap().as_str(),
            "0"
        );
        assert_eq!(luna.billing.upstream_cost_count, 1);
        let history = usage
            .models
            .iter()
            .find(|row| row.identity.billing_model.is_none())
            .unwrap();
        assert!(history.identity.response_model.is_none());
        assert!(history.billing.upstream_cost_usd.is_none());
    }
    database.close().await;
}
