//! fork：上游设置 SQL 参数调整后，日志门控仍应按原值落库并进入快照。

use gateway_store::postgres::{
    PgRuntimeSettingsRepository, PgRuntimeSnapshotRepository, RuntimeSettingsRepository,
    RuntimeSnapshotRepository,
};

use super::{TestDatabase, runtime_settings::settings_with_margin};

#[tokio::test]
async fn request_log_policy_round_trips_with_runtime_settings() {
    let Some(database) = TestDatabase::create("fork_log").await else {
        return;
    };
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    let mut update = settings_with_margin(3600);
    update.values.request_log_enabled = false;
    update.values.request_log_test_key_id = Some("capture-test-key".to_owned());
    repository.update_runtime_settings(update).await.unwrap();

    let stored = repository.load_runtime_settings().await.unwrap();
    assert!(!stored.values.request_log_enabled);
    assert_eq!(
        stored.values.request_log_test_key_id.as_deref(),
        Some("capture-test-key")
    );
    assert_eq!(stored.values.refresh_margin_seconds, 3600);
    // 快照侧的设置事实只暴露构造方法，这里按序列化形态核对日志门控是否进入快照。
    let snapshot = PgRuntimeSnapshotRepository::new(database.pool.clone())
        .load_runtime_snapshot()
        .await
        .unwrap();
    let settings = serde_json::to_value(&snapshot.settings).unwrap();
    assert_eq!(settings["request_log_enabled"], serde_json::json!(false));
    assert_eq!(
        settings["request_log_test_key_id"],
        serde_json::json!("capture-test-key")
    );
    database.close().await;
}

/// 生产库谱系：上游只到 0019 时 fork 的 9001–9006 已经执行；之后同步进来的上游 0020–0033
/// 与 fork 新增的 9000 / 9007 都要能在这种库上补跑，且日志门控原值保留。
#[tokio::test]
async fn upstream_migration_is_applied_after_existing_fork_migrations() {
    use sqlx::migrate::Migrate as _;

    let Some(database) = TestDatabase::create_through("fork_upgrade", 19).await else {
        return;
    };
    let mut connection = database.pool.acquire().await.unwrap();
    for migration in super::TEST_MIGRATOR
        .iter()
        .filter(|migration| (9001..=9006).contains(&migration.version))
    {
        connection
            .apply("_sqlx_migrations", migration)
            .await
            .unwrap();
    }
    drop(connection);
    sqlx::query("update runtime_settings set request_log_enabled = false, request_log_test_key_id = 'upgrade-test-key' where id = 1")
        .execute(&database.pool)
        .await
        .unwrap();

    super::TEST_MIGRATOR.run(&database.pool).await.unwrap();
    let applied: Vec<i64> =
        sqlx::query_scalar("select version from _sqlx_migrations where success order by version")
            .fetch_all(&database.pool)
            .await
            .unwrap();
    let expected = super::TEST_MIGRATOR
        .iter()
        .map(|migration| migration.version)
        .collect::<Vec<_>>();
    assert_eq!(applied, expected);
    // 9006 的索引列已被上游 0028 删除；9000 在已应用 9006 的库上不得再补列，9007 也不得留下痕迹。
    let shim_column_exists: bool = sqlx::query_scalar(
        "select exists(select 1 from information_schema.columns where table_schema = current_schema() and table_name = 'model_requests' and column_name = 'provider_account_id')",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert!(!shim_column_exists);
    let settings = PgRuntimeSettingsRepository::new(database.pool.clone())
        .load_runtime_settings()
        .await
        .unwrap();
    assert!(!settings.values.request_log_enabled);
    assert_eq!(
        settings.values.request_log_test_key_id.as_deref(),
        Some("upgrade-test-key")
    );
    database.close().await;
}

/// 全新库按版本号顺序执行：0001–0033 之后才轮到 9001–9006，9006 索引的列此时已不存在，
/// 靠 9000 临时补列、9007 收回；最终 schema 不能残留该列。
#[tokio::test]
async fn fresh_database_runs_frozen_fork_index_migration_through_the_shim_column() {
    let Some(database) = TestDatabase::create("fork_fresh_lineage").await else {
        return;
    };
    let shim_column_exists: bool = sqlx::query_scalar(
        "select exists(select 1 from information_schema.columns where table_schema = current_schema() and table_name = 'model_requests' and column_name = 'provider_account_id')",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert!(!shim_column_exists);
    let shim_index_exists: bool = sqlx::query_scalar(
        "select exists(select 1 from pg_indexes where schemaname = current_schema() and indexname = 'model_requests_account_started_idx')",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert!(!shim_index_exists);
    database.close().await;
}
