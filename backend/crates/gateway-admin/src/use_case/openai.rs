//! fork：票据复活、免登录导入与票据恢复消费的 OpenAI 固定凭据入口。
//!
//! 上游已把 OpenAI/xAI 固定服务并入通用 [`super::credentials::CredentialsService`]；这里只保留 fork
//! 用到的窄 trait，由 `CredentialsService` 每次操作冻结 openai 实现后委托（见 `credentials::flow`）。

use async_trait::async_trait;

use crate::model::{
    AdminError,
    account_tickets::TicketSecret,
    provider_credentials::{
        CredentialImportResult, CredentialMutationResult, ImportCredentials, ProviderDocument,
        RotateCredential,
    },
};

#[async_trait]
pub trait OpenAiService: Send + Sync {
    /// 用票据登录换回单账号导入文档（经 Provider 的登录服务，走给定出口）。
    async fn ticket_login(
        &self,
        ticket: &TicketSecret,
        proxy: Option<&gateway_core::account::OutboundProxy>,
    ) -> Result<ProviderDocument, AdminError>;
    /// 只新建 OAuth 账号：API Key 账号与上游身份已存在的账号整批拒绝，供免登录入口使用。
    async fn import_new_accounts(
        &self,
        command: ImportCredentials,
    ) -> Result<CredentialImportResult, AdminError>;
    async fn rotate(
        &self,
        command: RotateCredential,
    ) -> Result<CredentialMutationResult, AdminError>;
}
