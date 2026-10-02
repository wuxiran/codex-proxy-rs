//! 可复用的账号出口配置，以及脱敏后的连通性测试结果。

use chrono::{DateTime, Utc};
use gateway_core::account::OutboundProxy;

use super::{PageSize, Revision, account_groups::AccountGroupRef};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountProxySelection {
    Direct,
    Url(OutboundProxy),
    Saved(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportProxyBinding {
    pub id: String,
    pub proxy: OutboundProxy,
}

#[derive(Debug, Clone)]
pub struct ProxyListQuery {
    pub page: u32,
    pub page_size: PageSize,
    pub search: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyAccountRef {
    pub id: String,
    pub name: String,
    pub email: Option<String>,
    pub provider_kind: String,
    pub authentication_kind: String,
    pub plan_type: Option<String>,
    pub plan_type_display: Option<String>,
    pub groups: Vec<AccountGroupRef>,
    pub enabled: bool,
}

/// 按代理查询关联账号，分页与搜索均在存储层执行。
#[derive(Debug, Clone)]
pub struct ProxyAccountListQuery {
    pub proxy_id: String,
    pub page: u32,
    pub page_size: PageSize,
    pub search: String,
}

#[derive(Debug, Clone)]
pub struct ProxyAccountPage {
    pub items: Vec<ProxyAccountRef>,
    pub total: u64,
    pub page: u32,
    pub page_size: u16,
}

/// 出口 IP 的地理归属；由第三方服务按 IP 推断，仅用于展示，不参与路由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyExitGeo {
    pub country: String,
    pub country_code: String,
    pub region: Option<String>,
    pub city: Option<String>,
    /// 出口所在地的 IANA 时区（如 America/New_York）。测试时从地理服务解析，
    /// 用于按出口自动回填代理请求位置的时区；DB 子表未持久化此值，读回为 None。
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyTestResult {
    pub success: bool,
    pub latency_ms: u64,
    pub exit_ip: Option<std::net::IpAddr>,
    /// 地区查询失败不影响连通性结论，此时为空。
    pub exit_geo: Option<ProxyExitGeo>,
    pub exit_ipv4: Option<std::net::Ipv4Addr>,
    pub exit_ipv6: Option<std::net::Ipv6Addr>,
    pub message: String,
}

pub const PROXY_QUALITY_BASE_TARGET: &str = "base_connectivity";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyQualityItemStatus {
    Pass,
    Warn,
    Fail,
    Challenge,
}

impl ProxyQualityItemStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Fail => "fail",
            Self::Challenge => "challenge",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Pass, Self::Warn, Self::Fail, Self::Challenge]
            .into_iter()
            .find(|status| status.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyQualityStatus {
    Healthy,
    Warn,
    Challenge,
    Failed,
}

impl ProxyQualityStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Warn => "warn",
            Self::Challenge => "challenge",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Healthy, Self::Warn, Self::Challenge, Self::Failed]
            .into_iter()
            .find(|status| status.as_str() == value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyQualityItem {
    pub target: String,
    pub status: ProxyQualityItemStatus,
    pub http_status: Option<u16>,
    pub latency_ms: Option<u64>,
    pub message: String,
    pub cf_ray: Option<String>,
}

/// 探测端口的原始产出：基础连通性结果，以及连通时各上游目标的逐项结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyQualityProbe {
    pub base: ProxyTestResult,
    pub items: Vec<ProxyQualityItem>,
}

/// 列表只携带结论，完整逐项明细按需读取。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyQualitySnapshot {
    pub score: u8,
    pub grade: char,
    pub status: ProxyQualityStatus,
    pub summary: String,
    pub checked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyQualityReport {
    pub snapshot: ProxyQualitySnapshot,
    pub exit_ip: Option<std::net::IpAddr>,
    pub exit_geo: Option<ProxyExitGeo>,
    pub base_latency_ms: Option<u64>,
    pub passed_count: u32,
    pub warn_count: u32,
    pub failed_count: u32,
    pub challenge_count: u32,
    pub items: Vec<ProxyQualityItem>,
}

impl ProxyQualityReport {
    /// 评分只依赖逐项结论：告警 −10、失败 −22、挑战 −30，下限 0。
    /// 基础连通性作为第一项参与计数；它失败时不会再有上游目标项。
    #[must_use]
    pub fn finalize(probe: ProxyQualityProbe, checked_at: DateTime<Utc>) -> Self {
        let ProxyQualityProbe { base, items } = probe;
        let mut all = Vec::with_capacity(items.len() + 1);
        all.push(ProxyQualityItem {
            target: PROXY_QUALITY_BASE_TARGET.to_owned(),
            status: if base.success {
                ProxyQualityItemStatus::Pass
            } else {
                ProxyQualityItemStatus::Fail
            },
            http_status: None,
            latency_ms: Some(base.latency_ms),
            message: if base.success {
                "代理出口连通正常".to_owned()
            } else {
                base.message.clone()
            },
            cf_ray: None,
        });
        if base.success {
            all.extend(items);
        }
        let count = |status| {
            u32::try_from(all.iter().filter(|item| item.status == status).count())
                .unwrap_or(u32::MAX)
        };
        let passed_count = count(ProxyQualityItemStatus::Pass);
        let warn_count = count(ProxyQualityItemStatus::Warn);
        let failed_count = count(ProxyQualityItemStatus::Fail);
        let challenge_count = count(ProxyQualityItemStatus::Challenge);
        let penalty = warn_count
            .saturating_mul(10)
            .saturating_add(failed_count.saturating_mul(22))
            .saturating_add(challenge_count.saturating_mul(30));
        let score = u8::try_from(100_u32.saturating_sub(penalty)).unwrap_or(0);
        let status = if challenge_count > 0 {
            ProxyQualityStatus::Challenge
        } else if failed_count > 0 || passed_count == 0 {
            ProxyQualityStatus::Failed
        } else if warn_count > 0 {
            ProxyQualityStatus::Warn
        } else {
            ProxyQualityStatus::Healthy
        };
        Self {
            snapshot: ProxyQualitySnapshot {
                score,
                grade: quality_grade(score),
                status,
                summary: format!(
                    "通过 {passed_count} 项，告警 {warn_count} 项，失败 {failed_count} 项，挑战 {challenge_count} 项"
                ),
                checked_at,
            },
            exit_ip: base.exit_ip,
            exit_geo: base.exit_geo,
            base_latency_ms: base.success.then_some(base.latency_ms),
            passed_count,
            warn_count,
            failed_count,
            challenge_count,
            items: all,
        }
    }
}

#[must_use]
pub const fn quality_grade(score: u8) -> char {
    match score {
        90.. => 'A',
        75..=89 => 'B',
        60..=74 => 'C',
        40..=59 => 'D',
        _ => 'F',
    }
}

#[derive(Debug, Clone)]
pub struct ProxyRecord {
    pub location: Option<gateway_core::account::RequestLocation>,
    pub id: String,
    pub name: String,
    pub proxy: OutboundProxy,
    pub revision: Revision,
    pub account_count: u64,
    pub last_test_at: Option<DateTime<Utc>>,
    pub last_test: Option<ProxyTestResult>,
    pub quality: Option<ProxyQualitySnapshot>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ProxyPage {
    pub items: Vec<ProxyRecord>,
    pub total: u64,
    pub page: u32,
    pub page_size: u16,
}

#[derive(Debug, Clone)]
pub struct NewProxy {
    pub location: Option<gateway_core::account::RequestLocation>,
    pub name: String,
    pub proxy: OutboundProxy,
}

#[derive(Debug, Clone)]
pub struct UpdateProxy {
    /// 外层为空保留配置，内层为空恢复全局继承。
    pub location: Option<Option<gateway_core::account::RequestLocation>>,
    pub id: String,
    pub revision: Revision,
    pub name: String,
    pub proxy: Option<OutboundProxy>,
}

#[derive(Debug, Clone)]
pub struct ProxyMutation {
    pub config_revision: Revision,
    pub record: ProxyRecord,
}

/// 质量检测同时刷新连通性结果，返回完整报告与更新后的记录。
#[derive(Debug, Clone)]
pub struct ProxyQualityOutcome {
    pub record: ProxyRecord,
    pub report: ProxyQualityReport,
}

pub const MAX_PROXY_BATCH_ITEMS: usize = 200;

/// 跳过项只保留脱敏端点，凭据不随结果回传。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyBatchSkip {
    pub reference: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct ProxyBatchCreate {
    /// 本批没有新建任何代理时配置修订号不变。
    pub config_revision: Option<Revision>,
    pub created: Vec<ProxyRecord>,
    pub skipped: Vec<ProxyBatchSkip>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyBatchDeleteItem {
    pub id: String,
    pub revision: Revision,
}

#[derive(Debug, Clone)]
pub struct ProxyBatchDelete {
    pub config_revision: Option<Revision>,
    pub deleted_ids: Vec<String>,
    pub skipped: Vec<ProxyBatchSkip>,
}
