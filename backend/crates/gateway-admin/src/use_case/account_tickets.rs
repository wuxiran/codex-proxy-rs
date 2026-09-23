//! 账号成本、到期时间与登录票据（fork 子表 `account_tickets`）。
//!
//! 票据明文只在本进程内存里短暂存在：保存时立即加密，恢复时解密后交给 Provider 登录，
//! 不写日志、不回传前端。

use gateway_core::account::{OpaqueProviderData, ProviderAccountId};
use secrecy::ExposeSecret as _;
use serde_json::{Map, Value};

use super::{accounts::DefaultAccountsService, map_store_error};
use crate::model::{
    AdminError,
    account_tickets::{
        AccountTicketFacts, AccountTicketWrite, TicketChange, TicketCurrency, TicketSecret,
        UpdateAccountTicket, mask_email, normalize_amount,
    },
    provider_credentials::ProviderDocument,
};

const OPENAI_OAUTH: &str = "oauth";

impl DefaultAccountsService {
    pub(super) async fn load_ticket_facts(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<AccountTicketFacts, AdminError> {
        self.provider_for_account(account_id).await?;
        Ok(self
            .accounts
            .load_account_tickets(std::slice::from_ref(&account_id.as_str().to_owned()))
            .await
            .map_err(|error| map_store_error(error, "account ticket"))?
            .remove(account_id.as_str())
            .unwrap_or_default())
    }

    pub(super) async fn save_ticket(
        &self,
        command: UpdateAccountTicket,
    ) -> Result<AccountTicketFacts, AdminError> {
        if command.ticket_line.is_some() && !command.clear_ticket {
            self.require_ticket_account(&command.account_id).await?;
        } else {
            self.provider_for_account(&command.account_id).await?;
        }
        let purchase = match (
            command.purchase_amount.as_deref(),
            command.purchase_currency.as_deref(),
        ) {
            (None | Some(""), _) => None,
            (Some(amount), currency) => Some((
                normalize_amount(amount)?,
                TicketCurrency::parse(currency.unwrap_or("CNY"))?,
            )),
        };
        let account_id = command.account_id.as_str().to_owned();
        let ticket = if command.clear_ticket {
            TicketChange::Clear
        } else if let Some(line) = command.ticket_line.as_ref() {
            let secret = TicketSecret::parse_line(line.expose_secret())?;
            TicketChange::Set {
                ciphertext: self
                    .ticket_cipher
                    .seal(&account_id, &secret.to_plaintext())?,
                hint: mask_email(&secret.email),
            }
        } else {
            TicketChange::Keep
        };
        let ticket_action = match &ticket {
            TicketChange::Keep => "keep",
            TicketChange::Set { .. } => "set",
            TicketChange::Clear => "clear",
        };
        self.accounts
            .save_account_ticket(AccountTicketWrite {
                account_id: account_id.clone(),
                purchase,
                purchased_at: command.purchased_at,
                expires_at: command.expires_at,
                ticket,
            })
            .await
            .map_err(|error| map_store_error(error, "account ticket"))?;
        tracing::info!(
            target: "account_ticket",
            request_id = %command.context.request_id,
            account_id = %account_id,
            ticket = ticket_action,
            "account ticket updated"
        );
        self.load_ticket_facts(&command.account_id).await
    }

    /// 解密票据并组装 Provider 轮换材料；登录与写回由 Provider 的轮换流程完成。
    pub(super) async fn ticket_material(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<ProviderDocument, AdminError> {
        self.require_ticket_account(account_id).await?;
        let sealed = self
            .accounts
            .load_account_ticket_ciphertext(account_id.as_str())
            .await
            .map_err(|error| map_store_error(error, "account ticket"))?
            .ok_or_else(|| AdminError::invalid("该账号还没有录入票据"))?;
        let secret =
            TicketSecret::from_plaintext(&self.ticket_cipher.open(account_id.as_str(), &sealed)?)?;
        let mut ticket = Map::new();
        ticket.insert("email".to_owned(), Value::String(secret.email.clone()));
        ticket.insert(
            "password".to_owned(),
            Value::String(secret.password.expose_secret().to_owned()),
        );
        ticket.insert(
            "totp_secret".to_owned(),
            Value::String(secret.totp_secret.expose_secret().to_owned()),
        );
        let mut material = Map::new();
        material.insert("ticket_restore".to_owned(), Value::Object(ticket));
        Ok(ProviderDocument::new(OpaqueProviderData::new(material)))
    }

    /// 票据只用于 OpenAI OAuth 账号：API Key 账号没有可登录的身份；成本与到期不限账号类型。
    async fn require_ticket_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<(), AdminError> {
        let (stored, _) = self.provider_for_account(account_id).await?;
        if stored.account.provider_kind.as_str() != "openai"
            || stored.account.authentication_kind != OPENAI_OAUTH
        {
            return Err(AdminError::invalid("只有 OpenAI OAuth 账号支持登录票据"));
        }
        Ok(())
    }
}
