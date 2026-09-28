//! fork：遍历代理 / 诊断出口的契约测试（从 contract/mod.rs 移出）。

use super::*;

/// 遍历代理时一个出口上的失败说明的是那个出口。即使上游回的是 401，也不能据此把
/// 一个正在服务线上流量的账号标成凭据失效；不带临时出口的连接测试则照旧回写。
#[tokio::test]
async fn probe_through_a_temporary_egress_never_writes_back_account_state() {
    for (through_temporary_egress, expected) in [
        (true, CredentialState::Ready),
        (false, CredentialState::Expired),
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        let account_id = "acct_egress_probe";
        create_account(&store, account_id).await;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .respond_with(
                ResponseTemplate::new(401)
                    .insert_header("content-type", "application/json")
                    .set_body_string(
                        r#"{"error":{"message":"token expired","code":"token_expired"}}"#,
                    ),
            )
            .mount(&server)
            .await;

        let mut context = diagnostic_context("req_egress_probe", account_id);
        if through_temporary_egress {
            context = context.with_diagnostic_egress(Some(
                gateway_core::engine::DiagnosticEgress::new(None, None),
            ));
        }
        let affinity = Arc::new(MemorySessionAffinity::default());
        let result = provider_with_affinity_and_base_url(&store, affinity, server.uri())
            .execute(
                planned_request("openai", http_generate_operation()),
                context,
            )
            .await;
        if let Ok(mut stream) = result {
            while let Some(event) = stream.next().await {
                if event.is_err() {
                    break;
                }
            }
        }

        let account = store.account(account_id).expect("account after probe");
        assert_eq!(
            account.credential_state(),
            expected,
            "through_temporary_egress={through_temporary_egress}"
        );
    }
}
