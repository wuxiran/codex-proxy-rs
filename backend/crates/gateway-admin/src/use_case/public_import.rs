//! 免登录账号导入：密链令牌校验、逐账号随机出口、固定分组与 state 绑定。
//!
//! 配置落在 runtime 数据目录而非数据库：蓝绿两个槽位共享同一目录，每次读取文件即可保持一致，
//! 也不需要为一个运维入口引入迁移。

use std::{
    collections::BTreeSet,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_core::{
    account::{OpaqueProviderData, ProviderAccountId},
    routing::AccountGroupId,
};
use rand_core::{OsRng, RngCore as _};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use subtle::ConstantTimeEq as _;

use super::{account_groups::AccountGroupService, openai::OpenAiService, proxies::ProxiesService};
use crate::model::{
    AdminError, MutationActor, MutationContext, PageSize,
    account_groups::AccountGroupListQuery,
    accounts::{AccountImportSettings, AccountWeight},
    provider_credentials::{
        CredentialMutation, ImportCredentials, ProviderDocument, RotateCredential,
    },
    proxies::{ProxyListQuery, ProxyRecord},
    public_import::{
        MAX_PUBLIC_IMPORT_ACCOUNTS, PublicImportConfig, PublicImportEntry, PublicImportItem,
        PublicImportItemStatus, PublicImportResult, UpdatePublicImportConfig,
    },
};

const CONFIG_FILE: &str = "config.json";
const TOKEN_PREFIX: &str = "imp-";
/// 账号条目里会覆盖默认出口的字段；公开入口一律剥离，出口只由本机代理池决定。
const ACCOUNT_PROXY_FIELDS: [&str; 3] = ["outboundProxyUrl", "outbound_proxy_url", "proxy_key"];

#[async_trait]
pub trait PublicImportService: Send + Sync {
    /// 读取配置；首次读取时生成令牌并落盘。
    async fn config(&self) -> Result<PublicImportConfig, AdminError>;
    async fn update(
        &self,
        context: &MutationContext,
        command: UpdatePublicImportConfig,
    ) -> Result<PublicImportConfig, AdminError>;
    async fn rotate_token(
        &self,
        context: &MutationContext,
    ) -> Result<PublicImportConfig, AdminError>;
    /// 令牌无效或入口关闭时返回 `None`，调用方不得区分这两种情况。
    async fn entry(&self, token: &str) -> Result<Option<PublicImportEntry>, AdminError>;
    /// 令牌无效或入口关闭时返回 `None`。
    async fn import(
        &self,
        token: &str,
        request_id: &str,
        document: Map<String, Value>,
    ) -> Result<Option<PublicImportResult>, AdminError>;
}

pub struct DefaultPublicImportService {
    root: PathBuf,
    openai: Arc<dyn OpenAiService>,
    proxies: Arc<dyn ProxiesService>,
    groups: Arc<dyn AccountGroupService>,
    // 串行化读改写，避免并发保存互相覆盖；跨槽位靠原子 rename 保证文件完整。
    write_lock: tokio::sync::Mutex<()>,
}

impl DefaultPublicImportService {
    pub(crate) fn new(
        root: PathBuf,
        openai: Arc<dyn OpenAiService>,
        proxies: Arc<dyn ProxiesService>,
        groups: Arc<dyn AccountGroupService>,
    ) -> Self {
        Self {
            root,
            openai,
            proxies,
            groups,
            write_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn load(&self) -> Result<Option<PublicImportConfig>, AdminError> {
        let bytes = match fs::read(self.root.join(CONFIG_FILE)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(AdminError::unavailable("导入入口配置暂不可读")),
        };
        let stored: StoredConfig = serde_json::from_slice(&bytes)
            .map_err(|_| AdminError::internal("导入入口配置已损坏"))?;
        stored.into_config().map(Some)
    }

    fn save(&self, config: &PublicImportConfig) -> Result<(), AdminError> {
        let unavailable = |_| AdminError::unavailable("导入入口配置暂不可写");
        fs::create_dir_all(&self.root).map_err(unavailable)?;
        let bytes = serde_json::to_vec_pretty(&StoredConfig::from(config))
            .map_err(|_| AdminError::internal("导入入口配置无法序列化"))?;
        atomic_write(&self.root.join(CONFIG_FILE), &bytes).map_err(unavailable)
    }

    async fn load_or_create(&self) -> Result<PublicImportConfig, AdminError> {
        if let Some(config) = self.load()? {
            return Ok(config);
        }
        let _guard = self.write_lock.lock().await;
        if let Some(config) = self.load()? {
            return Ok(config);
        }
        let config = PublicImportConfig {
            enabled: false,
            token: generate_token(),
            group_ids: Vec::new(),
            pin_turn_state: true,
            expires_at: None,
            updated_at: Utc::now(),
        };
        self.save(&config)?;
        Ok(config)
    }

    /// 校验令牌；只读取已存在的配置，未初始化的入口视为关闭。
    fn authorize(&self, token: &str) -> Result<Option<PublicImportConfig>, AdminError> {
        let Some(config) = self.load()? else {
            return Ok(None);
        };
        // 令牌长度固定且公开，长度不等时提前返回不泄露内容。
        let matches: bool = token.as_bytes().ct_eq(config.token.as_bytes()).into();
        let expired = config.expires_at.is_some_and(|at| at <= Utc::now());
        Ok(
            (matches && config.enabled && !expired && !config.group_ids.is_empty())
                .then_some(config),
        )
    }

    async fn group_names(
        &self,
        ids: &[AccountGroupId],
    ) -> Result<Vec<(AccountGroupId, String)>, AdminError> {
        let wanted = ids.iter().collect::<BTreeSet<_>>();
        let page_size =
            PageSize::new(PageSize::MAX).map_err(|_| AdminError::internal("分页参数不合法"))?;
        let mut found = Vec::new();
        let mut page = 1;
        loop {
            let result = self
                .groups
                .list(AccountGroupListQuery {
                    page,
                    page_size,
                    search: None,
                    enabled: None,
                })
                .await?;
            let fetched = result.items.len();
            found.extend(
                result
                    .items
                    .into_iter()
                    .filter(|group| wanted.contains(&group.id))
                    .map(|group| (group.id, group.name)),
            );
            if fetched == 0 || u64::from(page) * u64::from(page_size.get()) >= result.total {
                return Ok(found);
            }
            page += 1;
        }
    }

    /// 已通过连通性测试的代理才进入随机池，与管理端手工选择代理的约束一致。
    async fn eligible_proxies(&self) -> Result<Vec<ProxyRecord>, AdminError> {
        let page_size =
            PageSize::new(PageSize::MAX).map_err(|_| AdminError::internal("分页参数不合法"))?;
        let mut eligible = Vec::new();
        let mut page = 1;
        loop {
            let result = self
                .proxies
                .list(ProxyListQuery {
                    page,
                    page_size,
                    search: String::new(),
                })
                .await?;
            let fetched = result.items.len();
            eligible.extend(
                result
                    .items
                    .into_iter()
                    .filter(|proxy| proxy.last_test.as_ref().is_some_and(|test| test.success)),
            );
            if fetched == 0 || u64::from(page) * u64::from(page_size.get()) >= result.total {
                return Ok(eligible);
            }
            page += 1;
        }
    }

    async fn pin_turn_state(
        &self,
        context: &MutationContext,
        account_id: ProviderAccountId,
    ) -> bool {
        let mut material = Map::new();
        material.insert("pin_turn_state".to_owned(), Value::Bool(true));
        let result = self
            .openai
            .rotate(RotateCredential {
                mutation: CredentialMutation {
                    context: context.clone(),
                    account_id: account_id.clone(),
                },
                provider_material: ProviderDocument::new(OpaqueProviderData::new(material)),
                settings: None,
            })
            .await;
        if let Err(error) = &result {
            // API Key 账号不支持 state 绑定，属预期；账号本身已入库，不回滚导入。
            tracing::info!(target: "public_import", account_id = %account_id, reason = %error.message(),
                "imported account left without turn state pin");
        }
        result.is_ok()
    }
}

#[async_trait]
impl PublicImportService for DefaultPublicImportService {
    async fn config(&self) -> Result<PublicImportConfig, AdminError> {
        self.load_or_create().await
    }

    async fn update(
        &self,
        context: &MutationContext,
        command: UpdatePublicImportConfig,
    ) -> Result<PublicImportConfig, AdminError> {
        let unique = command.group_ids.iter().collect::<BTreeSet<_>>();
        if unique.len() != command.group_ids.len() {
            return Err(AdminError::invalid("目标分组重复"));
        }
        if command.enabled && command.group_ids.is_empty() {
            return Err(AdminError::invalid("开启导入入口前请先选择目标分组"));
        }
        // 已过期的时间配合开启状态只会得到一个打不开的链接，直接拒绝更容易发现。
        if command.enabled && command.expires_at.is_some_and(|at| at <= Utc::now()) {
            return Err(AdminError::invalid("有效期必须晚于当前时间"));
        }
        if self.group_names(&command.group_ids).await?.len() != command.group_ids.len() {
            return Err(AdminError::invalid("目标分组不存在，请刷新后重试"));
        }
        let current = self.load_or_create().await?;
        let _guard = self.write_lock.lock().await;
        let config = PublicImportConfig {
            enabled: command.enabled,
            token: current.token,
            group_ids: command.group_ids,
            pin_turn_state: command.pin_turn_state,
            expires_at: command.expires_at,
            updated_at: Utc::now(),
        };
        self.save(&config)?;
        tracing::info!(target: "public_import", request_id = %context.request_id, enabled = config.enabled,
            groups = config.group_ids.len(), pin_turn_state = config.pin_turn_state,
            "public import entry updated");
        Ok(config)
    }

    async fn rotate_token(
        &self,
        context: &MutationContext,
    ) -> Result<PublicImportConfig, AdminError> {
        let current = self.load_or_create().await?;
        let _guard = self.write_lock.lock().await;
        let config = PublicImportConfig {
            token: generate_token(),
            updated_at: Utc::now(),
            ..current
        };
        self.save(&config)?;
        tracing::info!(target: "public_import", request_id = %context.request_id,
            "public import token rotated");
        Ok(config)
    }

    async fn entry(&self, token: &str) -> Result<Option<PublicImportEntry>, AdminError> {
        let Some(config) = self.authorize(token)? else {
            return Ok(None);
        };
        let mut names = self.group_names(&config.group_ids).await?;
        names.sort_by_key(|(id, _)| config.group_ids.iter().position(|item| item == id));
        Ok(Some(PublicImportEntry {
            group_names: names.into_iter().map(|(_, name)| name).collect(),
            pin_turn_state: config.pin_turn_state,
            expires_at: config.expires_at,
            max_accounts: MAX_PUBLIC_IMPORT_ACCOUNTS,
        }))
    }

    async fn import(
        &self,
        token: &str,
        request_id: &str,
        document: Map<String, Value>,
    ) -> Result<Option<PublicImportResult>, AdminError> {
        let Some(config) = self.authorize(token)? else {
            return Ok(None);
        };
        let accounts = split_accounts(document)?;
        let proxies = self.eligible_proxies().await?;
        if proxies.is_empty() {
            return Err(AdminError::conflict("暂无通过测试的出站代理，请联系管理员"));
        }
        let context = MutationContext {
            actor: MutationActor::System,
            request_id: request_id.to_owned(),
        };
        let mut items = Vec::with_capacity(accounts.len());
        for (position, account) in accounts.into_iter().enumerate() {
            let index = position + 1;
            let name = account_label(&account);
            let proxy = &proxies[random_index(proxies.len())];
            let mut document = Map::new();
            document.insert(
                "accounts".to_owned(),
                Value::Array(vec![Value::Object(account)]),
            );
            let imported = self
                .openai
                .import_document(ImportCredentials {
                    outbound_proxy_id: Some(proxy.id.clone()),
                    settings: Some(AccountImportSettings {
                        notes: None,
                        enabled: true,
                        concurrency_limit: None,
                        weight: AccountWeight::DEFAULT,
                        model_access: None,
                        group_ids: config.group_ids.clone(),
                    }),
                    context: context.clone(),
                    document: ProviderDocument::new(OpaqueProviderData::new(document)),
                })
                .await;
            let item = match imported {
                Ok(result) => {
                    let mut state_pinned =
                        config.pin_turn_state && !result.credential_ids.is_empty();
                    if config.pin_turn_state {
                        for account_id in &result.credential_ids {
                            state_pinned &= self.pin_turn_state(&context, account_id.clone()).await;
                        }
                    }
                    tracing::info!(target: "public_import", request_id, index, proxy_id = %proxy.id,
                        accounts = result.credential_ids.len(), state_pinned, "public import item committed");
                    PublicImportItem {
                        index,
                        name,
                        status: PublicImportItemStatus::Imported,
                        imported_accounts: result.credential_ids.len(),
                        proxy_name: Some(proxy.name.clone()),
                        state_pinned,
                        message: None,
                    }
                }
                Err(error) => {
                    tracing::warn!(target: "public_import", request_id, index, proxy_id = %proxy.id,
                        reason = %error.message(), "public import item failed");
                    PublicImportItem {
                        index,
                        name,
                        status: PublicImportItemStatus::Failed,
                        imported_accounts: 0,
                        proxy_name: None,
                        state_pinned: false,
                        message: Some(error.message().to_owned()),
                    }
                }
            };
            items.push(item);
        }
        Ok(Some(PublicImportResult { items }))
    }
}

/// 把 sub2api 导出（含 `{ data: … }` 响应信封）或单账号文档拆成独立账号条目，
/// 使每个账号单独抽取出口、单独成败；文档自带的代理一律丢弃。
fn split_accounts(mut document: Map<String, Value>) -> Result<Vec<Map<String, Value>>, AdminError> {
    if let Some(Value::Object(inner)) = document.get("data")
        && inner.contains_key("accounts")
    {
        let Some(Value::Object(inner)) = document.remove("data") else {
            return Err(AdminError::invalid("账号文件格式不正确"));
        };
        document = inner;
    }
    // 观澜 CDK 兑换会消耗第三方额度，公开入口不代为兑换。
    if document.contains_key("cdks") {
        return Err(AdminError::invalid("此入口不支持 CDK 兑换"));
    }
    let values = match document.remove("accounts") {
        Some(Value::Array(values)) => values,
        Some(_) => return Err(AdminError::invalid("accounts 必须是数组")),
        None => vec![Value::Object(document)],
    };
    if values.is_empty() {
        return Err(AdminError::invalid("文件中没有账号"));
    }
    if values.len() > MAX_PUBLIC_IMPORT_ACCOUNTS {
        return Err(AdminError::invalid(format!(
            "单次最多导入 {MAX_PUBLIC_IMPORT_ACCOUNTS} 个账号"
        )));
    }
    values
        .into_iter()
        .map(|value| match value {
            Value::Object(mut account) => {
                for field in ACCOUNT_PROXY_FIELDS {
                    account.remove(field);
                }
                Ok(account)
            }
            _ => Err(AdminError::invalid("账号条目必须是 JSON 对象")),
        })
        .collect()
}

fn account_label(account: &Map<String, Value>) -> Option<String> {
    let credentials = account.get("credentials").and_then(Value::as_object);
    ["name", "label", "email"]
        .iter()
        .find_map(|field| account.get(*field).and_then(Value::as_str))
        .or_else(|| {
            credentials
                .and_then(|value| value.get("email"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(120).collect())
}

fn generate_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("{TOKEN_PREFIX}{}", hex::encode(bytes))
}

/// 拒绝采样消除取模偏差；代理池很小，循环几乎总是一次结束。
fn random_index(len: usize) -> usize {
    let len = len as u64;
    let zone = u64::MAX - u64::MAX % len;
    loop {
        let value = OsRng.next_u64();
        if value < zone {
            return (value % len) as usize;
        }
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::now_v7()));
    let result = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredConfig {
    enabled: bool,
    token: String,
    group_ids: Vec<String>,
    pin_turn_state: bool,
    // chrono 在本 crate 未启用 serde，落盘用 RFC 3339 文本。
    #[serde(default)]
    expires_at: Option<String>,
    updated_at: String,
}

impl StoredConfig {
    fn into_config(self) -> Result<PublicImportConfig, AdminError> {
        if self.token.is_empty() {
            return Err(AdminError::internal("导入入口配置已损坏"));
        }
        Ok(PublicImportConfig {
            enabled: self.enabled,
            token: self.token,
            group_ids: self
                .group_ids
                .into_iter()
                .map(AccountGroupId::new)
                .collect::<Result<_, _>>()
                .map_err(|_| AdminError::internal("导入入口配置已损坏"))?,
            pin_turn_state: self.pin_turn_state,
            expires_at: self
                .expires_at
                .map(|value| DateTime::parse_from_rfc3339(&value).map(|at| at.with_timezone(&Utc)))
                .transpose()
                .map_err(|_| AdminError::internal("导入入口配置已损坏"))?,
            updated_at: DateTime::parse_from_rfc3339(&self.updated_at)
                .map_err(|_| AdminError::internal("导入入口配置已损坏"))?
                .with_timezone(&Utc),
        })
    }
}

impl From<&PublicImportConfig> for StoredConfig {
    fn from(config: &PublicImportConfig) -> Self {
        Self {
            enabled: config.enabled,
            token: config.token.clone(),
            group_ids: config.group_ids.iter().map(ToString::to_string).collect(),
            pin_turn_state: config.pin_turn_state,
            expires_at: config.expires_at.map(|at| at.to_rfc3339()),
            updated_at: config.updated_at.to_rfc3339(),
        }
    }
}
