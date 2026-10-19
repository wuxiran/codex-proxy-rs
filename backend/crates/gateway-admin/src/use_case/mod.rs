//! 管理控制面的用例实现

pub mod account_groups;
mod account_tickets;
pub mod accounts;
pub mod auth;
pub mod backup;
pub mod client_distribution;
pub mod client_keys;
pub mod credentials;
pub mod import_tasks;
pub mod key_usage;
pub mod observability;
pub mod plugin_accounts;
pub mod plugin_client_keys;
pub mod plugin_resources;
pub(crate) mod plugin_update;
pub mod plugins;
pub mod proxies;
pub mod public_import;
pub mod settings;
pub mod system;
pub mod usage_data;

// fork: 自有用例模块
pub mod openai; // fork: 票据登录 / 免登录导入的窄 trait
pub(crate) mod turn_state_hunt;
pub(crate) mod usage_projection_cache;

mod credential_mutation;
mod error;
mod publication;

use credential_mutation::{
    commit_authorization, commit_credential_refresh, commit_credential_rotation,
    delete_credentials, import_proxy_binding, pending_authorization,
    publish_credentials_and_observe_quota, required_credential, required_plugin_credential,
    validate_authorization_commit, validate_prepared_import, validate_prepared_rotation,
    validate_prepared_rotation_facts,
};
use error::{map_provider_error, map_store_error};
use publication::publish_committed;
