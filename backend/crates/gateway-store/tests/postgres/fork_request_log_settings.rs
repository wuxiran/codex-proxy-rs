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
    update.request_log_enabled = false;
    update.request_log_test_key_id = Some("capture-test-key".to_owned());
    repository.update_runtime_settings(update).await.unwrap();

    let stored = repository.load_runtime_settings().await.unwrap();
    assert!(!stored.request_log_enabled);
    assert_eq!(
        stored.request_log_test_key_id.as_deref(),
        Some("capture-test-key")
    );
    assert_eq!(stored.refresh_margin_seconds, 3600);
    let snapshot = PgRuntimeSnapshotRepository::new(database.pool.clone())
        .load_runtime_snapshot()
        .await
        .unwrap();
    assert!(!snapshot.settings.request_log_enabled);
    assert_eq!(
        snapshot.settings.request_log_test_key_id.as_deref(),
        Some("capture-test-key")
    );
    database.close().await;
}

#[tokio::test]
async fn upstream_migration_is_applied_after_existing_fork_migrations() {
    use sqlx::migrate::Migrate as _;

    let Some(database) = TestDatabase::create_through("fork_upgrade", 19).await else {
        return;
    };
    // 重建升级前的谱系：上游只到 0019，但 fork 的 9xxx 已经执行。
    let mut connection = database.pool.acquire().await.unwrap();
    for migration in super::TEST_MIGRATOR
        .iter()
        .filter(|migration| migration.version >= 9000)
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
    let applied: bool = sqlx::query_scalar(
        "select exists(select 1 from _sqlx_migrations where version = 20 and success)",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert!(applied);
    let settings = PgRuntimeSettingsRepository::new(database.pool.clone())
        .load_runtime_settings()
        .await
        .unwrap();
    assert!(!settings.request_log_enabled);
    assert_eq!(
        settings.request_log_test_key_id.as_deref(),
        Some("upgrade-test-key")
    );
    database.close().await;
}
