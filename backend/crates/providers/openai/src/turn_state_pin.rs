//! 账号自有 turn-state 的有界实验缓存；候选长度只用于实验筛选，不代表模型质量。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

const MAX_PINS: usize = 2048;
pub(crate) const MAX_PIN_AGE: Duration = Duration::from_secs(3600);

/// turn-state 票据的最小可信长度。上游（OA）多次调整过票据长度（探针已失效，
/// 且各模型现已统一到 ~780），因此不再按写死的逐模型长度精确匹配，只要求票据
/// 达到一个合理下限且为 ASCII 可见字符即视为有效，避免把合法票据误判成非法而钉不上。
pub(crate) const MIN_TURN_STATE_LEN: usize = 200;

/// 作用域键里保留的 length 分量已不再作长度门；统一取此常量，使同一账号/模型/出口下
/// 不同长度的票据落在同一作用域，写入与读取一致即可命中。
const TURN_STATE_SCOPE_LEN: usize = 0;

/// 捕获规则由套餐事实选择，并原样提供给管理端，避免页面另行猜测长度。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CaptureRule {
    default_length: Option<usize>,
    model_lengths: BTreeMap<&'static str, usize>,
}

impl CaptureRule {
    pub(crate) fn for_plan(_plan: Option<&str>) -> Self {
        // 长度门已废弃（见 MIN_TURN_STATE_LEN）：不再按套餐/模型区分票据长度，
        // 所有套餐所有模型统一用同一作用域长度常量，靠下限+ASCII 校验判合法。
        Self {
            default_length: Some(TURN_STATE_SCOPE_LEN),
            model_lengths: BTreeMap::new(),
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
    /// 账号级 state 是在哪个出口上观测到的；只对同一出口发出的请求生效。
    /// 客户端级 state 由真实流量被动捕获，不区分出口，恒为 `None`。
    egress: Option<String>,
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

/// 出口的稳定指纹：代理地址含凭据，不在更多地方保留原文。
pub(crate) fn egress_fingerprint(proxy_url: Option<&str>) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(proxy_url.unwrap_or("direct").as_bytes()))
}

impl TurnStatePins {
    // 作用域的每个分量都是独立事实，打包成结构体只会多一层无意义的搬运。
    #[expect(clippy::too_many_arguments)]
    pub(crate) fn attempt(
        &self,
        account: &str,
        binding: String,
        model: &str,
        client: &str,
        expected_length: usize,
        egress: &str,
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
            // 账号级 state 只属于探测到它的那个出口：账号之后被改绑到别处（包括钉住
            // 前后那一瞬间的并发改绑）就不再使用，等续期在新出口上重新找。
            if pin.egress.as_deref().is_some_and(|probed| probed != egress) {
                return None;
            }
            pin.hits = pin.hits.saturating_add(1);
            Some(pin.value.clone())
        });
        PinAttempt {
            pins: self.clone(),
            scope,
            egress: egress.to_owned(),
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
        egress: String,
        value: &str,
        captured_at: SystemTime,
        now: SystemTime,
    ) -> Result<(), PinRejected> {
        let _ = expected_length; // 长度门已废弃：只作下限+ASCII 校验，不再精确匹配长度。
        if value.len() < MIN_TURN_STATE_LEN || !value.bytes().all(|b| b.is_ascii_graphic()) {
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
                egress: Some(egress),
            },
        );
        Ok(())
    }

    /// 账号级 state 的捕获时刻；自动续期据此判断是否临近到期。
    ///
    /// 长度规则属于作用域：套餐变化让规则变了之后，旧 state 已经不会被任何请求命中，
    /// 这里同样视为缺失，续期才会去找符合新规则的 state。
    pub(crate) fn account_wide_captured_at(
        &self,
        account: &str,
        binding: &str,
        model: &str,
        expected_length: usize,
        egress: &str,
        now: SystemTime,
    ) -> Option<SystemTime> {
        self.0.lock().ok()?.iter().find_map(|(scope, pin)| {
            (scope.client.is_none()
                && pin.egress.as_deref() == Some(egress)
                && scope.account == account
                && scope.binding == binding
                && scope.model == model
                && scope.expected_length == expected_length
                && pin.active(now))
            .then_some(pin.captured_at)
        })
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
    /// 本次请求实际走的出口。
    egress: String,
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
                v.len() >= MIN_TURN_STATE_LEN && v.bytes().all(|b| b.is_ascii_graphic())
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
            // 本请求在途期间管理员可能已经钉了账号级 state。此时再写客户端级 state，
            // 查找会优先命中它，等于让换绑前出口的旧值盖过刚钉的新值。
            // 只有同一出口上的账号级 state 才需要这样保护：账号已被改绑到别的出口时，
            // 旧出口的账号级 state 对本请求本来就不生效，不能让它挡住新出口上的被动捕获。
            let account_wide = Scope {
                client: None,
                ..self.scope.clone()
            };
            if pins
                .get(&account_wide)
                .is_some_and(|pin| pin.egress.as_deref() == Some(self.egress.as_str()))
            {
                return;
            }
            if pins.len() < MAX_PINS {
                // 并发请求中的首个成功候选获胜，随后任何长度都不覆盖它。
                pins.entry(self.scope.clone()).or_insert(PinnedState {
                    value,
                    captured_at: self.started_at,
                    hits: 0,
                    egress: None,
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

#[cfg(test)]
mod length_gate_tests {
    use super::*;

    fn repeat(n: usize) -> String {
        "a".repeat(n)
    }

    #[test]
    fn accepts_780_ticket_and_reads_back_on_same_egress() {
        let pins = TurnStatePins::default();
        let now = SystemTime::now();
        let value = repeat(780);
        assert!(
            pins.pin_account_wide(
                "acct", "bind".into(), "gpt-6-astra", TURN_STATE_SCOPE_LEN,
                "egr".into(), &value, now, now,
            )
            .is_ok()
        );
        let attempt = pins.attempt(
            "acct", "bind".into(), "gpt-6-astra", "cli", TURN_STATE_SCOPE_LEN, "egr", now,
        );
        assert_eq!(attempt.value(), Some(value.as_str()));
    }

    #[test]
    fn accepts_legacy_lengths() {
        let pins = TurnStatePins::default();
        let now = SystemTime::now();
        for len in [292usize, 332, 356] {
            assert!(
                pins.pin_account_wide(
                    "a", "b".into(), "m", TURN_STATE_SCOPE_LEN, "e".into(),
                    &repeat(len), now, now,
                )
                .is_ok(),
                "len {len} should pin"
            );
        }
    }

    #[test]
    fn rejects_too_short_and_non_ascii() {
        let pins = TurnStatePins::default();
        let now = SystemTime::now();
        assert!(matches!(
            pins.pin_account_wide("a", "b".into(), "m", TURN_STATE_SCOPE_LEN, "e".into(), &repeat(100), now, now),
            Err(PinRejected::Length)
        ));
        let non_ascii = "\u{00e9}".repeat(300);
        assert!(matches!(
            pins.pin_account_wide("a", "b".into(), "m", TURN_STATE_SCOPE_LEN, "e".into(), &non_ascii, now, now),
            Err(PinRejected::Length)
        ));
    }
}
