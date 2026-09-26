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

use super::{
    account_groups::AccountGroupService, accounts::AccountsService, openai::OpenAiService,
    proxies::ProxiesService,
};
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
        PublicImportItemStatus, PublicImportResult, PublicTicketImport, UpdatePublicImportConfig,
    },
};

const CONFIG_FILE: &str = "config.json";
const TOKEN_PREFIX: &str = "imp-";
/// 账号条目里会覆盖默认出口的字段；公开入口一律剥离，出口只由本机代理池决定。
const ACCOUNT_PROXY_FIELDS: [&str; 3] = ["outboundProxyUrl", "outbound_proxy_url", "proxy_key"];

#[async_trait]
pub trait PublicImportService: Send + Sync {
    /// 列出全部号商配置（可能为空，不再自动生成默认入口）。
    async fn list(&self) -> Result<Vec<PublicImportConfig>, AdminError>;
    /// 新建一个号商配置并生成新令牌。
    async fn create(
        &self,
        context: &MutationContext,
        command: UpdatePublicImportConfig,
    ) -> Result<PublicImportConfig, AdminError>;
    /// 按 id 修改一个号商配置。
    async fn update(
        &self,
        context: &MutationContext,
        id: &str,
        command: UpdatePublicImportConfig,
    ) -> Result<PublicImportConfig, AdminError>;
    /// 按 id 删除一个号商配置。
    async fn delete(&self, context: &MutationContext, id: &str) -> Result<(), AdminError>;
    /// 按 id 轮换某个号商的令牌。
    async fn rotate_token(
        &self,
        context: &MutationContext,
        id: &str,
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
    /// 票据导入：逐行随机出口登录建号，并加密保存票据、买入价与到期时间。
    /// 令牌无效或入口关闭时返回 `None`。
    async fn import_tickets(
        &self,
        token: &str,
        request_id: &str,
        command: PublicTicketImport,
    ) -> Result<Option<PublicImportResult>, AdminError>;
}

pub struct DefaultPublicImportService {
    root: PathBuf,
    openai: Arc<dyn OpenAiService>,
    proxies: Arc<dyn ProxiesService>,
    groups: Arc<dyn AccountGroupService>,
    accounts: Arc<dyn AccountsService>,
    // 读改写整体在锁内完成，避免并发保存把刚轮换掉的令牌写回；跨槽位靠原子 rename 保证文件完整
    // （管理请求只进入 active 槽位，排空中的旧槽位不接新的管理写入）。
    write_lock: tokio::sync::Mutex<()>,
}

impl DefaultPublicImportService {
    pub(crate) fn new(
        root: PathBuf,
        openai: Arc<dyn OpenAiService>,
        proxies: Arc<dyn ProxiesService>,
        groups: Arc<dyn AccountGroupService>,
        accounts: Arc<dyn AccountsService>,
    ) -> Self {
        Self {
            root,
            openai,
            proxies,
            groups,
            accounts,
            write_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// 读取全部号商配置（文件缺失视为空）；兼容旧单配置格式并原样迁移进列表。
    fn load_all(&self) -> Result<Vec<PublicImportConfig>, AdminError> {
        let bytes = match fs::read(self.root.join(CONFIG_FILE)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(AdminError::unavailable("导入入口配置暂不可读")),
        };
        let stored: StoredFile = serde_json::from_slice(&bytes)
            .map_err(|_| AdminError::internal("导入入口配置已损坏"))?;
        stored.into_configs()
    }

    fn save_all(&self, configs: &[PublicImportConfig]) -> Result<(), AdminError> {
        let unavailable = |_| AdminError::unavailable("导入入口配置暂不可写");
        fs::create_dir_all(&self.root).map_err(unavailable)?;
        let bytes = serde_json::to_vec_pretty(&StoredFile::from_configs(configs))
            .map_err(|_| AdminError::internal("导入入口配置无法序列化"))?;
        atomic_write(&self.root.join(CONFIG_FILE), &bytes).map_err(unavailable)
    }

    /// 校验令牌：遍历全部号商配置做常数时间比较，命中且入口开启、未过期、已配分组时返回该配置。
    /// 不因某条配置关闭而提前返回，避免通过时间差探测配置状态。
    fn authorize(&self, token: &str) -> Result<Option<PublicImportConfig>, AdminError> {
        let now = Utc::now();
        let mut hit: Option<PublicImportConfig> = None;
        for config in self.load_all()? {
            let matches: bool = token.as_bytes().ct_eq(config.token.as_bytes()).into();
            let usable =
                config.enabled && !config.expires_at.is_some_and(|at| at <= now) && !config.group_ids.is_empty();
            if matches && usable {
                hit = Some(config);
            }
        }
        Ok(hit)
    }

    /// 校验并规范化新建/修改配置的公共字段。
    async fn validate(&self, command: &UpdatePublicImportConfig) -> Result<String, AdminError> {
        let name = command.name.trim().to_owned();
        if name.is_empty() {
            return Err(AdminError::invalid("请填写号商名称"));
        }
        if name.chars().count() > 60 {
            return Err(AdminError::invalid("号商名称过长"));
        }
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
        Ok(name)
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
    async fn list(&self) -> Result<Vec<PublicImportConfig>, AdminError> {
        self.load_all()
    }

    async fn create(
        &self,
        context: &MutationContext,
        command: UpdatePublicImportConfig,
    ) -> Result<PublicImportConfig, AdminError> {
        let name = self.validate(&command).await?;
        let _guard = self.write_lock.lock().await;
        let mut configs = self.load_all()?;
        let config = PublicImportConfig {
            id: generate_id(),
            name,
            enabled: command.enabled,
            token: generate_token(),
            group_ids: command.group_ids,
            pin_turn_state: command.pin_turn_state,
            expires_at: command.expires_at,
            updated_at: Utc::now(),
        };
        configs.push(config.clone());
        self.save_all(&configs)?;
        tracing::info!(target: "public_import", request_id = %context.request_id, id = %config.id,
            enabled = config.enabled, groups = config.group_ids.len(), "public import config created");
        Ok(config)
    }

    async fn update(
        &self,
        context: &MutationContext,
        id: &str,
        command: UpdatePublicImportConfig,
    ) -> Result<PublicImportConfig, AdminError> {
        let name = self.validate(&command).await?;
        let _guard = self.write_lock.lock().await;
        let mut configs = self.load_all()?;
        let slot = configs
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or_else(|| AdminError::not_found("号商配置不存在，请刷新后重试"))?;
        slot.name = name;
        slot.enabled = command.enabled;
        slot.group_ids = command.group_ids;
        slot.pin_turn_state = command.pin_turn_state;
        slot.expires_at = command.expires_at;
        slot.updated_at = Utc::now();
        let config = slot.clone();
        self.save_all(&configs)?;
        tracing::info!(target: "public_import", request_id = %context.request_id, id = %config.id,
            enabled = config.enabled, groups = config.group_ids.len(), pin_turn_state = config.pin_turn_state,
            "public import config updated");
        Ok(config)
    }

    async fn delete(&self, context: &MutationContext, id: &str) -> Result<(), AdminError> {
        let _guard = self.write_lock.lock().await;
        let mut configs = self.load_all()?;
        let before = configs.len();
        configs.retain(|item| item.id != id);
        if configs.len() == before {
            return Err(AdminError::not_found("号商配置不存在，请刷新后重试"));
        }
        self.save_all(&configs)?;
        tracing::info!(target: "public_import", request_id = %context.request_id, id = %id,
            "public import config deleted");
        Ok(())
    }

    async fn rotate_token(
        &self,
        context: &MutationContext,
        id: &str,
    ) -> Result<PublicImportConfig, AdminError> {
        let _guard = self.write_lock.lock().await;
        let mut configs = self.load_all()?;
        let slot = configs
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or_else(|| AdminError::not_found("号商配置不存在，请刷新后重试"))?;
        slot.token = generate_token();
        slot.updated_at = Utc::now();
        let config = slot.clone();
        self.save_all(&configs)?;
        tracing::info!(target: "public_import", request_id = %context.request_id, id = %config.id,
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
                .import_new_accounts(ImportCredentials {
                    outbound_proxy_id: Some(proxy.id.clone()),
                    settings: Some(AccountImportSettings {
                        // 号商名写进备注，方便在账号列表追溯是哪个号商丢的号。
                        notes: Some(config.name.clone()),
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

    async fn import_tickets(
        &self,
        token: &str,
        request_id: &str,
        command: PublicTicketImport,
    ) -> Result<Option<PublicImportResult>, AdminError> {
        let Some(config) = self.authorize(token)? else {
            return Ok(None);
        };
        if command.tickets.is_empty() {
            return Err(AdminError::invalid("请至少填写一条票据"));
        }
        if command.tickets.len() > MAX_PUBLIC_IMPORT_ACCOUNTS {
            return Err(AdminError::invalid(format!(
                "单次最多导入 {MAX_PUBLIC_IMPORT_ACCOUNTS} 个账号"
            )));
        }
        // 必填且提前校验，避免登录建号成功后才发现成本信息不合法。
        crate::model::account_tickets::normalize_amount(&command.purchase_amount)?;
        crate::model::account_tickets::TicketCurrency::parse(&command.purchase_currency)?;
        if command.expires_at <= Utc::now() {
            return Err(AdminError::invalid("预计到期时间必须晚于当前时间"));
        }
        let proxies = self.eligible_proxies().await?;
        if proxies.is_empty() {
            return Err(AdminError::conflict("暂无通过测试的出站代理，请联系管理员"));
        }
        let context = MutationContext {
            actor: MutationActor::System,
            request_id: request_id.to_owned(),
        };
        let mut items = Vec::with_capacity(command.tickets.len());
        for (position, line) in command.tickets.iter().enumerate() {
            let proxy = &proxies[random_index(proxies.len())];
            items.push(
                self.import_one_ticket(&config, &context, &command, proxy, line, position + 1)
                    .await,
            );
        }
        Ok(Some(PublicImportResult { items }))
    }
}

impl DefaultPublicImportService {
    /// 单个票据：登录 → 只建新 OAuth 号 → 保存票据/成本/到期 → 按配置钉 state。
    async fn import_one_ticket(
        &self,
        config: &PublicImportConfig,
        context: &MutationContext,
        command: &PublicTicketImport,
        proxy: &ProxyRecord,
        line: &secrecy::SecretString,
        index: usize,
    ) -> PublicImportItem {
        use secrecy::ExposeSecret as _;
        let failed = |name: Option<String>, message: &str| PublicImportItem {
            index,
            name,
            status: PublicImportItemStatus::Failed,
            imported_accounts: 0,
            proxy_name: None,
            state_pinned: false,
            message: Some(message.to_owned()),
        };
        let secret =
            match crate::model::account_tickets::TicketSecret::parse_line(line.expose_secret()) {
                Ok(secret) => secret,
                Err(error) => return failed(None, error.message()),
            };
        let name = Some(crate::model::account_tickets::mask_email(&secret.email));
        let document = match self.openai.ticket_login(&secret, Some(&proxy.proxy)).await {
            Ok(document) => document,
            Err(error) => {
                tracing::warn!(target: "public_import", request_id = %context.request_id, index,
                    proxy_id = %proxy.id, reason = %error.message(), "public ticket login failed");
                return failed(name, error.message());
            }
        };
        let imported = match self
            .openai
            .import_new_accounts(ImportCredentials {
                outbound_proxy_id: Some(proxy.id.clone()),
                settings: Some(AccountImportSettings {
                    // 号商名写进备注，方便在账号列表追溯是哪个号商丢的号。
                    notes: Some(config.name.clone()),
                    enabled: true,
                    concurrency_limit: None,
                    weight: AccountWeight::DEFAULT,
                    model_access: None,
                    group_ids: config.group_ids.clone(),
                }),
                context: context.clone(),
                document,
            })
            .await
        {
            Ok(result) => result,
            Err(error) => return failed(name, error.message()),
        };
        for account_id in &imported.credential_ids {
            let saved = self
                .accounts
                .update_account_ticket(crate::model::account_tickets::UpdateAccountTicket {
                    context: context.clone(),
                    account_id: account_id.clone(),
                    purchase_amount: Some(command.purchase_amount.clone()),
                    purchase_currency: Some(command.purchase_currency.clone()),
                    purchased_at: Some(Utc::now()),
                    expires_at: Some(command.expires_at),
                    ticket_line: Some(line.clone()),
                    clear_ticket: false,
                })
                .await;
            if let Err(error) = saved {
                // 账号已入库；票据没存上只影响之后的自动复活，不回滚导入。
                tracing::warn!(target: "public_import", account_id = %account_id,
                    reason = %error.message(), "imported account left without ticket");
            }
        }
        let mut state_pinned = config.pin_turn_state && !imported.credential_ids.is_empty();
        if config.pin_turn_state {
            for account_id in &imported.credential_ids {
                state_pinned &= self.pin_turn_state(context, account_id.clone()).await;
            }
        }
        tracing::info!(target: "public_import", request_id = %context.request_id, index,
            proxy_id = %proxy.id, accounts = imported.credential_ids.len(), state_pinned,
            "public ticket import committed");
        PublicImportItem {
            index,
            name,
            status: PublicImportItemStatus::Imported,
            imported_accounts: imported.credential_ids.len(),
            proxy_name: Some(proxy.name.clone()),
            state_pinned,
            message: None,
        }
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

/// 迁移旧单配置时给它一个稳定 id（旧格式只可能有一个），避免每次读取都换 id 导致按 id 定位失败。
const LEGACY_CONFIG_ID: &str = "default";

fn generate_id() -> String {
    let mut bytes = [0u8; 8];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 落盘文件：新格式 `{ "configs": [...] }`；兼容旧的单配置扁平对象。
#[derive(Deserialize)]
#[serde(untagged)]
enum StoredFile {
    Multi(StoredFileMulti),
    Single(StoredConfig),
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredFileMulti {
    configs: Vec<StoredConfig>,
}

impl StoredFile {
    fn into_configs(self) -> Result<Vec<PublicImportConfig>, AdminError> {
        let stored = match self {
            StoredFile::Multi(multi) => multi.configs,
            StoredFile::Single(one) => vec![one],
        };
        stored.into_iter().map(StoredConfig::into_config).collect()
    }

    fn from_configs(configs: &[PublicImportConfig]) -> StoredFileMulti {
        StoredFileMulti {
            configs: configs.iter().map(StoredConfig::from).collect(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredConfig {
    /// 旧格式缺失时留空，读取时补稳定 id。
    #[serde(default)]
    id: String,
    /// 旧格式缺失时留空，读取时补默认号商名。
    #[serde(default)]
    name: String,
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
        let id = if self.id.trim().is_empty() {
            LEGACY_CONFIG_ID.to_owned()
        } else {
            self.id
        };
        let name = if self.name.trim().is_empty() {
            "默认号商".to_owned()
        } else {
            self.name
        };
        Ok(PublicImportConfig {
            id,
            name,
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
            id: config.id.clone(),
            name: config.name.clone(),
            enabled: config.enabled,
            token: config.token.clone(),
            group_ids: config.group_ids.iter().map(ToString::to_string).collect(),
            pin_turn_state: config.pin_turn_state,
            expires_at: config.expires_at.map(|at| at.to_rfc3339()),
            updated_at: config.updated_at.to_rfc3339(),
        }
    }
}
