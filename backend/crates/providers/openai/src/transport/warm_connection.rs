//! 请求构造、探针和连接池共享的预热发布凭证；回池不等于通过验收。

use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::Duration,
};

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
        }))
    }

    pub(crate) fn accepts_business_egress(&self, egress: &str) -> bool {
        self.0.business_egress.as_deref() == Some(egress)
    }

    /// 发布完整验证的结果。禁用答案检查时必须传 false，观测不能显示为已验质量。
    pub fn publish(&self, checked: bool) {
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
        self.0.status.load(Ordering::Acquire) == VERIFIED
    }

    pub(crate) fn rejected(&self) -> bool {
        self.0.status.load(Ordering::Acquire) == REJECTED
    }
}
