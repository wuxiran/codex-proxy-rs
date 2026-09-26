//! 免登录账号导入入口的配置与逐账号结果。

use std::fmt;

use chrono::{DateTime, Utc};
use gateway_core::routing::AccountGroupId;

/// 单次提交最多拆出的账号数，与 Provider 的 `accounts` 上限一致。
pub const MAX_PUBLIC_IMPORT_ACCOUNTS: usize = 200;

/// 入口配置。每个配置对应一个上游号商（供应商），`token` 是页面地址里的密链令牌，
/// 持有者无需登录即可导入；`name` 是号商名，导入的账号会带上它便于追溯来源。
#[derive(Clone, PartialEq, Eq)]
pub struct PublicImportConfig {
    /// 稳定标识，用于管理端定位某个号商配置（不外泄给密链持有者）。
    pub id: String,
    /// 号商名称，例如「迷茫」。
    pub name: String,
    pub enabled: bool,
    pub token: String,
    pub group_ids: Vec<AccountGroupId>,
    pub pin_turn_state: bool,
    /// 链接失效时间；`None` 表示长期有效。到期后对外表现与入口关闭一致。
    pub expires_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

impl fmt::Debug for PublicImportConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublicImportConfig")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("enabled", &self.enabled)
            .field("token", &"[REDACTED]")
            .field("group_ids", &self.group_ids)
            .field("pin_turn_state", &self.pin_turn_state)
            .field("expires_at", &self.expires_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

/// 新建或修改一个号商配置的字段；令牌只能通过轮换更新。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePublicImportConfig {
    pub name: String,
    pub enabled: bool,
    pub group_ids: Vec<AccountGroupId>,
    pub pin_turn_state: bool,
    pub expires_at: Option<DateTime<Utc>>,
}

/// 密链页面展示的最小事实，不含分组 ID 和代理信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicImportEntry {
    pub group_names: Vec<String>,
    pub pin_turn_state: bool,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_accounts: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicImportItemStatus {
    Imported,
    Failed,
}

/// 单个账号条目的导入结果；`message` 已脱敏，可直接展示给提交者。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicImportItem {
    pub index: usize,
    pub name: Option<String>,
    pub status: PublicImportItemStatus,
    pub imported_accounts: usize,
    pub proxy_name: Option<String>,
    pub state_pinned: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicImportResult {
    pub items: Vec<PublicImportItem>,
}

/// 免登录入口的票据导入：每行 `邮箱----密码----2FA密钥`，买入价与预计到期时间必填。
pub struct PublicTicketImport {
    pub tickets: Vec<secrecy::SecretString>,
    pub purchase_amount: String,
    pub purchase_currency: String,
    pub expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for PublicTicketImport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PublicTicketImport")
            .field("tickets", &self.tickets.len())
            .field("purchase_amount", &self.purchase_amount)
            .field("purchase_currency", &self.purchase_currency)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}
