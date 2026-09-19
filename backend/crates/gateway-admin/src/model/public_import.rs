//! 免登录账号导入入口的配置与逐账号结果。

use std::fmt;

use chrono::{DateTime, Utc};
use gateway_core::routing::AccountGroupId;

/// 单次提交最多拆出的账号数，与 Provider 的 `accounts` 上限一致。
pub const MAX_PUBLIC_IMPORT_ACCOUNTS: usize = 200;

/// 入口配置。`token` 是页面地址里的密链令牌，持有者无需登录即可导入。
#[derive(Clone, PartialEq, Eq)]
pub struct PublicImportConfig {
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
            .field("enabled", &self.enabled)
            .field("token", &"[REDACTED]")
            .field("group_ids", &self.group_ids)
            .field("pin_turn_state", &self.pin_turn_state)
            .field("expires_at", &self.expires_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

/// 管理员可修改的字段；令牌只能通过轮换更新。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePublicImportConfig {
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
