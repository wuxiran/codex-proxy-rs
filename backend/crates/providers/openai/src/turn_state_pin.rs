//! 账号自有 turn-state 的有界实验缓存；候选长度只用于实验筛选，不代表模型质量。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

const MAX_PINS: usize = 2048;
pub(crate) const MAX_PIN_AGE: Duration = Duration::from_secs(3600);

/// 捕获规则由套餐事实选择，并原样提供给管理端，避免页面另行猜测长度。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CaptureRule {
    default_length: Option<usize>,
    model_lengths: BTreeMap<&'static str, usize>,
}

impl CaptureRule {
    pub(crate) fn for_plan(plan: Option<&str>) -> Self {
        if matches!(
            plan,
            Some(
                "team"
                    | "business"
                    | "self_serve_business_prolite"
                    | "self_serve_business_usage_based"
            )
        ) {
            Self {
                default_length: None,
                model_lengths: BTreeMap::from([
                    ("gpt-5.5", 332),
                    ("gpt-5.6-sol", 332),
                    ("gpt-5.6-terra", 356),
                    ("gpt-6-astra", 332),
                ]),
            }
        } else {
            // Pro 及其它套餐保留既有 292 规则；Team 未列出的模型不自动套用。
            Self {
                default_length: Some(292),
                model_lengths: BTreeMap::new(),
            }
        }
    }

    pub(crate) fn expected_length(&self, model: &str) -> Option<usize> {
        self.model_lengths
            .get(model)
            .copied()
            .or(self.default_length)
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Scope {
    account: String,
    binding: String,
    model: String,
    /// `None` 是管理员遍历代理后钉住的账号级 state，对该账号该模型的全部客户端生效。
    client: Option<String>,
    expected_length: usize,
}

struct PinnedState {
    value: String,
    captured_at: SystemTime,
    hits: u64,
}

impl PinnedState {
    fn active(&self, now: SystemTime) -> bool {
        now.duration_since(self.captured_at)
            .is_ok_and(|age| age < MAX_PIN_AGE)
    }
}

/// 不实现 Debug，防止不透明上游值被日志意外展开。
#[derive(Clone, Default)]
pub(crate) struct TurnStatePins(Arc<Mutex<BTreeMap<Scope, PinnedState>>>);

pub(crate) struct PinStatus {
    pub(crate) model: String,
    pub(crate) length: usize,
    pub(crate) captured_at: SystemTime,
    pub(crate) hits: u64,
    pub(crate) account_wide: bool,
}

/// 账号级钉住被拒的原因；不携带 state 值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PinRejected {
    Length,
    Expired,
    Full,
}

impl TurnStatePins {
    pub(crate) fn attempt(
        &self,
        account: &str,
        binding: String,
        model: &str,
        client: &str,
        expected_length: usize,
        now: SystemTime,
    ) -> PinAttempt {
        let scope = Scope {
            account: account.to_owned(),
            binding,
            model: model.to_owned(),
            client: Some(client.to_owned()),
            expected_length,
        };
        let value = self.0.lock().ok().and_then(|mut pins| {
            pins.retain(|_, pin| pin.active(now));
            // 客户端自己的 state 优先；没有时回退到账号级 state。
            let account_wide = Scope {
                client: None,
                ..scope.clone()
            };
            let key = if pins.contains_key(&scope) {
                &scope
            } else {
                &account_wide
            };
            let pin = pins.get_mut(key)?;
            pin.hits = pin.hits.saturating_add(1);
            Some(pin.value.clone())
        });
        PinAttempt {
            pins: self.clone(),
            scope,
            value,
            candidate: None,
            started_at: now,
        }
    }

    pub(crate) fn status(&self, account: &str, binding: &str, now: SystemTime) -> Vec<PinStatus> {
        self.0.lock().map_or_else(
            |_| Vec::new(),
            |pins| {
                pins.iter()
                    .filter(|(scope, pin)| {
                        scope.account == account && scope.binding == binding && pin.active(now)
                    })
                    .map(|(scope, pin)| PinStatus {
                        model: scope.model.clone(),
                        length: pin.value.len(),
                        captured_at: pin.captured_at,
                        hits: pin.hits,
                        account_wide: scope.client.is_none(),
                    })
                    .collect()
            },
        )
    }

    /// 管理员显式钉住一个刚在目标出口上观测到的 state。它替换该账号该模型下的全部旧
    /// state（旧值来自换绑前的出口，是否仍有效未知）；被动捕获仍然永不覆盖。
    // 作用域的每个分量都是独立事实，打包成结构体只会多一层无意义的搬运。
    #[expect(clippy::too_many_arguments)]
    pub(crate) fn pin_account_wide(
        &self,
        account: &str,
        binding: String,
        model: &str,
        expected_length: usize,
        value: &str,
        captured_at: SystemTime,
        now: SystemTime,
    ) -> Result<(), PinRejected> {
        if value.len() != expected_length || !value.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(PinRejected::Length);
        }
        if !now
            .duration_since(captured_at)
            .is_ok_and(|age| age < MAX_PIN_AGE)
        {
            return Err(PinRejected::Expired);
        }
        let mut pins = self.0.lock().map_err(|_| PinRejected::Full)?;
        pins.retain(|scope, pin| {
            pin.active(now)
                && !(scope.account == account && scope.binding == binding && scope.model == model)
        });
        if pins.len() >= MAX_PINS {
            return Err(PinRejected::Full);
        }
        pins.insert(
            Scope {
                account: account.to_owned(),
                binding,
                model: model.to_owned(),
                client: None,
                expected_length,
            },
            PinnedState {
                value: value.to_owned(),
                captured_at,
                hits: 0,
            },
        );
        Ok(())
    }

    pub(crate) fn clear(&self, account: &str) {
        if let Ok(mut pins) = self.0.lock() {
            pins.retain(|scope, _| scope.account != account);
        }
    }
}

pub(crate) struct PinAttempt {
    pins: TurnStatePins,
    scope: Scope,
    value: Option<String>,
    candidate: Option<String>,
    started_at: SystemTime,
}

impl PinAttempt {
    pub(crate) fn value(&self) -> Option<&str> {
        self.value.as_deref()
    }

    pub(crate) fn observe(&mut self, value: Option<&str>) {
        // 只接收本次真实上游响应；客户端传入的 state 不具备捕获资格。
        if self.candidate.is_none()
            && let Some(value) = value.filter(|v| {
                v.len() == self.scope.expected_length && v.bytes().all(|b| b.is_ascii_graphic())
            })
        {
            self.candidate = Some(value.to_owned());
        }
    }

    pub(crate) fn completed(&mut self, now: SystemTime) {
        // 本次请求已经复用了一个 state（含账号级回退）时不再另立客户端级 state。
        if self.value.is_some() {
            return;
        }
        let Some(value) = self.candidate.take() else {
            return;
        };
        // 完成时已经过期的长请求不能重新赋予候选一小时寿命。
        if !now
            .duration_since(self.started_at)
            .is_ok_and(|age| age < MAX_PIN_AGE)
        {
            return;
        }
        if let Ok(mut pins) = self.pins.0.lock() {
            pins.retain(|_, pin| pin.active(now));
            if pins.len() < MAX_PINS {
                // 并发请求中的首个成功候选获胜，随后任何长度都不覆盖它。
                pins.entry(self.scope.clone()).or_insert(PinnedState {
                    value,
                    captured_at: self.started_at,
                    hits: 0,
                });
            }
        }
    }
}

/// 凭据刷新或管理员重新捕获后，旧 state 不再具备复用资格；Cookie 更新不改变绑定。
pub(crate) fn credential_binding(generation: &str, access_token: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let mut digest = Sha256::new();
    digest.update(generation.as_bytes());
    digest.update([0]);
    digest.update(access_token.as_bytes());
    hex::encode(digest.finalize())
}
