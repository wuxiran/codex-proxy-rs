//! 账号成本、到期时间与登录票据（fork 子表 `account_tickets`）。
//!
//! 票据是 `邮箱----密码----2FA 密钥` 一行；只在服务端加密后落库，接口只回显打码的邮箱。

use std::fmt;

use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret as _, SecretString};

use super::{AdminError, MutationContext};

/// 成本币种。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketCurrency {
    Cny,
    Usd,
}

impl TicketCurrency {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cny => "CNY",
            Self::Usd => "USD",
        }
    }

    /// # Errors
    ///
    /// 不是 `CNY` / `USD` 时返回错误。
    pub fn parse(value: &str) -> Result<Self, AdminError> {
        match value {
            "CNY" => Ok(Self::Cny),
            "USD" => Ok(Self::Usd),
            _ => Err(AdminError::invalid("币种只支持 CNY 或 USD")),
        }
    }
}

/// 一个账号的成本、到期与票据状态；不含任何票据明文。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountTicketFacts {
    /// 买入价，保留两位小数的十进制文本。
    pub purchase_amount: Option<String>,
    pub purchase_currency: Option<TicketCurrency>,
    pub purchased_at: Option<DateTime<Utc>>,
    /// 预计失效时间（例如车主踢人的时间）。
    pub expires_at: Option<DateTime<Utc>>,
    /// 打码后的票据邮箱；为空表示没有票据。
    pub ticket_hint: Option<String>,
    pub ticket_updated_at: Option<DateTime<Utc>>,
    /// 自买入（未填买入时间则自入库）起按模型价格计费的累计美元金额。
    pub spent_usd: Option<String>,
    /// 本轮失效后已自动复活的次数（上限见自动复活任务）；成功或恢复正常后清零。
    pub auto_revive_attempts: i32,
    pub auto_revive_last_at: Option<DateTime<Utc>>,
    pub auto_revive_last_error: Option<String>,
}

/// 票据的写入意图。
pub enum TicketChange {
    Keep,
    Set { ciphertext: Vec<u8>, hint: String },
    Clear,
}

impl fmt::Debug for TicketChange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Keep => formatter.write_str("Keep"),
            Self::Set { hint, .. } => formatter.debug_struct("Set").field("hint", hint).finish(),
            Self::Clear => formatter.write_str("Clear"),
        }
    }
}

/// Store 写入命令：成本与到期整体替换，票据按意图处理。
#[derive(Debug)]
pub struct AccountTicketWrite {
    pub account_id: String,
    pub purchase: Option<(String, TicketCurrency)>,
    pub purchased_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub ticket: TicketChange,
}

/// 管理端保存命令。
pub struct UpdateAccountTicket {
    pub context: MutationContext,
    pub account_id: gateway_core::account::ProviderAccountId,
    pub purchase_amount: Option<String>,
    pub purchase_currency: Option<String>,
    pub purchased_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    /// `Some` 时替换票据（`邮箱----密码----2FA 密钥`）；`None` 保持不变。
    pub ticket_line: Option<SecretString>,
    pub clear_ticket: bool,
}

impl fmt::Debug for UpdateAccountTicket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateAccountTicket")
            .field("account_id", &self.account_id)
            .field("purchase_amount", &self.purchase_amount)
            .field("purchase_currency", &self.purchase_currency)
            .field(
                "ticket_line",
                &self.ticket_line.as_ref().map(|_| "<redacted>"),
            )
            .field("clear_ticket", &self.clear_ticket)
            .finish_non_exhaustive()
    }
}

/// 解析后的票据明文。
pub struct TicketSecret {
    pub email: String,
    pub password: SecretString,
    pub totp_secret: SecretString,
}

impl fmt::Debug for TicketSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TicketSecret")
            .field("email", &mask_email(&self.email))
            .finish_non_exhaustive()
    }
}

impl TicketSecret {
    /// 解析 `邮箱----密码----2FA 密钥`；2FA 段可省略，多余段忽略。
    ///
    /// # Errors
    ///
    /// 缺少邮箱或密码、邮箱不含 `@`、或 2FA 密钥不是 Base32 时返回错误。
    pub fn parse_line(line: &str) -> Result<Self, AdminError> {
        let mut parts = line.trim().split("----").map(str::trim);
        let email = parts.next().unwrap_or_default();
        let password = parts.next().unwrap_or_default();
        let totp_secret = parts
            .next()
            .unwrap_or_default()
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>()
            .to_ascii_uppercase();
        if email.is_empty() || !email.contains('@') || email.len() > 254 {
            return Err(AdminError::invalid("票据格式应为 邮箱----密码----2FA密钥"));
        }
        if password.is_empty() || password.len() > 256 {
            return Err(AdminError::invalid("票据缺少密码"));
        }
        if !totp_secret
            .trim_end_matches('=')
            .chars()
            .all(|character| matches!(character, 'A'..='Z' | '2'..='7'))
            || totp_secret.len() > 128
        {
            return Err(AdminError::invalid("2FA 密钥应为 Base32（A-Z、2-7）"));
        }
        Ok(Self {
            email: email.to_owned(),
            password: SecretString::from(password.to_owned()),
            totp_secret: SecretString::from(totp_secret),
        })
    }

    /// 加密前的规范化序列化；只在服务端内存里出现。
    #[must_use]
    pub fn to_plaintext(&self) -> Vec<u8> {
        serde_json::json!({
            "email": self.email,
            "password": self.password.expose_secret(),
            "totp_secret": self.totp_secret.expose_secret(),
        })
        .to_string()
        .into_bytes()
    }

    /// # Errors
    ///
    /// 解密后的内容不是本模块写入的格式时返回错误。
    pub fn from_plaintext(bytes: &[u8]) -> Result<Self, AdminError> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| AdminError::internal("票据内容已损坏"))?;
        let field = |name: &str| {
            value
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| AdminError::internal("票据内容已损坏"))
        };
        Ok(Self {
            email: field("email")?,
            password: SecretString::from(field("password")?),
            totp_secret: SecretString::from(field("totp_secret")?),
        })
    }
}

/// 打码邮箱：保留首尾各一个字符与域名。
#[must_use]
pub fn mask_email(email: &str) -> String {
    let Some((local, domain)) = email.split_once('@') else {
        return "***".to_owned();
    };
    let mut chars = local.chars();
    match (chars.next(), chars.next_back()) {
        (Some(first), Some(last)) => format!("{first}***{last}@{domain}"),
        (Some(first), None) => format!("{first}***@{domain}"),
        _ => format!("***@{domain}"),
    }
}

/// 买入价：非负、最多两位小数、整数部分不超过 10 位。
///
/// # Errors
///
/// 格式不合法时返回错误。
pub fn normalize_amount(value: &str) -> Result<String, AdminError> {
    let value = value.trim();
    let (integer, fraction) = value.split_once('.').unwrap_or((value, ""));
    let valid = !integer.is_empty()
        && integer.len() <= 10
        && integer.chars().all(|character| character.is_ascii_digit())
        && fraction.len() <= 2
        && fraction.chars().all(|character| character.is_ascii_digit());
    if !valid {
        return Err(AdminError::invalid("买入价应为非负数，最多两位小数"));
    }
    Ok(format!("{integer}.{fraction:0<2}"))
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret as _;

    use super::{TicketSecret, mask_email, normalize_amount};

    #[test]
    fn parses_ticket_line_and_normalizes_totp_secret() {
        let ticket =
            TicketSecret::parse_line(" a.b@example.com----p@ss----abcd efgh 2345 ").unwrap();
        assert_eq!(ticket.email, "a.b@example.com");
        assert_eq!(ticket.password.expose_secret(), "p@ss");
        assert_eq!(ticket.totp_secret.expose_secret(), "ABCDEFGH2345");
        let restored = TicketSecret::from_plaintext(&ticket.to_plaintext()).unwrap();
        assert_eq!(restored.email, ticket.email);
        assert_eq!(restored.totp_secret.expose_secret(), "ABCDEFGH2345");
    }

    #[test]
    fn rejects_malformed_ticket_lines() {
        assert!(TicketSecret::parse_line("no-at----pw----ABC").is_err());
        assert!(TicketSecret::parse_line("a@b.c----").is_err());
        assert!(TicketSecret::parse_line("a@b.c----pw----not-base32!").is_err());
        assert!(TicketSecret::parse_line("a@b.c----pw").is_ok());
    }

    #[test]
    fn masks_email_and_normalizes_amount() {
        assert_eq!(mask_email("dennyfuo@gmail.com"), "d***o@gmail.com");
        assert_eq!(mask_email("a@x.io"), "a***@x.io");
        assert_eq!(normalize_amount("55").unwrap(), "55.00");
        assert_eq!(normalize_amount("55.5").unwrap(), "55.50");
        assert!(normalize_amount("-1").is_err());
        assert!(normalize_amount("1.234").is_err());
    }
}
