//! 模板存储：内存里装全部 pin（热缓存），磁盘上只放账号级模板。
//!
//! 磁盘布局 `<root>/buckets/<账号>/<模型>.json`，路径就是桶键：文件内容与所在路径
//! 不一致即视为跨桶污染，丢弃不用。多实例（蓝绿）共享同一目录：写在跨进程锁内完成，
//! 读按文件 mtime 刷新（每桶最多每秒 stat 一次），让一方钉住的模板另一方也能看到。
//! 客户端级被动捕获的 pin 只在内存：按客户端密钥分桶、基数无界、丢了最多少捕获一轮。

use std::{
    collections::BTreeMap,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, SystemTime},
};

use serde::Serialize;

use crate::{
    fs_util,
    record::{BucketRecord, Source, TtlCaps, unix_seconds},
};

pub const MAX_PINS: usize = 2048;
const BUCKETS_DIR: &str = "buckets";
const INDEX_FILE: &str = "index.json";
const INDEX_VERSION: u32 = 1;
const DISK_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// pin 的作用域；`client = None` 是账号级。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Scope {
    pub account: String,
    pub binding: String,
    pub model: String,
    pub client: Option<String>,
}

impl Scope {
    pub fn account_wide(&self) -> Self {
        Self {
            client: None,
            ..self.clone()
        }
    }

    fn of(record: &BucketRecord) -> Self {
        Self {
            account: record.account.clone(),
            binding: record.binding.clone(),
            model: record.model.clone(),
            client: record.client.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("turn state store is unavailable")]
    Io,
    #[error("turn state bucket key is not a safe path component")]
    Path,
    #[error("turn state store is full")]
    Full,
}

struct DiskState {
    checked: Instant,
    mtime: Option<SystemTime>,
}

struct Inner {
    pins: BTreeMap<Scope, BucketRecord>,
    disk: BTreeMap<(String, String), DiskState>,
    /// 已作废但磁盘文件没删掉的账号级模板（桶键 → 被作废的值）。文件删不掉时靠它挡住
    /// 重新加载；同一个桶换了别的值或文件终于删掉后移除。
    revoked: BTreeMap<(String, String), String>,
}

impl Inner {
    fn prune(&mut self, now: SystemTime, caps: TtlCaps) {
        self.pins.retain(|_, record| record.active_under(now, caps));
    }
}

/// 不实现 Debug，防止票据值被日志意外展开。
pub struct PinStore {
    root: Option<PathBuf>,
    inner: Mutex<Inner>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexEntry {
    len: usize,
    issued_at: u64,
    expires_at: u64,
    source: Source,
    egress: Option<String>,
    ready: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    version: u32,
    updated_at: u64,
    buckets: BTreeMap<String, IndexEntry>,
}

impl PinStore {
    pub fn in_memory() -> Self {
        Self {
            root: None,
            inner: Mutex::new(Inner {
                pins: BTreeMap::new(),
                disk: BTreeMap::new(),
                revoked: BTreeMap::new(),
            }),
        }
    }

    pub fn open(root: &Path) -> Result<Self, StoreError> {
        fs_util::ensure_dir(&root.join(BUCKETS_DIR)).map_err(|_| StoreError::Io)?;
        Ok(Self {
            root: Some(root.to_path_buf()),
            inner: Mutex::new(Inner {
                pins: BTreeMap::new(),
                disk: BTreeMap::new(),
                revoked: BTreeMap::new(),
            }),
        })
    }

    fn buckets_dir(&self) -> Option<PathBuf> {
        self.root.as_ref().map(|root| root.join(BUCKETS_DIR))
    }

    fn account_dir(&self, account: &str) -> Option<PathBuf> {
        Some(
            self.buckets_dir()?
                .join(fs_util::sanitize_component(account)?),
        )
    }

    fn bucket_path(&self, account: &str, model: &str) -> Option<PathBuf> {
        Some(
            self.account_dir(account)?
                .join(format!("{}.json", fs_util::sanitize_component(model)?)),
        )
    }

    /// 让内存里的账号级条目跟上磁盘：每桶最多每秒 stat 一次，mtime 没变就不读。
    fn refresh_bucket(
        &self,
        inner: &mut Inner,
        account: &str,
        model: &str,
        now: SystemTime,
        caps: TtlCaps,
    ) {
        let Some(path) = self.bucket_path(account, model) else {
            return;
        };
        let key = (account.to_owned(), model.to_owned());
        let now_instant = Instant::now();
        if inner
            .disk
            .get(&key)
            .is_some_and(|state| now_instant.duration_since(state.checked) < DISK_CHECK_INTERVAL)
        {
            return;
        }
        let previous = inner.disk.get(&key).and_then(|state| state.mtime);
        let mtime = match fs::metadata(&path) {
            Ok(meta) => meta.modified().ok(),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                inner.pins.retain(|scope, record| {
                    !(record.account_wide() && scope.account == account && scope.model == model)
                });
                inner.disk.insert(
                    key,
                    DiskState {
                        checked: now_instant,
                        mtime: None,
                    },
                );
                return;
            }
            Err(_) => return,
        };
        inner.disk.insert(
            key.clone(),
            DiskState {
                checked: now_instant,
                mtime,
            },
        );
        // 有作废标记时不走 mtime 捷径：遗留文件要一直重试删除。
        if mtime.is_some() && mtime == previous && !inner.revoked.contains_key(&key) {
            return;
        }
        let Some(record) = read_record(&path, account, model) else {
            let _ = fs_util::remove_file_if_exists(&path);
            return;
        };
        // 作废时没删掉的文件：不重新接纳，顺手再删一次；删掉了才撤下标记。
        if inner.revoked.get(&key) == Some(&record.value) {
            if self.remove_revoked_file(&path, account, model, &record.value, now) {
                inner.revoked.remove(&key);
            }
            return;
        }
        inner.revoked.remove(&key);
        inner.pins.retain(|scope, existing| {
            !(existing.account_wide() && scope.account == account && scope.model == model)
        });
        if !record.active(now) {
            let _ = fs_util::remove_file_if_exists(&path);
            return;
        }
        // 只是被当前设置封顶的票不删文件：设置可能被调回去，另一个实例也可能用着别的设置。
        if !record.active_under(now, caps) {
            return;
        }
        if inner.pins.len() < MAX_PINS {
            inner.pins.insert(Scope::of(&record), record);
        }
    }

    fn refresh_account(&self, inner: &mut Inner, account: &str, now: SystemTime, caps: TtlCaps) {
        let Some(dir) = self.account_dir(account) else {
            return;
        };
        for file in fs_util::json_files(&dir) {
            if let Some(model) = file
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(decode_component)
            {
                self.refresh_bucket(inner, account, &model, now, caps);
            }
        }
    }

    fn refresh_all(&self, inner: &mut Inner, now: SystemTime, caps: TtlCaps) {
        let Some(dir) = self.buckets_dir() else {
            return;
        };
        for account_dir in fs_util::subdirs(&dir) {
            if let Some(account) = account_dir
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(decode_component)
            {
                self.refresh_account(inner, &account, now, caps);
            }
        }
    }

    /// 查找可复用的模板：客户端自己的 pin 优先，没有时回退到账号级；账号级只属于
    /// 观测到它的那个出口。返回值与命中的作用域，不计 hits（由调用方在真正注入时计）。
    pub fn lookup(
        &self,
        scope: &Scope,
        egress: &str,
        now: SystemTime,
        caps: TtlCaps,
    ) -> Option<(String, Scope)> {
        self.lookup_on_route(scope, egress, now, caps, None)
    }

    /// 云端票只能在当前路由子作用域中读取，无路由证明的旧云端票不再回放。
    pub fn lookup_on_route(
        &self,
        scope: &Scope,
        egress: &str,
        now: SystemTime,
        caps: TtlCaps,
        route_fingerprint: Option<&str>,
    ) -> Option<(String, Scope)> {
        let mut inner = self.inner.lock().ok()?;
        inner.prune(now, caps);
        self.refresh_bucket(&mut inner, &scope.account, &scope.model, now, caps);
        if let Some(record) = inner.pins.get(scope)
            && record.source != Source::Mint
        {
            return Some((record.value.clone(), scope.clone()));
        }
        if let Some(route) = route_fingerprint {
            let mut routed = scope.account_wide();
            routed.binding = crate::binding::mint_binding(&scope.binding, route);
            if let Some(record) = inner.pins.get(&routed)
                && record.source == Source::Mint
                && record.egress.as_deref() == Some(egress)
            {
                return Some((record.value.clone(), routed));
            }
        }
        let account_wide = scope.account_wide();
        let record = inner
            .pins
            .get(&account_wide)
            .filter(|record| record.source != Source::Mint)?;
        if record
            .egress
            .as_deref()
            .is_some_and(|probed| probed != egress)
        {
            return None;
        }
        Some((record.value.clone(), account_wide))
    }

    pub fn hit(&self, scope: &Scope) {
        if let Ok(mut inner) = self.inner.lock()
            && let Some(record) = inner.pins.get_mut(scope)
        {
            record.hits = record.hits.saturating_add(1);
        }
    }

    /// 某账号某凭据绑定下全部有效 pin（账号级 + 客户端级）。
    pub fn status(
        &self,
        account: &str,
        binding: &str,
        now: SystemTime,
        caps: TtlCaps,
    ) -> Vec<BucketRecord> {
        let Ok(mut inner) = self.inner.lock() else {
            return Vec::new();
        };
        inner.prune(now, caps);
        self.refresh_account(&mut inner, account, now, caps);
        inner
            .pins
            .iter()
            .filter(|(scope, _)| {
                scope.account == account
                    && crate::binding::credential_scope(&scope.binding) == binding
            })
            .map(|(_, record)| record.clone().capped(caps))
            .collect()
    }

    pub fn account_wide_record(
        &self,
        account: &str,
        binding: &str,
        model: &str,
        egress: &str,
        now: SystemTime,
        caps: TtlCaps,
    ) -> Option<BucketRecord> {
        let mut inner = self.inner.lock().ok()?;
        inner.prune(now, caps);
        self.refresh_bucket(&mut inner, account, model, now, caps);
        let scope = Scope {
            account: account.to_owned(),
            binding: binding.to_owned(),
            model: model.to_owned(),
            client: None,
        };
        inner
            .pins
            .get(&scope)
            .filter(|record| record.egress.as_deref() == Some(egress))
            .cloned()
            .map(|record| record.capped(caps))
    }

    /// 管理员/续期显式钉住账号级模板：替换该账号该模型该绑定下的全部旧 pin。
    /// 磁盘上已有同绑定、签发时间不早于本次的有效模板时不覆盖（多实例同时续期时后到者让步），
    /// 返回实际生效模板的到期时间。
    pub fn pin_account_wide(
        &self,
        record: BucketRecord,
        now: SystemTime,
        caps: TtlCaps,
    ) -> Result<SystemTime, StoreError> {
        let mut inner = self.inner.lock().map_err(|_| StoreError::Io)?;
        inner.prune(now, caps);
        let mut effective = record;
        if let Some(root) = &self.root {
            let path = self
                .bucket_path(&effective.account, &effective.model)
                .ok_or(StoreError::Path)?;
            let _guard = fs_util::lock(root).map_err(|_| StoreError::Io)?;
            let key = (effective.account.clone(), effective.model.clone());
            // 作废后没删掉的旧文件不能在这里被当成「更新的同绑定模板」而保留下来。
            let stale = inner.revoked.remove(&key);
            if let Some(existing) = read_record(&path, &effective.account, &effective.model)
                && stale.as_ref() != Some(&existing.value)
                && existing.active_under(now, caps)
                && existing.binding == effective.binding
                && existing.issued_at >= effective.issued_at
            {
                effective = existing;
            } else {
                let bytes = serde_json::to_vec_pretty(&effective).map_err(|_| StoreError::Io)?;
                let dir = path.parent().ok_or(StoreError::Path)?;
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or(StoreError::Path)?;
                fs_util::atomic_write(dir, name, &bytes).map_err(|_| StoreError::Io)?;
                rewrite_index(root, now);
            }
            inner.disk.insert(
                (effective.account.clone(), effective.model.clone()),
                DiskState {
                    checked: Instant::now(),
                    mtime: fs::metadata(&path)
                        .ok()
                        .and_then(|meta| meta.modified().ok()),
                },
            );
        }
        let account = effective.account.clone();
        let binding = effective.binding.clone();
        let model = effective.model.clone();
        inner.pins.retain(|scope, _| {
            !(scope.account == account
                && crate::binding::credential_scope(&scope.binding)
                    == crate::binding::credential_scope(&binding)
                && scope.model == model)
        });
        if inner.pins.len() >= MAX_PINS {
            return Err(StoreError::Full);
        }
        let expires_at = effective.expires_under(caps);
        inner.pins.insert(Scope::of(&effective), effective);
        Ok(expires_at)
    }

    /// 被动捕获：本请求没有复用 pin、同一出口上没有账号级模板、且该客户端还没有 pin 时写入。
    pub fn insert_passive(
        &self,
        record: BucketRecord,
        egress: &str,
        now: SystemTime,
        caps: TtlCaps,
    ) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        inner.prune(now, caps);
        self.refresh_bucket(&mut inner, &record.account, &record.model, now, caps);
        let scope = Scope::of(&record);
        if inner.pins.iter().any(|(candidate, pin)| {
            candidate.account == scope.account
                && candidate.model == scope.model
                && candidate.client.is_none()
                && crate::binding::credential_scope(&candidate.binding) == scope.binding
                && pin.egress.as_deref() == Some(egress)
        }) {
            return false;
        }
        if inner.pins.len() >= MAX_PINS || inner.pins.contains_key(&scope) {
            return false;
        }
        inner.pins.insert(scope, record);
        true
    }

    /// 在目录锁内删除仍是 `value` 的桶文件；已是别的值时视为成功。返回磁盘上是否已不再有它。
    fn remove_revoked_file(
        &self,
        path: &Path,
        account: &str,
        model: &str,
        value: &str,
        now: SystemTime,
    ) -> bool {
        let Some(root) = &self.root else {
            return true;
        };
        let removed = fs_util::lock(root).and_then(|_guard| {
            if read_record(path, account, model).is_some_and(|record| record.value == value) {
                fs_util::remove_file_if_exists(path)?;
                rewrite_index(root, now);
            }
            Ok(())
        });
        if let Err(error) = &removed {
            tracing::warn!(
                target: "turn_state",
                account,
                model,
                error = %error,
                "[turn-state] a revoked template could not be removed from disk; it stays blocked in memory"
            );
        }
        removed.is_ok()
    }

    /// 条件失效：只有 `scope` 下现存的值仍是 `value` 时才删除，返回是否删了。
    /// 期间别的请求或续期已经换上新值时不动它；账号级模板连同磁盘文件一起删，
    /// 磁盘上已是别的值（另一个实例刚续上）时同样保留。文件删不掉（拿不到目录锁、删除出错）
    /// 时记日志并在本进程内挡住这个值，之后每次刷新这个桶都会再删一次。
    pub fn invalidate(&self, scope: &Scope, value: &str, now: SystemTime) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        if inner
            .pins
            .get(scope)
            .is_none_or(|record| record.value != value)
        {
            return false;
        }
        inner.pins.remove(scope);
        if scope.client.is_some() {
            return true;
        }
        let key = (scope.account.clone(), scope.model.clone());
        inner.disk.remove(&key);
        if let Some(path) = self.bucket_path(&scope.account, &scope.model)
            && !self.remove_revoked_file(&path, &scope.account, &scope.model, value, now)
        {
            inner.revoked.insert(key, value.to_owned());
        }
        true
    }

    pub fn clear_account(&self, account: &str) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.pins.retain(|scope, _| scope.account != account);
        inner.disk.retain(|(existing, _), _| existing != account);
        inner.revoked.retain(|(existing, _), _| existing != account);
        if let (Some(root), Some(dir)) = (&self.root, self.account_dir(account))
            && let Ok(_guard) = fs_util::lock(root)
        {
            let _ = fs_util::remove_dir_if_exists(&dir);
            rewrite_index(root, SystemTime::now());
        }
    }

    /// 清掉一个账号（可限定模型）的全部模板；返回清掉的内存条目数。
    pub fn clear_bucket(&self, account: &str, model: Option<&str>) -> usize {
        let Ok(mut inner) = self.inner.lock() else {
            return 0;
        };
        let before = inner.pins.len();
        inner.pins.retain(|scope, _| {
            !(scope.account == account && model.is_none_or(|m| scope.model == m))
        });
        inner
            .disk
            .retain(|(a, m), _| !(a == account && model.is_none_or(|wanted| m == wanted)));
        inner
            .revoked
            .retain(|(a, m), _| !(a == account && model.is_none_or(|wanted| m == wanted)));
        if let Some(root) = &self.root
            && let Ok(_guard) = fs_util::lock(root)
        {
            match model {
                Some(model) => {
                    if let Some(path) = self.bucket_path(account, model) {
                        let _ = fs_util::remove_file_if_exists(&path);
                    }
                }
                None => {
                    if let Some(dir) = self.account_dir(account) {
                        let _ = fs_util::remove_dir_if_exists(&dir);
                    }
                }
            }
            rewrite_index(root, SystemTime::now());
        }
        before - inner.pins.len()
    }

    /// 全部有效 pin 的快照（先把磁盘上别的实例写的桶拉进来）。
    pub fn records(&self, now: SystemTime, caps: TtlCaps) -> Vec<BucketRecord> {
        let Ok(mut inner) = self.inner.lock() else {
            return Vec::new();
        };
        inner.prune(now, caps);
        self.refresh_all(&mut inner, now, caps);
        inner
            .pins
            .values()
            .map(|record| record.clone().capped(caps))
            .collect()
    }
}

fn read_record(path: &Path, account: &str, model: &str) -> Option<BucketRecord> {
    let bytes = fs::read(path).ok()?;
    let record: BucketRecord = serde_json::from_slice(&bytes).ok()?;
    if !record.matches_path(account, model) || !record.consistent() || !record.account_wide() {
        tracing::warn!(
            target: "turn_state",
            account,
            model,
            "[turn-state] dropping a bucket file whose contents disagree with its path"
        );
        return None;
    }
    Some(record)
}

/// 还原 `sanitize_component` 的百分号编码。
fn decode_component(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            let value = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            out.push(value);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// 重建无值索引；调用方须持有目录锁。
fn rewrite_index(root: &Path, now: SystemTime) {
    let mut buckets = BTreeMap::new();
    for account_dir in fs_util::subdirs(&root.join(BUCKETS_DIR)) {
        for file in fs_util::json_files(&account_dir) {
            let Ok(bytes) = fs::read(&file) else {
                continue;
            };
            let Ok(record) = serde_json::from_slice::<BucketRecord>(&bytes) else {
                continue;
            };
            buckets.insert(
                format!("{}/{}", record.account, record.model),
                IndexEntry {
                    len: record.len,
                    issued_at: unix_seconds(record.issued_at),
                    expires_at: unix_seconds(record.expires_at),
                    source: record.source,
                    egress: record.egress.clone(),
                    ready: record.active(now),
                },
            );
        }
    }
    let index = Index {
        version: INDEX_VERSION,
        updated_at: unix_seconds(now),
        buckets,
    };
    if let Ok(bytes) = serde_json::to_vec_pretty(&index) {
        let _ = fs_util::atomic_write(root, INDEX_FILE, &bytes);
    }
}
