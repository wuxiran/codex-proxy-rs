//! 请求构造、探针和连接池共享的预热发布凭证；回池不等于通过验收。

use sha2::{Digest, Sha256};
use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::Instant;

const PENDING: u8 = 0;
const VERIFIED: u8 = 1;
const UNCHECKED: u8 = 2;
const REJECTED: u8 = 3;

#[derive(Debug)]
struct Approval {
    model: String,
    max_age_ms: AtomicU64,
    status: AtomicU8,
    business_egress: Option<String>,
    verification: OnceLock<Verification>,
}

#[derive(Debug)]
struct Verification {
    at: Instant,
    at_ms: u64,
    valid_for: Duration,
    ticket: Option<[u8; 32]>,
    ticket_expires_at: Option<Instant>,
    policy: Option<[u8; 32]>,
}

/// 只在本地比较指纹，不保存或输出票据原文。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WarmVerificationContext {
    ticket: Option<[u8; 32]>,
    policy: Option<[u8; 32]>,
    continuing: bool,
}

impl WarmVerificationContext {
    pub(crate) fn from_fingerprint(ticket: Option<[u8; 32]>, policy: [u8; 32]) -> Self {
        Self {
            ticket,
            policy: Some(policy),
            continuing: false,
        }
    }
    pub(crate) fn new(ticket: Option<&str>, policy: Option<[u8; 32]>) -> Self {
        Self {
            ticket: ticket.map(ticket_fingerprint),
            policy,
            continuing: false,
        }
    }

    pub(crate) fn with_continuation(mut self, continuing: bool) -> Self {
        self.continuing = continuing;
        self
    }

    pub(crate) fn is_new_chain(&self) -> bool {
        !self.continuing
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WarmVerificationStatus {
    Fresh,
    Expired,
    ConditionsChanged,
    Unchecked,
    Pending,
    Rejected,
}

impl WarmVerificationStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Expired => "expired",
            Self::ConditionsChanged => "conditions_changed",
            Self::Unchecked => "unchecked",
            Self::Pending => "pending",
            Self::Rejected => "rejected",
        }
    }
}

pub(crate) struct WarmVerificationSnapshot {
    pub(crate) status: WarmVerificationStatus,
    pub(crate) at_ms: Option<u64>,
    pub(crate) age_ms: Option<u64>,
}

fn ticket_fingerprint(ticket: &str) -> [u8; 32] {
    Sha256::digest(ticket.as_bytes()).into()
}

/// 后台探针持有的候选发布句柄，不会序列化到上游。
/// 只有读完当前探针并发布后，业务才可领养对应连接。
#[derive(Debug, Clone)]
pub struct WarmConnectionApproval(Arc<Approval>);

impl PartialEq for WarmConnectionApproval {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for WarmConnectionApproval {}

impl WarmConnectionApproval {
    pub(crate) fn model(&self) -> &str {
        &self.0.model
    }

    pub(crate) fn verification_expires_at_ms(&self) -> Option<u64> {
        self.0.verification.get().map(|proof| {
            proof
                .at_ms
                .saturating_add(u64::try_from(proof.valid_for.as_millis()).unwrap_or(u64::MAX))
        })
    }
    pub fn new(model: String, max_age: Duration) -> Self {
        Self::scoped(model, max_age, None)
    }

    pub(crate) fn scoped(
        model: String,
        max_age: Duration,
        business_egress: Option<String>,
    ) -> Self {
        Self(Arc::new(Approval {
            model,
            max_age_ms: AtomicU64::new(u64::try_from(max_age.as_millis()).unwrap_or(u64::MAX)),
            status: AtomicU8::new(PENDING),
            business_egress,
            verification: OnceLock::new(),
        }))
    }

    pub(crate) fn accepts_business_egress(&self, egress: &str) -> bool {
        self.0.business_egress.as_deref() == Some(egress)
    }

    /// 发布完整验证的结果。禁用答案检查时必须传 false，观测不能显示为已验质量。
    pub fn publish(&self, checked: bool) {
        self.publish_scoped(checked, self.max_age(), None, None);
    }

    /// 发布探针完成时冻结的证明；复探期限与票据剩余寿命由候选发布者取较短值。
    pub fn publish_scoped(
        &self,
        checked: bool,
        valid_for: Duration,
        ticket: Option<&str>,
        policy: Option<[u8; 32]>,
    ) {
        self.publish_rechecked(checked, valid_for, ticket, None, policy, None);
    }

    pub(crate) fn publish_rechecked(
        &self,
        checked: bool,
        mut valid_for: Duration,
        ticket: Option<&str>,
        ticket_valid_for: Option<Duration>,
        policy: Option<[u8; 32]>,
        previous: Option<&Self>,
    ) {
        if checked {
            let now = Instant::now();
            let previous = previous
                .filter(|previous| {
                    previous.was_checked() && previous.0.model.eq_ignore_ascii_case(&self.0.model)
                })
                .and_then(|previous| previous.0.verification.get())
                .filter(|proof| proof.policy == policy);
            let fingerprint = ticket
                .map(ticket_fingerprint)
                .or_else(|| previous.and_then(|proof| proof.ticket));
            let ticket_expires_at = if ticket.is_some() {
                now.checked_add(ticket_valid_for.unwrap_or(valid_for))
            } else {
                previous.and_then(|proof| proof.ticket_expires_at)
            };
            // 复探可以更新证明，但没有新票时必须保留原票的到期上限。
            if fingerprint.is_some() {
                let remaining = ticket_expires_at.map_or(Duration::ZERO, |deadline| {
                    deadline.saturating_duration_since(now)
                });
                valid_for = valid_for.min(remaining);
            }
            if valid_for.is_zero() {
                self.reject();
                return;
            }
            let verification = Verification {
                at: now,
                at_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |duration| {
                        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
                    }),
                valid_for: valid_for.min(self.max_age()),
                ticket: fingerprint,
                ticket_expires_at,
                policy,
            };
            if self.0.verification.set(verification).is_err() {
                return;
            }
        }
        let _ = self.0.status.compare_exchange(
            PENDING,
            if checked { VERIFIED } else { UNCHECKED },
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub fn reject(&self) {
        self.0.status.store(REJECTED, Ordering::Release);
    }

    pub(crate) fn permits(&self, model: &str, age: Duration) -> bool {
        matches!(self.0.status.load(Ordering::Acquire), VERIFIED | UNCHECKED)
            && age < self.max_age()
            && self.0.model.eq_ignore_ascii_case(model)
            && (!self.was_checked() || self.checked())
    }

    pub(crate) fn permits_request(
        &self,
        model: &str,
        age: Duration,
        context: &WarmVerificationContext,
    ) -> bool {
        self.permits(model, age)
            && (!self.was_checked()
                || self.snapshot(model, age, context).status == WarmVerificationStatus::Fresh)
    }

    pub(crate) fn was_checked(&self) -> bool {
        self.0.status.load(Ordering::Acquire) == VERIFIED
    }

    pub(crate) fn snapshot(
        &self,
        model: &str,
        age: Duration,
        context: &WarmVerificationContext,
    ) -> WarmVerificationSnapshot {
        let proof = self.0.verification.get();
        let status = match self.0.status.load(Ordering::Acquire) {
            VERIFIED if !self.checked() || age >= self.max_age() => WarmVerificationStatus::Expired,
            VERIFIED => {
                // 不带票与探针本身的请求条件相符；附带票时必须是该探针对应的票。
                let matches = proof.is_some_and(|proof| {
                    proof.policy == context.policy
                        && (context.ticket.is_none() || proof.ticket == context.ticket)
                        && self.0.model.eq_ignore_ascii_case(model)
                });
                if matches {
                    WarmVerificationStatus::Fresh
                } else {
                    WarmVerificationStatus::ConditionsChanged
                }
            }
            UNCHECKED => WarmVerificationStatus::Unchecked,
            REJECTED => WarmVerificationStatus::Rejected,
            _ => WarmVerificationStatus::Pending,
        };
        WarmVerificationSnapshot {
            status,
            at_ms: proof.map(|proof| proof.at_ms),
            age_ms: proof
                .map(|proof| u64::try_from(proof.at.elapsed().as_millis()).unwrap_or(u64::MAX)),
        }
    }

    /// 提前一个小窗口复探，避免仍在暖池中的连接只能等证明过期才补齐。
    pub(crate) fn needs_reprobe(&self) -> bool {
        self.was_checked()
            && self.0.verification.get().is_none_or(|proof| {
                let lead = (proof.valid_for / 5).min(Duration::from_secs(20));
                proof.at.elapsed() >= proof.valid_for.saturating_sub(lead)
            })
    }

    pub(crate) fn policy_key(
        settings: &turn_state::WarmPoolSettings,
        ticket_ttl: Duration,
        generation: Option<&str>,
    ) -> [u8; 32] {
        let mut digest = Sha256::new();
        for value in [
            &settings.probe_prompt,
            &settings.probe_expect,
            &settings.probe_effort,
        ] {
            digest.update(value.len().to_be_bytes());
            digest.update(value.as_bytes());
        }
        digest.update([u8::from(settings.probe)]);
        digest.update(settings.reprobe_seconds.to_be_bytes());
        digest.update(ticket_ttl.as_secs().to_be_bytes());
        digest.update([u8::from(generation.is_some())]);
        if let Some(generation) = generation {
            digest.update(generation.as_bytes());
        }
        digest.finalize().into()
    }

    pub(crate) fn published(&self) -> bool {
        matches!(self.0.status.load(Ordering::Acquire), VERIFIED | UNCHECKED)
    }

    pub(crate) fn max_age(&self) -> Duration {
        Duration::from_millis(self.0.max_age_ms.load(Ordering::Acquire))
    }

    pub(crate) fn restrict_lifetime(&self, max_age: Duration) {
        self.0.max_age_ms.fetch_min(
            u64::try_from(max_age.as_millis()).unwrap_or(u64::MAX),
            Ordering::AcqRel,
        );
    }

    pub(crate) fn checked(&self) -> bool {
        self.was_checked()
            && self
                .0
                .verification
                .get()
                .is_some_and(|proof| proof.at.elapsed() < proof.valid_for)
    }

    pub(crate) fn rejected(&self) -> bool {
        self.0.status.load(Ordering::Acquire) == REJECTED
    }
}

#[cfg(test)]
mod verification_tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn verification_expires_at_240_seconds_despite_a_3000_second_connection() {
        let approval = WarmConnectionApproval::new("model".into(), Duration::from_secs(3000));
        approval.publish_scoped(true, Duration::from_secs(240), Some("synthetic-a"), None);
        let context = WarmVerificationContext::new(Some("synthetic-a"), None);
        tokio::time::advance(Duration::from_secs(239)).await;
        assert!(approval.permits_request("model", Duration::from_secs(239), &context));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(!approval.checked());
        assert!(!approval.permits_request("model", Duration::from_secs(240), &context));
        assert_eq!(
            approval
                .snapshot("model", Duration::from_secs(240), &context)
                .status,
            WarmVerificationStatus::Expired
        );
    }

    #[test]
    fn changing_ticket_model_or_probe_policy_cannot_reuse_the_previous_verification() {
        let approval = WarmConnectionApproval::new("model".into(), Duration::from_secs(3000));
        approval.publish_scoped(
            true,
            Duration::from_secs(240),
            Some("synthetic-a"),
            Some([1; 32]),
        );
        for (model, ticket, policy) in [
            ("model", "synthetic-b", [1; 32]),
            ("other-model", "synthetic-a", [1; 32]),
            ("model", "synthetic-a", [2; 32]),
        ] {
            let context = WarmVerificationContext::new(Some(ticket), Some(policy));
            assert!(!approval.permits_request(model, Duration::ZERO, &context));
            assert_eq!(
                approval.snapshot(model, Duration::ZERO, &context).status,
                WarmVerificationStatus::ConditionsChanged
            );
        }
        assert!(approval.permits_request(
            "model",
            Duration::ZERO,
            &WarmVerificationContext::new(Some("synthetic-a"), Some([1; 32]))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn reprobe_without_a_new_ticket_does_not_extend_the_previous_ticket_proof() {
        let first = WarmConnectionApproval::new("model".into(), Duration::from_secs(3000));
        first.publish_scoped(true, Duration::from_secs(240), Some("synthetic-a"), None);
        tokio::time::advance(Duration::from_secs(200)).await;
        let second = WarmConnectionApproval::new("model".into(), Duration::from_secs(3000));
        second.publish_rechecked(
            true,
            Duration::from_secs(240),
            None,
            None,
            None,
            Some(&first),
        );
        let context = WarmVerificationContext::new(Some("synthetic-a"), None);
        assert!(second.permits_request("model", Duration::from_secs(200), &context));
        tokio::time::advance(Duration::from_secs(40)).await;
        assert!(!second.checked());
    }

    #[tokio::test(start_paused = true)]
    async fn warm_pool_can_schedule_reprobe_before_verification_expires() {
        let approval = WarmConnectionApproval::new("model".into(), Duration::from_secs(3000));
        approval.publish_scoped(true, Duration::from_secs(240), None, None);
        tokio::time::advance(Duration::from_secs(219)).await;
        assert!(!approval.needs_reprobe());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(approval.needs_reprobe());
        assert!(approval.checked());
    }

    #[tokio::test(start_paused = true)]
    async fn reprobe_can_refresh_a_short_proof_while_the_same_ticket_is_still_live() {
        let first = WarmConnectionApproval::new("model".into(), Duration::from_secs(3000));
        first.publish_rechecked(
            true,
            Duration::from_secs(15),
            Some("synthetic-a"),
            Some(Duration::from_secs(240)),
            None,
            None,
        );
        tokio::time::advance(Duration::from_secs(16)).await;
        assert!(!first.checked());
        let second = WarmConnectionApproval::new("model".into(), Duration::from_secs(3000));
        second.publish_rechecked(
            true,
            Duration::from_secs(15),
            None,
            None,
            None,
            Some(&first),
        );
        assert!(second.checked());
        assert!(second.permits_request(
            "model",
            Duration::from_secs(16),
            &WarmVerificationContext::new(Some("synthetic-a"), None)
        ));
        tokio::time::advance(Duration::from_secs(15)).await;
        assert!(!second.checked());
    }
}
