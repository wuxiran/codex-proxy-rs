//! fork：票据到点且已不能调度的账号归入「已过期」目录状态（account_tickets，fork 迁移 9003）。

use gateway_admin::model::accounts::AccountListStatus;

use super::*;

#[tokio::test]
async fn fork_expired_status_hides_unschedulable_expired_accounts() {
    let Some(database) = TestDatabase::create("provider_account_expired_status").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let now = Utc::now();

    let mut alpha = account("acct_alpha", "user-alpha");
    alpha.email = Some("alpha@example.invalid".to_owned());
    let mut beta = account("acct_beta", "user-beta");
    beta.provider_kind = "xai".to_owned();
    beta.email = Some("beta@example.invalid".to_owned());
    beta.credential_state = CredentialState::Banned;
    let mut charlie = account("acct_charlie", "user-charlie");
    charlie.email = Some("charlie@example.invalid".to_owned());
    let mut invalid = account("acct_invalid", "user-invalid");
    invalid.email = Some("invalid@example.invalid".to_owned());
    invalid.credential_state = CredentialState::Invalid;
    let mut disabled = account("acct_disabled", "user-disabled");
    disabled.email = Some("disabled@example.invalid".to_owned());
    disabled.enabled = false;
    let mut quota_exhausted = account("acct_quota_exhausted", "user-quota-exhausted");
    quota_exhausted.email = Some("quota-exhausted@example.invalid".to_owned());
    for account in [alpha, beta, charlie, invalid, disabled, quota_exhausted] {
        repository
            .insert_provider_account(account)
            .await
            .expect("insert account list fixture");
    }
    for account_id in ["acct_charlie", "acct_quota_exhausted"] {
        let observed_at = SystemTime::now();
        repository
            .apply_quota_access(QuotaAccessChange {
                account_id: ProviderAccountId::new(account_id).expect("account id"),
                expected_revision: CredentialRevision::new(1).expect("credential revision"),
                state: QuotaState::exhausted(QuotaEvidence::ProviderDenied, observed_at, None),
            })
            .await
            .expect("seed exhausted account");
    }
    let store = admin_account_store(&database.pool);
    // fork：票据到点且已不能调度的账号归入「已过期」，默认目录与 total 都不含它；
    // 票据到点但还能调度（alpha 正常）的照常显示；未到点的票据不影响状态。
    sqlx::query(
        "insert into account_tickets (provider_account_id, expires_at)
         values ('acct_invalid', $1), ('acct_alpha', $1), ('acct_beta', $2)",
    )
    .bind(now - TimeDelta::minutes(1))
    .bind(now + TimeDelta::days(1))
    .execute(&database.pool)
    .await
    .expect("seed account tickets");
    let default_page = store
        .list_accounts(
            AccountListQuery {
                page: 1,
                page_size: PageSize::new(10).expect("page size"),
                provider_kind: None,
                group_filter: None,
                search: None,
                status: None,
                sort: None,
            },
            Default::default(),
        )
        .await
        .expect("hide expired accounts by default");
    assert_eq!(default_page.total, 5);
    assert!(
        default_page
            .items
            .iter()
            .all(|item| item.account.id != "acct_invalid")
    );
    assert!(
        default_page
            .items
            .iter()
            .any(|item| item.account.id == "acct_alpha")
    );
    assert_eq!(default_page.summary.total, 5);
    assert_eq!(default_page.summary.expired, 1);
    assert_eq!(default_page.summary.error, 1);
    assert_eq!(default_page.summary.normal, 1);
    assert_eq!(
        default_page.summary.total,
        default_page.summary.normal
            + default_page.summary.quota_exhausted
            + default_page.summary.rate_limited
            + default_page.summary.disabled
            + default_page.summary.error
    );
    let expired_accounts = store
        .list_accounts(
            AccountListQuery {
                page: 1,
                page_size: PageSize::new(10).expect("page size"),
                provider_kind: None,
                group_filter: None,
                search: None,
                status: Some(AccountListStatus::TicketExpired),
                sort: None,
            },
            Default::default(),
        )
        .await
        .expect("filter expired accounts");
    assert_eq!(expired_accounts.total, 1);
    assert_eq!(expired_accounts.items[0].account.id, "acct_invalid");
    database.close().await;
}
