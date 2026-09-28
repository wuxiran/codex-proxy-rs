//! fork：固定自身 state 等凭据配置轮换保留凭据健康与额度。

use super::*;

#[tokio::test]
async fn turn_state_configuration_rotation_preserves_credential_error_and_quota() {
    let Some(database) = TestDatabase::create("turn_state_configuration_status").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let id = "acct_pin_config";
    repository
        .import_provider_accounts(ImportProviderAccounts {
            settings: None,
            outbound_proxy: None,
            scope: ProviderAccountAdminScope {
                provider_kind: "openai".to_owned(),
            },
            accounts: vec![account(id, "pin-config-user")],
            audit: audit("pin_config_import", "import", id),
        })
        .await
        .unwrap();
    sqlx::query("update provider_accounts set credential_state='banned', last_error_reason='account_banned', last_error_message='retained failure', updated_at=greatest(now(),updated_at), quota_access_state='exhausted', quota_evidence='provider_denied', quota_access_observed_at=now() where id=$1")
        .bind(id).execute(&database.pool).await.unwrap();
    let before = repository.load_provider_account(id).await.unwrap().unwrap();
    let mut update = credential_update(id, 1, "same-secret-with-config");
    update.preserve_profile = true;
    update.preserve_credential_state = true;
    repository
        .rotate_provider_account(RotateProviderAccount {
            settings: None,
            scope: ProviderAccountAdminScope {
                provider_kind: "openai".to_owned(),
            },
            profile: profile(id, "must-not-replace"),
            replacement_identity: None,
            credential: update,
            audit: audit("pin_config_rotate", "rotate_credential", id),
        })
        .await
        .unwrap();
    let after = repository.load_provider_account(id).await.unwrap().unwrap();
    assert_eq!(after.summary.credential_revision.get(), 2);
    assert_eq!(
        after.summary.credential_state,
        before.summary.credential_state
    );
    assert_eq!(
        after.summary.credential_observed_at,
        before.summary.credential_observed_at
    );
    assert_eq!(
        after.summary.last_error_reason,
        before.summary.last_error_reason
    );
    assert_eq!(
        after.summary.last_error_message,
        before.summary.last_error_message
    );
    assert_eq!(after.summary.quota, before.summary.quota);
    assert_eq!(after.summary.name, before.summary.name);
    database.close().await;
}
