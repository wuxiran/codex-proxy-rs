//! fork：计价身份子表 model_request_billing（fork 迁移 9001）的终结写入与历史回填。

use gateway_core::engine::ExecutionStore;
use gateway_core::metering::CalculatedCost;
use gateway_store::postgres::{ObservabilityRepository as _, PgExecutionStore};
use serde_json::{Value, json};

use super::{
    TestDatabase,
    execution::{seed_running_request, successful_core_finalization},
};

#[tokio::test]
async fn model_billing_identity_and_both_costs_survive_finalization() {
    use gateway_core::metering::{ModelBillingObservation, ProviderReportedCost};
    let Some(database) = TestDatabase::create("billing_identity_persistence").await else {
        return;
    };
    seed_running_request(&database.pool, "req_billing_identity")
        .await
        .unwrap();
    sqlx::query("update model_requests set requested_model_id = 'gpt-6-astra', upstream_model_id = 'gpt-6-astra' where id = 'req_billing_identity'")
        .execute(&database.pool).await.unwrap();
    let store = PgExecutionStore::new(database.pool.clone());
    let mut finalization = successful_core_finalization("req_billing_identity");
    finalization.cost = ProviderReportedCost::from_usd_ticks(0)
        .unwrap()
        .into_estimate();
    finalization.billing = ModelBillingObservation {
        response_model: Some("gpt-5.6-luna".to_owned()),
        billing_model: Some("gpt-5.6-luna".to_owned()),
        calculated_cost: Some(CalculatedCost::from_usd_ticks(12345).unwrap().total()),
    };
    let trace = json!({"request_attribution": {
        "requested_model":"gpt-6-astra", "route_model":"gpt-6-astra",
        "response_model":"gpt-5.6-luna", "billing_model":"gpt-5.6-luna",
        "provider_account_id":"acct_mock", "outbound_proxy_endpoint":"http://proxy.example:8080/"
    }});
    finalization.diagnostic_trace_json = Some(trace.to_string());
    ExecutionStore::finalize_model_request(&store, finalization)
        .await
        .unwrap();
    // 计价身份/本地成本已迁子表 model_request_billing（fork 9001），JOIN 取回。
    let row: Value = sqlx::query_scalar("select jsonb_build_object('requested', mr.requested_model_id, 'upstream', mr.upstream_model_id, 'response', mrb.response_model, 'billing', mrb.billing_model, 'calculated', mrb.calculated_cost_amount::text, 'actual', mr.cost_amount::text, 'source', mr.cost_source) from model_requests mr left join model_request_billing mrb on mrb.model_request_id = mr.id where mr.id = 'req_billing_identity'")
        .fetch_one(&database.pool).await.unwrap();
    assert_eq!(
        row,
        json!({"requested":"gpt-6-astra","upstream":"gpt-6-astra","response":"gpt-5.6-luna","billing":"gpt-5.6-luna","calculated":"0.0000012345","actual":"0.0000000000","source":"provider_reported"})
    );
    let detail = super::observability_repository(&database.pool)
        .usage_record_detail("req_billing_identity")
        .await
        .unwrap();
    assert_eq!(detail.trace, Some(trace));
    database.close().await;
}

#[tokio::test]
async fn billing_migration_preserves_historical_cost_provenance_without_guessing_models() {
    let Some(database) = TestDatabase::create("billing_history_migration").await else {
        return;
    };
    for id in ["req_history_calculated", "req_history_reported"] {
        seed_running_request(&database.pool, id).await.unwrap();
    }
    sqlx::query("update model_requests set cost_source = case when id = 'req_history_calculated' then 'calculated' else 'provider_reported' end, cost_amount = 1.25, cost_currency = 'USD'")
        .execute(&database.pool).await.unwrap();
    // 计价列迁子表后，回填逻辑在 9001；重建子表并重跑迁移，验证仅 cost_source='calculated' 的历史金额被迁入。
    sqlx::raw_sql("drop table model_request_billing")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../../migrations/9001_model_request_billing.sql"
    ))
    .execute(&database.pool)
    .await
    .unwrap();
    let rows: Vec<Value> = sqlx::query_scalar("select jsonb_build_object('id', mr.id, 'price', mrb.calculated_cost_amount::text, 'amount', mr.cost_amount::text, 'source', mr.cost_source, 'response', mrb.response_model, 'billing', mrb.billing_model) from model_requests mr left join model_request_billing mrb on mrb.model_request_id = mr.id order by mr.id")
        .fetch_all(&database.pool).await.unwrap();
    assert_eq!(
        rows[0],
        json!({"id":"req_history_calculated","price":"1.2500000000","amount":"1.2500000000","source":"calculated","response":null,"billing":null})
    );
    assert_eq!(
        rows[1],
        json!({"id":"req_history_reported","price":null,"amount":"1.2500000000","source":"provider_reported","response":null,"billing":null})
    );
    database.close().await;
}
