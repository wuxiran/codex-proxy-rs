//! 账号自有 turn-state 的 pin：薄壳，存储、到期、决策与观测全部委托给 `turn_state` crate。
//!
//! 这里只保留 Provider 内部习惯的调用面（账号/绑定/模型/客户端/出口逐个传参），
//! 让 hunt、续期与管理端无需感知 crate 的类型。

use std::{collections::BTreeMap, time::SystemTime};

pub(crate) use turn_state::{
    MIN_TURN_STATE_LEN, PinRejected, PinStatus, credential_binding, egress_fingerprint,
};

/// 作用域里保留的 length 分量已不再作长度门；统一取此常量，使同一账号/模型/出口下
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
        // 正常/受限档由 turn_state 运行设置提供，靠下限+ASCII 校验判合法。
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

/// 不实现 Debug，防止不透明上游值被日志意外展开。
#[derive(Clone)]
pub(crate) struct TurnStatePins(turn_state::TurnStateService);

impl Default for TurnStatePins {
    fn default() -> Self {
        Self(turn_state::TurnStateService::in_memory())
    }
}

impl TurnStatePins {
    pub(crate) fn from_service(service: turn_state::TurnStateService) -> Self {
        Self(service)
    }

    pub(crate) fn service(&self) -> &turn_state::TurnStateService {
        &self.0
    }

    /// 请求侧钩子：`carried` 是客户端自带的 state，决策要拿它和桶内模板对照。
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
        carried: Option<&str>,
        now: SystemTime,
    ) -> PinAttempt {
        let _ = expected_length; // 长度门已废弃：作用域不再含长度分量。
        PinAttempt(self.0.begin_request(turn_state::RequestFacts {
            account,
            binding,
            model,
            client,
            egress,
            carried,
            now,
        }))
    }

    pub(crate) fn status(&self, account: &str, binding: &str, now: SystemTime) -> Vec<PinStatus> {
        self.0.status(account, binding, now)
    }

    /// 管理员显式钉住一个刚在目标出口上观测到的 state。它替换该账号该模型下的全部旧
    /// state（旧值来自换绑前的出口，是否仍有效未知）；被动捕获仍然永不覆盖。
    /// 返回生效模板的到期时间（磁盘上已有更新的同绑定模板时以它为准）。
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
    ) -> Result<SystemTime, PinRejected> {
        let _ = expected_length; // 长度门已废弃：只作下限+ASCII 校验，不再精确匹配长度。
        self.0.pin_account_wide(turn_state::AccountWidePin {
            account,
            binding,
            model,
            egress,
            value,
            captured_at,
            now,
            source: turn_state::Source::Hunt,
            ttl: None,
            gateway: None,
        })
    }

    /// 账号级 state 的到期时刻；自动续期据此判断是否临近到期。
    pub(crate) fn account_wide_expires_at(
        &self,
        account: &str,
        binding: &str,
        model: &str,
        egress: &str,
        now: SystemTime,
    ) -> Option<SystemTime> {
        self.0
            .account_wide_expires_at(account, binding, model, egress, now)
    }

    pub(crate) fn clear(&self, account: &str) {
        self.0.clear(account);
    }
}

pub(crate) struct PinAttempt(turn_state::Attempt);

impl PinAttempt {
    pub(crate) fn value(&self) -> Option<&str> {
        self.0.value()
    }

    /// 只接收本次真实上游响应；客户端传入的 state 不具备捕获资格。
    pub(crate) fn observe(&mut self, value: Option<&str>) {
        self.0.observe(value);
    }

    pub(crate) fn completed(&mut self, now: SystemTime) {
        self.0.completed(now);
    }

    /// 桶里没有有效模板：账号此模型缺票。
    pub(crate) fn needs_template(&self) -> bool {
        self.0.needs_template()
    }
}
