//! 命名代理持有认证信息，账号保留解析后的 URL 供 Provider 传输使用。

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use gateway_admin::{
    model::{
        MutationContext, Revision as AdminRevision,
        account_groups::{AccountGroupColor, AccountGroupRef},
        proxies::*,
    },
    ports::{
        proxy::{ProxyImportGuard, ProxyImportReservation, ProxyStore},
        store::{AdminStoreError, AdminStoreResult},
    },
};
use gateway_core::{
    account::{OutboundProxy, ProviderAccountId},
    routing::AccountGroupId,
};
use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder, Row as _, Transaction, postgres::PgRow};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{append_admin_audit_event_in_transaction, bump_config_revision_in_transaction};
use crate::{
    ConflictKind, Revision, StoreError, StoreResult, admin_revision, admin_store_error,
    mutation_audit, postgres_unavailable,
};

const ENTITY: &str = "outbound proxy";
// geo/质量列迁到子表 outbound_proxy_quality（fork 迁移 9002），读侧 LEFT JOIN 取回；
// 按原列名投影，record()/exit_geo_from_row/quality_snapshot_from_row 无需改动。
// quality_report(大 jsonb)不在列表/详情里读，仅 quality_report() 单查，故此处不投影。
const SELECT: &str = "select p.*,
    q.last_test_country, q.last_test_country_code, q.last_test_region, q.last_test_city,
    q.quality_checked_at, q.quality_score, q.quality_grade, q.quality_status, q.quality_summary,
    (select count(*) from provider_accounts a where a.outbound_proxy_id = p.id) as account_count
    from outbound_proxies p
    left join outbound_proxy_quality q on q.proxy_id = p.id";

#[derive(Clone)]
pub struct PgProxyRepository {
    pool: PgPool,
    import_slots: Arc<Semaphore>,
}

impl PgProxyRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            import_slots: Arc::new(Semaphore::new(4)),
        }
    }
}

struct PgProxyImportGuard {
    // 独立会话持有咨询锁，避免空闲事务超时，也不给提交事务占用的连接池制造死锁。
    // 每个仓储最多增加四个会话；取消请求或进程退出时，关闭连接会自动释放锁。
    _connection: PgConnection,
    _slot: OwnedSemaphorePermit,
}

impl ProxyImportGuard for PgProxyImportGuard {}

async fn exclude_active_imports(
    transaction: &mut Transaction<'_, Postgres>,
    id: &str,
) -> StoreResult<()> {
    let acquired: bool =
        sqlx::query_scalar("select pg_try_advisory_xact_lock(hashtextextended($1, 739219))")
            .bind(id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|_| unavailable())?;
    if !acquired {
        return Err(conflict(id));
    }
    Ok(())
}

fn store_error(error: StoreError) -> AdminStoreError {
    admin_store_error(ENTITY, error)
}
fn unavailable() -> StoreError {
    postgres_unavailable("outbound proxy operation")
}
fn conflict(id: &str) -> StoreError {
    StoreError::Conflict {
        entity: ENTITY,
        id: id.to_owned(),
        kind: ConflictKind::InvalidTransition,
    }
}
fn invalid() -> StoreError {
    StoreError::InvalidData {
        entity: ENTITY,
        message: "invalid proxy record".to_owned(),
    }
}

fn record(row: PgRow) -> StoreResult<ProxyRecord> {
    let success: Option<bool> = row.try_get("last_test_success").map_err(|_| invalid())?;
    let ip: Option<String> = row.try_get("last_test_ip").map_err(|_| invalid())?;
    let ipv4: Option<String> = row.try_get("last_test_ipv4").map_err(|_| invalid())?;
    let ipv6: Option<String> = row.try_get("last_test_ipv6").map_err(|_| invalid())?;
    let latency: Option<i64> = row.try_get("last_test_latency_ms").map_err(|_| invalid())?;
    Ok(ProxyRecord {
        location: location_from_row(&row)?,
        id: row.try_get("id").map_err(|_| invalid())?,
        name: row.try_get("name").map_err(|_| invalid())?,
        proxy: OutboundProxy::parse(
            &row.try_get::<String, _>("proxy_url")
                .map_err(|_| invalid())?,
        )
        .map_err(|_| invalid())?,
        revision: AdminRevision::new(
            u64::try_from(row.try_get::<i64, _>("revision").map_err(|_| invalid())?)
                .map_err(|_| invalid())?,
        )
        .map_err(|_| invalid())?,
        account_count: u64::try_from(
            row.try_get::<i64, _>("account_count")
                .map_err(|_| invalid())?,
        )
        .map_err(|_| invalid())?,
        last_test_at: row.try_get("last_test_at").map_err(|_| invalid())?,
        last_test: success
            .map(|success| -> StoreResult<_> {
                Ok(ProxyTestResult {
                    success,
                    latency_ms: u64::try_from(latency.ok_or_else(invalid)?)
                        .map_err(|_| invalid())?,
                    exit_ip: ip.map(|ip| ip.parse().map_err(|_| invalid())).transpose()?,
                    exit_geo: exit_geo_from_row(&row)?,
                    exit_ipv4: ipv4
                        .map(|ip| ip.parse().map_err(|_| invalid()))
                        .transpose()?,
                    exit_ipv6: ipv6
                        .map(|ip| ip.parse().map_err(|_| invalid()))
                        .transpose()?,
                    message: row
                        .try_get::<Option<String>, _>("last_test_message")
                        .map_err(|_| invalid())?
                        .unwrap_or_default(),
                })
            })
            .transpose()?,
        quality: quality_snapshot_from_row(&row)?,
        created_at: row.try_get("created_at").map_err(|_| invalid())?,
        updated_at: row.try_get("updated_at").map_err(|_| invalid())?,
    })
}

fn exit_geo_from_row(row: &PgRow) -> StoreResult<Option<ProxyExitGeo>> {
    let country: Option<String> = row.try_get("last_test_country").map_err(|_| invalid())?;
    country
        .map(|country| {
            Ok(ProxyExitGeo {
                country,
                country_code: row
                    .try_get::<Option<String>, _>("last_test_country_code")
                    .map_err(|_| invalid())?
                    .ok_or_else(invalid)?,
                region: row.try_get("last_test_region").map_err(|_| invalid())?,
                city: row.try_get("last_test_city").map_err(|_| invalid())?,
            })
        })
        .transpose()
}

fn quality_snapshot_from_row(row: &PgRow) -> StoreResult<Option<ProxyQualitySnapshot>> {
    let checked_at: Option<chrono::DateTime<chrono::Utc>> =
        row.try_get("quality_checked_at").map_err(|_| invalid())?;
    checked_at
        .map(|checked_at| {
            let text = |column: &str| -> StoreResult<String> {
                row.try_get::<Option<String>, _>(column)
                    .map_err(|_| invalid())?
                    .ok_or_else(invalid)
            };
            let score: i16 = row
                .try_get::<Option<i16>, _>("quality_score")
                .map_err(|_| invalid())?
                .ok_or_else(invalid)?;
            Ok(ProxyQualitySnapshot {
                score: u8::try_from(score).map_err(|_| invalid())?,
                grade: text("quality_grade")?.chars().next().ok_or_else(invalid)?,
                status: ProxyQualityStatus::parse(&text("quality_status")?).ok_or_else(invalid)?,
                summary: text("quality_summary")?,
                checked_at,
            })
        })
        .transpose()
}

/// 报告文档只保存列表结论之外的明细，结论以列为准，避免两处各执一词。
fn quality_report_document(report: &ProxyQualityReport) -> serde_json::Value {
    serde_json::json!({
        "exitIp": report.exit_ip.map(|ip| ip.to_string()),
        "exitGeo": report.exit_geo.as_ref().map(|geo| serde_json::json!({
            "country": geo.country,
            "countryCode": geo.country_code,
            "region": geo.region,
            "city": geo.city,
        })),
        "baseLatencyMs": report.base_latency_ms,
        "passedCount": report.passed_count,
        "warnCount": report.warn_count,
        "failedCount": report.failed_count,
        "challengeCount": report.challenge_count,
        "items": report.items.iter().map(|item| serde_json::json!({
            "target": item.target,
            "status": item.status.as_str(),
            "httpStatus": item.http_status,
            "latencyMs": item.latency_ms,
            "message": item.message,
            "cfRay": item.cf_ray,
        })).collect::<Vec<_>>(),
    })
}

fn quality_report_from_document(
    snapshot: ProxyQualitySnapshot,
    document: &serde_json::Value,
) -> StoreResult<ProxyQualityReport> {
    let text = |value: &serde_json::Value| value.as_str().map(str::to_owned);
    let count = |key: &str| -> StoreResult<u32> {
        document[key]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(invalid)
    };
    let geo = &document["exitGeo"];
    Ok(ProxyQualityReport {
        snapshot,
        exit_ip: text(&document["exitIp"])
            .map(|ip| ip.parse().map_err(|_| invalid()))
            .transpose()?,
        exit_geo: (!geo.is_null())
            .then(|| -> StoreResult<_> {
                Ok(ProxyExitGeo {
                    country: text(&geo["country"]).ok_or_else(invalid)?,
                    country_code: text(&geo["countryCode"]).ok_or_else(invalid)?,
                    region: text(&geo["region"]),
                    city: text(&geo["city"]),
                })
            })
            .transpose()?,
        base_latency_ms: document["baseLatencyMs"].as_u64(),
        passed_count: count("passedCount")?,
        warn_count: count("warnCount")?,
        failed_count: count("failedCount")?,
        challenge_count: count("challengeCount")?,
        items: document["items"]
            .as_array()
            .ok_or_else(invalid)?
            .iter()
            .map(|item| {
                Ok(ProxyQualityItem {
                    target: text(&item["target"]).ok_or_else(invalid)?,
                    status: item["status"]
                        .as_str()
                        .and_then(ProxyQualityItemStatus::parse)
                        .ok_or_else(invalid)?,
                    http_status: item["httpStatus"]
                        .as_u64()
                        .and_then(|value| u16::try_from(value).ok()),
                    latency_ms: item["latencyMs"].as_u64(),
                    message: text(&item["message"]).unwrap_or_default(),
                    cf_ray: text(&item["cfRay"]),
                })
            })
            .collect::<StoreResult<_>>()?,
    })
}

/// 连通性结果的五个基础列之后依次绑定地区四列；失败结果的地区恒为空以满足表约束。
// 父表 outbound_proxies 只保连通性 + 上游双栈列；geo（country/code/region/city）迁子表，见 upsert_test_geo。
const RECORD_TEST_SET: &str = "last_test_at = now(), last_test_success = $3, last_test_latency_ms = $4,      last_test_ip = $5, last_test_ipv4 = $6, last_test_ipv6 = $7, last_test_message = $8";
// 质量检测复用连通性基础探测，但 ProxyQualityReport 不携带双栈地址；用 coalesce 保留上次测得的
// last_test_ipv4/ipv6，避免「先普通测试拿到双栈、再点质量检测把双栈清空」的回归。
const RECORD_QUALITY_TEST_SET: &str = "last_test_at = now(), last_test_success = $3, last_test_latency_ms = $4,      last_test_ip = $5, last_test_ipv4 = coalesce($6, last_test_ipv4), last_test_ipv6 = coalesce($7, last_test_ipv6), last_test_message = $8";

fn bind_test_result<'q>(
    query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    result: &ProxyTestResult,
) -> StoreResult<sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>> {
    Ok(query
        .bind(result.success)
        .bind(i64::try_from(result.latency_ms).map_err(|_| invalid())?)
        .bind(result.exit_ip.map(|ip| ip.to_string()))
        .bind(result.exit_ipv4.map(|ip| ip.to_string()))
        .bind(result.exit_ipv6.map(|ip| ip.to_string()))
        .bind(result.message.clone()))
}

/// 出口地区只在连通成功时保留；失败或无地区时四列均为空（与原「每次测试重置 geo」一致）。
fn test_geo_columns(
    result: &ProxyTestResult,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    match result.exit_geo.clone().filter(|_| result.success) {
        Some(geo) => (
            Some(geo.country),
            Some(geo.country_code),
            geo.region,
            geo.city,
        ),
        None => (None, None, None, None),
    }
}

/// 把出口地区写进子表；仅更新 geo 四列，保留既有质量快照（对应原来「普通测试不动 quality 列」）。
async fn upsert_test_geo(
    transaction: &mut Transaction<'_, Postgres>,
    id: &str,
    result: &ProxyTestResult,
) -> StoreResult<()> {
    let (country, code, region, city) = test_geo_columns(result);
    sqlx::query(
        "insert into outbound_proxy_quality
           (proxy_id, last_test_country, last_test_country_code, last_test_region, last_test_city)
         values ($1, $2, $3, $4, $5)
         on conflict (proxy_id) do update set
           last_test_country = excluded.last_test_country,
           last_test_country_code = excluded.last_test_country_code,
           last_test_region = excluded.last_test_region,
           last_test_city = excluded.last_test_city",
    )
    .bind(id)
    .bind(country)
    .bind(code)
    .bind(region)
    .bind(city)
    .execute(&mut **transaction)
    .await
    .map_err(|_| unavailable())?;
    Ok(())
}

pub(crate) fn location_from_row(
    row: &PgRow,
) -> StoreResult<Option<gateway_core::account::RequestLocation>> {
    let country: Option<String> = row.try_get("location_country").map_err(|_| invalid())?;
    country
        .map(|country| {
            gateway_core::account::RequestLocation {
                country,
                region: row.try_get("location_region").map_err(|_| invalid())?,
                city: row.try_get("location_city").map_err(|_| invalid())?,
                timezone: row
                    .try_get::<String, _>("location_timezone")
                    .map_err(|_| invalid())?
                    .parse()
                    .map_err(|_| invalid())?,
            }
            .normalized()
            .map_err(|_| invalid())
        })
        .transpose()
}

async fn save_location(
    transaction: &mut Transaction<'_, Postgres>,
    id: &str,
    location: Option<&gateway_core::account::RequestLocation>,
) -> StoreResult<()> {
    if let Some(location) = location {
        location.validate().map_err(|_| invalid())?;
    }
    sqlx::query("update outbound_proxies set location_country = $2, location_region = $3, location_city = $4, location_timezone = $5 where id = $1")
        .bind(id)
        .bind(location.map(|value| value.country.as_str()))
        .bind(location.map(|value| value.region.trim()))
        .bind(location.map(|value| value.city.trim()))
        .bind(location.map(|value| value.timezone.name()))
        .execute(&mut **transaction).await.map_err(|_| unavailable())?;
    Ok(())
}

async fn lock_url(
    transaction: &mut Transaction<'_, Postgres>,
    proxy: &OutboundProxy,
) -> StoreResult<()> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 739218))")
        .bind(proxy.expose_url())
        .execute(&mut **transaction)
        .await
        .map_err(|_| unavailable())?;
    Ok(())
}

/// 导入和旧版 URL 写入在账号事务内登记到共享代理目录。
pub(crate) async fn ensure_proxy_for_url(
    transaction: &mut Transaction<'_, Postgres>,
    proxy: &OutboundProxy,
    name: Option<&str>,
) -> StoreResult<(String, bool)> {
    lock_url(transaction, proxy).await?;
    if let Some(id) = sqlx::query_scalar::<_, String>("select id from outbound_proxies where proxy_url = $1 order by created_at, id limit 1 for share")
        .bind(proxy.expose_url()).fetch_optional(&mut **transaction).await.map_err(|_| unavailable())? {
        return Ok((id, false));
    }
    let id = format!("proxy_{}", uuid::Uuid::now_v7().simple());
    let generated_name = format!("Imported proxy {}", &id[id.len() - 8..]);
    sqlx::query("insert into outbound_proxies (id, name, proxy_url) values ($1, $2, $3)")
        .bind(&id)
        .bind(name.unwrap_or(&generated_name))
        .bind(proxy.expose_url())
        .execute(&mut **transaction)
        .await
        .map_err(|_| unavailable())?;
    Ok((id, true))
}

pub(crate) async fn resolve_proxy_selection(
    transaction: &mut Transaction<'_, Postgres>,
    selection: &AccountProxySelection,
) -> StoreResult<(Option<String>, Option<OutboundProxy>)> {
    match selection {
        AccountProxySelection::Direct => Ok((None, None)),
        AccountProxySelection::Url(proxy) => {
            let (id, _) = ensure_proxy_for_url(transaction, proxy, None).await?;
            Ok((Some(id), Some(proxy.clone())))
        }
        AccountProxySelection::Saved(id) => {
            let value: String = sqlx::query_scalar(
                "select proxy_url from outbound_proxies where id = $1 for share",
            )
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(|| StoreError::NotFound {
                entity: ENTITY,
                id: id.clone(),
            })?;
            Ok((
                Some(id.clone()),
                Some(OutboundProxy::parse(&value).map_err(|_| invalid())?),
            ))
        }
    }
}

async fn audit(
    transaction: &mut Transaction<'_, Postgres>,
    context: &MutationContext,
    action: &str,
    id: &str,
    fields: &[&str],
    revision: Revision,
) -> StoreResult<()> {
    append_admin_audit_event_in_transaction(
        transaction,
        mutation_audit(
            context,
            action,
            "outbound_proxy",
            id,
            fields.iter().map(|value| (*value).to_owned()).collect(),
        ),
        revision,
    )
    .await
}

#[async_trait]
impl ProxyStore for PgProxyRepository {
    async fn remove_account(
        &self,
        proxy_id: &str,
        account_id: &ProviderAccountId,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminRevision> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| store_error(unavailable()))?;
        let revision = bump_config_revision_in_transaction(&mut transaction)
            .await
            .map_err(store_error)?;
        // 在同一条更新中校验绑定，避免旧弹窗清除账号后来选择的其他代理。
        let updated = sqlx::query(
            "update provider_accounts
             set outbound_proxy_id = null, outbound_proxy_url = null,
                 updated_at = greatest(now(), updated_at)
             where id = $1 and outbound_proxy_id = $2",
        )
        .bind(account_id.as_str())
        .bind(proxy_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| store_error(unavailable()))?;
        if updated.rows_affected() != 1 {
            return Err(store_error(conflict(proxy_id)));
        }
        append_admin_audit_event_in_transaction(
            &mut transaction,
            mutation_audit(
                context,
                "update",
                "provider_account",
                account_id.as_str(),
                vec!["outbound_proxy".to_owned()],
            ),
            revision,
        )
        .await
        .map_err(store_error)?;
        transaction
            .commit()
            .await
            .map_err(|_| store_error(unavailable()))?;
        admin_revision(revision)
    }

    async fn list_accounts(
        &self,
        query: ProxyAccountListQuery,
    ) -> AdminStoreResult<ProxyAccountPage> {
        if query.page == 0 {
            return Err(store_error(invalid()));
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| store_error(unavailable()))?;
        // 数量和当前页共用只读快照，避免绑定变化使同一次响应的分页事实不一致。
        sqlx::query("set transaction isolation level repeatable read, read only")
            .execute(&mut *transaction)
            .await
            .map_err(|_| store_error(unavailable()))?;
        let total: i64 = sqlx::query_scalar(
            "select count(a.id) from outbound_proxies p
             left join provider_accounts a on a.outbound_proxy_id = p.id
                 and (strpos(lower(a.name), lower($2)) > 0
                      or strpos(lower(coalesce(a.email, '')), lower($2)) > 0)
             where p.id = $1 group by p.id",
        )
        .bind(&query.proxy_id)
        .bind(&query.search)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| store_error(unavailable()))?
        .ok_or_else(|| {
            store_error(StoreError::NotFound {
                entity: ENTITY,
                id: query.proxy_id.clone(),
            })
        })?;
        let mut items = sqlx::query(
            "select id, name, email, provider_kind, authentication_kind, plan_type, enabled from provider_accounts
             where outbound_proxy_id = $1 and (strpos(lower(name), lower($2)) > 0
                 or strpos(lower(coalesce(email, '')), lower($2)) > 0)
             order by name, id limit $3 offset $4",
        )
        .bind(&query.proxy_id)
        .bind(&query.search)
        .bind(i64::from(query.page_size.get()))
        .bind(i64::from(query.page - 1) * i64::from(query.page_size.get()))
        .fetch_all(&mut *transaction)
        .await
        .map_err(|_| store_error(unavailable()))?
        .into_iter()
        .map(|row| {
            Ok(ProxyAccountRef {
                id: row.try_get("id").map_err(|_| invalid())?,
                name: row.try_get("name").map_err(|_| invalid())?,
                email: row.try_get("email").map_err(|_| invalid())?,
                provider_kind: row.try_get("provider_kind").map_err(|_| invalid())?,
                authentication_kind: row.try_get("authentication_kind").map_err(|_| invalid())?,
                plan_type: row.try_get("plan_type").map_err(|_| invalid())?,
                plan_type_display: None,
                groups: Vec::new(),
                enabled: row.try_get("enabled").map_err(|_| invalid())?,
            })
        })
        .collect::<StoreResult<Vec<_>>>()
        .map_err(store_error)?;
        if !items.is_empty() {
            // 仅批量加载当前页的分组，并沿用同一快照，避免逐账号查询。
            let account_ids: Vec<&str> = items.iter().map(|account| account.id.as_str()).collect();
            let rows = sqlx::query_as::<_, (String, String, String, String, bool)>(
                "select m.provider_account_id, g.id, g.name, g.color, g.enabled
                 from account_group_accounts m join account_groups g on g.id = m.account_group_id
                 where m.provider_account_id = any($1::text[])
                 order by m.provider_account_id, g.id",
            )
            .bind(&account_ids)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|_| store_error(unavailable()))?;
            let mut groups = BTreeMap::<String, Vec<AccountGroupRef>>::new();
            for (account_id, group_id, name, color, enabled) in rows {
                groups.entry(account_id).or_default().push(AccountGroupRef {
                    id: AccountGroupId::new(group_id).map_err(|_| store_error(invalid()))?,
                    name,
                    color: AccountGroupColor::parse(&color)
                        .ok_or_else(|| store_error(invalid()))?,
                    enabled,
                });
            }
            for account in &mut items {
                account.groups = groups.remove(&account.id).unwrap_or_default();
            }
        }
        transaction
            .commit()
            .await
            .map_err(|_| store_error(unavailable()))?;
        Ok(ProxyAccountPage {
            items,
            total: u64::try_from(total).map_err(|_| store_error(invalid()))?,
            page: query.page,
            page_size: query.page_size.get(),
        })
    }

    async fn reserve_import(&self, id: &str) -> AdminStoreResult<ProxyImportReservation> {
        let slot = self
            .import_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| store_error(conflict(id)))?;
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|_| store_error(unavailable()))?
            .detach();
        let acquired: bool =
            sqlx::query_scalar("select pg_try_advisory_lock_shared(hashtextextended($1, 739219))")
                .bind(id)
                .fetch_one(&mut connection)
                .await
                .map_err(|_| store_error(unavailable()))?;
        if !acquired {
            return Err(store_error(conflict(id)));
        }
        let record = match self.get(id).await {
            Ok(record) => record,
            Err(error) => {
                // 拒绝预留时先等待数据库释放锁，避免连接关闭尚未生效就误挡后续代理操作。
                sqlx::query("select pg_advisory_unlock_shared(hashtextextended($1, 739219))")
                    .bind(id)
                    .execute(&mut connection)
                    .await
                    .map_err(|_| store_error(unavailable()))?;
                return Err(error);
            }
        };
        Ok(ProxyImportReservation {
            binding: ImportProxyBinding {
                id: record.id,
                proxy: record.proxy,
            },
            guard: Box::new(PgProxyImportGuard {
                _connection: connection,
                _slot: slot,
            }),
        })
    }

    async fn list(&self, query: ProxyListQuery) -> AdminStoreResult<ProxyPage> {
        if query.page == 0 {
            return Err(store_error(invalid()));
        }
        let total: i64 = sqlx::query_scalar(
            "select count(*) from outbound_proxies where strpos(lower(name), lower($1)) > 0",
        )
        .bind(&query.search)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| store_error(unavailable()))?;
        let mut builder = QueryBuilder::<Postgres>::new(SELECT);
        builder
            .push(" where strpos(lower(p.name), lower(")
            .push_bind(&query.search)
            .push(")) > 0 order by p.created_at desc, p.id limit ")
            .push_bind(i64::from(query.page_size.get()))
            .push(" offset ")
            .push_bind(i64::from(query.page - 1) * i64::from(query.page_size.get()));
        let items = builder
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|_| store_error(unavailable()))?
            .into_iter()
            .map(record)
            .collect::<StoreResult<Vec<_>>>()
            .map_err(store_error)?;
        Ok(ProxyPage {
            items,
            total: u64::try_from(total).map_err(|_| store_error(invalid()))?,
            page: query.page,
            page_size: query.page_size.get(),
        })
    }

    async fn get(&self, id: &str) -> AdminStoreResult<ProxyRecord> {
        let mut builder = QueryBuilder::<Postgres>::new(SELECT);
        builder.push(" where p.id = ").push_bind(id);
        let row = builder
            .build()
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| store_error(unavailable()))?
            .ok_or_else(|| {
                store_error(StoreError::NotFound {
                    entity: ENTITY,
                    id: id.to_owned(),
                })
            })?;
        record(row).map_err(store_error)
    }

    async fn create(
        &self,
        command: NewProxy,
        context: &MutationContext,
    ) -> AdminStoreResult<ProxyMutation> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| store_error(unavailable()))?;
        let revision = bump_config_revision_in_transaction(&mut transaction)
            .await
            .map_err(store_error)?;
        let (id, created) =
            ensure_proxy_for_url(&mut transaction, &command.proxy, Some(&command.name))
                .await
                .map_err(store_error)?;
        if !created {
            return Err(store_error(conflict(&id)));
        }
        save_location(&mut transaction, &id, command.location.as_ref())
            .await
            .map_err(store_error)?;
        audit(
            &mut transaction,
            context,
            "create",
            &id,
            &["name", "proxy_url", "location"],
            revision,
        )
        .await
        .map_err(store_error)?;
        transaction
            .commit()
            .await
            .map_err(|_| store_error(unavailable()))?;
        Ok(ProxyMutation {
            config_revision: admin_revision(revision)?,
            record: self.get(&id).await?,
        })
    }

    async fn update(
        &self,
        command: UpdateProxy,
        context: &MutationContext,
    ) -> AdminStoreResult<ProxyMutation> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| store_error(unavailable()))?;
        exclude_active_imports(&mut transaction, &command.id)
            .await
            .map_err(store_error)?;
        let revision = bump_config_revision_in_transaction(&mut transaction)
            .await
            .map_err(store_error)?;
        if let Some(proxy) = &command.proxy {
            lock_url(&mut transaction, proxy)
                .await
                .map_err(store_error)?;
            let duplicate: bool = sqlx::query_scalar(
                "select exists(select 1 from outbound_proxies where proxy_url = $1 and id <> $2)",
            )
            .bind(proxy.expose_url())
            .bind(&command.id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|_| store_error(unavailable()))?;
            if duplicate {
                return Err(store_error(conflict(&command.id)));
            }
            // URL 变更即清空该代理的 geo/质量子表行（等价于原来 update 把这些列一起置 NULL）。
            // 在父表 update 之前执行，此时 proxy_url 仍是旧值，比对才成立。
            sqlx::query(
                "delete from outbound_proxy_quality q using outbound_proxies p
                 where q.proxy_id = p.id and p.id = $1 and p.proxy_url <> $2",
            )
            .bind(&command.id)
            .bind(proxy.expose_url())
            .execute(&mut *transaction)
            .await
            .map_err(|_| store_error(unavailable()))?;
        }
        let changed = sqlx::query(
            "update outbound_proxies set name = $3, proxy_url = coalesce($4, proxy_url), revision = revision + 1, updated_at = now(),
             last_test_at = case when $4 is not null and $4 <> proxy_url then null else last_test_at end,
             last_test_success = case when $4 is not null and $4 <> proxy_url then null else last_test_success end,
             last_test_latency_ms = case when $4 is not null and $4 <> proxy_url then null else last_test_latency_ms end,
             last_test_ip = case when $4 is not null and $4 <> proxy_url then null else last_test_ip end,
             last_test_ipv4 = case when $4 is not null and $4 <> proxy_url then null else last_test_ipv4 end,
             last_test_ipv6 = case when $4 is not null and $4 <> proxy_url then null else last_test_ipv6 end,
             last_test_message = case when $4 is not null and $4 <> proxy_url then null else last_test_message end
             where id = $1 and revision = $2")
            .bind(&command.id).bind(i64::try_from(command.revision.get()).map_err(|_| store_error(invalid()))?)
            .bind(&command.name).bind(command.proxy.as_ref().map(OutboundProxy::expose_url))
            .execute(&mut *transaction).await.map_err(|_| store_error(unavailable()))?;
        if changed.rows_affected() != 1 {
            return Err(store_error(conflict(&command.id)));
        }
        if let Some(location) = &command.location {
            save_location(&mut transaction, &command.id, location.as_ref())
                .await
                .map_err(store_error)?;
        }
        sqlx::query("update provider_accounts a set outbound_proxy_url = p.proxy_url, updated_at = greatest(now(), a.updated_at) from outbound_proxies p where p.id = $1 and a.outbound_proxy_id = p.id and a.outbound_proxy_url is distinct from p.proxy_url")
            .bind(&command.id).execute(&mut *transaction).await.map_err(|_| store_error(unavailable()))?;
        audit(
            &mut transaction,
            context,
            "update",
            &command.id,
            if command.location.is_some() {
                &["name", "proxy_url", "location"]
            } else {
                &["name", "proxy_url"]
            },
            revision,
        )
        .await
        .map_err(store_error)?;
        transaction
            .commit()
            .await
            .map_err(|_| store_error(unavailable()))?;
        Ok(ProxyMutation {
            config_revision: admin_revision(revision)?,
            record: self.get(&command.id).await?,
        })
    }

    async fn delete(
        &self,
        id: &str,
        revision: AdminRevision,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminRevision> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| store_error(unavailable()))?;
        exclude_active_imports(&mut transaction, id)
            .await
            .map_err(store_error)?;
        let config_revision = bump_config_revision_in_transaction(&mut transaction)
            .await
            .map_err(store_error)?;
        let result = sqlx::query("delete from outbound_proxies where id = $1 and revision = $2")
            .bind(id)
            .bind(i64::try_from(revision.get()).map_err(|_| store_error(invalid()))?)
            .execute(&mut *transaction)
            .await
            .map_err(|error| {
                // PostgreSQL 18 为 RESTRICT 返回不同于普通外键违规的错误码。
                if error.as_database_error().is_some_and(|error| {
                    error.is_foreign_key_violation() || error.code().as_deref() == Some("23001")
                }) {
                    store_error(conflict(id))
                } else {
                    store_error(unavailable())
                }
            })?;
        if result.rows_affected() != 1 {
            return Err(store_error(conflict(id)));
        }
        audit(
            &mut transaction,
            context,
            "delete",
            id,
            &[],
            config_revision,
        )
        .await
        .map_err(store_error)?;
        transaction
            .commit()
            .await
            .map_err(|_| store_error(unavailable()))?;
        admin_revision(config_revision)
    }

    async fn record_test(
        &self,
        id: &str,
        revision: AdminRevision,
        result: ProxyTestResult,
        context: &MutationContext,
    ) -> AdminStoreResult<ProxyRecord> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| store_error(unavailable()))?;
        exclude_active_imports(&mut transaction, id)
            .await
            .map_err(store_error)?;
        let statement = format!(
            "update outbound_proxies set {RECORD_TEST_SET} where id = $1 and revision = $2"
        );
        let query = sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(id)
            .bind(i64::try_from(revision.get()).map_err(|_| store_error(invalid()))?);
        let updated = bind_test_result(query, &result)
            .map_err(store_error)?
            .execute(&mut *transaction)
            .await
            .map_err(|_| store_error(unavailable()))?;
        if updated.rows_affected() != 1 {
            return Err(store_error(conflict(id)));
        }
        // 出口地区写子表（连通性+双栈已写父表）；两组数据在同一事务里一起前进。
        upsert_test_geo(&mut transaction, id, &result)
            .await
            .map_err(store_error)?;
        let current: i64 =
            sqlx::query_scalar("select config_revision from runtime_settings where id = 1")
                .fetch_one(&mut *transaction)
                .await
                .map_err(|_| store_error(unavailable()))?;
        let current = Revision::new(u64::try_from(current).map_err(|_| store_error(invalid()))?)
            .map_err(store_error)?;
        audit(
            &mut transaction,
            context,
            "test",
            id,
            &["last_test"],
            current,
        )
        .await
        .map_err(store_error)?;
        transaction
            .commit()
            .await
            .map_err(|_| store_error(unavailable()))?;
        self.get(id).await
    }

    async fn record_quality(
        &self,
        id: &str,
        revision: AdminRevision,
        report: ProxyQualityReport,
        context: &MutationContext,
    ) -> AdminStoreResult<ProxyRecord> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| store_error(unavailable()))?;
        exclude_active_imports(&mut transaction, id)
            .await
            .map_err(store_error)?;
        let base = report
            .items
            .first()
            .filter(|item| item.target == PROXY_QUALITY_BASE_TARGET)
            .ok_or_else(|| store_error(invalid()))?;
        // 质量检测的第一项就是一次完整的连通性测试，两组列必须在同一事务里一起前进。
        let test = ProxyTestResult {
            success: base.status == ProxyQualityItemStatus::Pass,
            latency_ms: base.latency_ms.unwrap_or_default(),
            exit_ip: report.exit_ip,
            exit_geo: report.exit_geo.clone(),
            // 质量检测是单次探测，不做双栈；双栈列由 record_test 路径写入。
            exit_ipv4: None,
            exit_ipv6: None,
            message: if base.status == ProxyQualityItemStatus::Pass {
                "连接成功".to_owned()
            } else {
                base.message.clone()
            },
        };
        // 连通性写父表（双栈用 coalesce 保留上次结果）；geo + 质量快照/报告写子表（同事务）。
        let statement = format!(
            "update outbound_proxies set {RECORD_QUALITY_TEST_SET} where id = $1 and revision = $2"
        );
        let query = sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(id)
            .bind(i64::try_from(revision.get()).map_err(|_| store_error(invalid()))?);
        let updated = bind_test_result(query, &test)
            .map_err(store_error)?
            .execute(&mut *transaction)
            .await
            .map_err(|_| store_error(unavailable()))?;
        if updated.rows_affected() != 1 {
            return Err(store_error(conflict(id)));
        }
        let (country, code, region, city) = test_geo_columns(&test);
        let document = quality_report_document(&report);
        sqlx::query(
            "insert into outbound_proxy_quality
               (proxy_id, last_test_country, last_test_country_code, last_test_region, last_test_city,
                quality_checked_at, quality_score, quality_grade, quality_status, quality_summary, quality_report)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
             on conflict (proxy_id) do update set
               last_test_country = excluded.last_test_country,
               last_test_country_code = excluded.last_test_country_code,
               last_test_region = excluded.last_test_region,
               last_test_city = excluded.last_test_city,
               quality_checked_at = excluded.quality_checked_at,
               quality_score = excluded.quality_score,
               quality_grade = excluded.quality_grade,
               quality_status = excluded.quality_status,
               quality_summary = excluded.quality_summary,
               quality_report = excluded.quality_report",
        )
        .bind(id)
        .bind(country)
        .bind(code)
        .bind(region)
        .bind(city)
        .bind(report.snapshot.checked_at)
        .bind(i16::from(report.snapshot.score))
        .bind(report.snapshot.grade.to_string())
        .bind(report.snapshot.status.as_str())
        .bind(report.snapshot.summary)
        .bind(sqlx::types::Json(document))
        .execute(&mut *transaction)
        .await
        .map_err(|_| store_error(unavailable()))?;
        let current: i64 =
            sqlx::query_scalar("select config_revision from runtime_settings where id = 1")
                .fetch_one(&mut *transaction)
                .await
                .map_err(|_| store_error(unavailable()))?;
        let current = Revision::new(u64::try_from(current).map_err(|_| store_error(invalid()))?)
            .map_err(store_error)?;
        audit(
            &mut transaction,
            context,
            "quality_check",
            id,
            &["last_test", "quality"],
            current,
        )
        .await
        .map_err(store_error)?;
        transaction
            .commit()
            .await
            .map_err(|_| store_error(unavailable()))?;
        self.get(id).await
    }

    async fn quality_report(&self, id: &str) -> AdminStoreResult<Option<ProxyQualityReport>> {
        // 质量列已迁子表；先确认代理存在（区分 NotFound 与"无质量报告"），再读子表。
        let exists: bool =
            sqlx::query_scalar("select exists(select 1 from outbound_proxies where id = $1)")
                .bind(id)
                .fetch_one(&self.pool)
                .await
                .map_err(|_| store_error(unavailable()))?;
        if !exists {
            return Err(store_error(StoreError::NotFound {
                entity: ENTITY,
                id: id.to_owned(),
            }));
        }
        let Some(row) = sqlx::query(
            "select quality_checked_at, quality_score, quality_grade, quality_status,              quality_summary, quality_report from outbound_proxy_quality where proxy_id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| store_error(unavailable()))?
        else {
            return Ok(None);
        };
        let Some(snapshot) = quality_snapshot_from_row(&row).map_err(store_error)? else {
            return Ok(None);
        };
        let document: sqlx::types::Json<serde_json::Value> = row
            .try_get::<Option<_>, _>("quality_report")
            .map_err(|_| store_error(invalid()))?
            .ok_or_else(|| store_error(invalid()))?;
        quality_report_from_document(snapshot, &document.0)
            .map(Some)
            .map_err(store_error)
    }
}
