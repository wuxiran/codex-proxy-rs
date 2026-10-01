//! 业务响应的模型对照：上游声明的模型与实际发送的是否一致。
//!
//! 每收到一块响应就对照一次，所以 `response.created` 一到就能发现换模型，早于事件交给
//! 下游。发现后这一轮签发的 state 不再进 pin 和会话，正在复用的 pin 条件失效；
//! 结果按「账号 × 模型」计数，并连同本次请求的路由节点写进请求日志。

use std::time::SystemTime;

use gateway_core::account::ProviderAccount;
use turn_state::{ServedMatch, ServedMismatchAction};

use serde_json::json;

use super::execution::OpenAiSessionCapture;
use super::observation::OpenAiResponseObservationState;
use crate::credential::{CodexCredentialSelector, RuntimeCodexCookie};
use crate::route_pair::RoutePairRef;
use crate::transport::canonical::CodexCanonicalDecoder;
use crate::transport::client::CodexResponseMetadataUpdates;
use crate::turn_state_pin::{PinAttempt, TurnStatePins};

/// 本次请求的路由节点是从哪里得知的。
#[derive(Clone, Copy, PartialEq, Eq)]
enum RouteSource {
    /// 请求带出去的 pair。
    Request,
    /// 上游在这次响应里新发的 pair。
    Response,
    /// WebSocket 连接握手时用的 pair（握手响应新发的，或握手请求带的）。
    Connection,
}

impl RouteSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Response => "response",
            Self::Connection => "connection",
        }
    }
}

pub(super) struct ServedWatch {
    pins: TurnStatePins,
    account: String,
    sent_model: String,
    /// 诊断请求和经临时出口的探测不计入：它们说明的不是这个账号的业务流量。
    active: bool,
    verdict: ServedMatch,
    completed: bool,
    /// 这次响应实际走的 pair。WebSocket 以连接握手时的为准，不看账号此刻的 cookie：
    /// 复用的连接留在握手时的节点上，期间账号的 pair 可能已被别的请求换掉。
    route: Option<(RoutePairRef, RouteSource)>,
    /// WebSocket 响应与转发任务共享的状态；用来要求终态后不把连接放回池。
    connection: Option<CodexResponseMetadataUpdates>,
    handled: bool,
    block_response: bool,
}

impl ServedWatch {
    pub(super) fn new(pins: &TurnStatePins, account: &str, sent_model: &str, active: bool) -> Self {
        Self {
            pins: pins.clone(),
            account: account.to_owned(),
            sent_model: sent_model.to_owned(),
            active,
            verdict: ServedMatch::Unknown,
            completed: false,
            route: None,
            connection: None,
            handled: false,
            block_response: false,
        }
    }

    /// 阻断当前响应，绝不请求 Core 重放业务请求。
    pub(super) const fn block_response(&self) -> bool {
        self.block_response
    }

    /// 上游响应头到达后调用一次：定下这次响应走的是哪一对路由 cookie。
    pub(super) async fn routed(
        &mut self,
        sent_cookies: &[RuntimeCodexCookie],
        set_cookie_headers: &[String],
        websocket: Option<&CodexResponseMetadataUpdates>,
    ) {
        if let Some(updates) = websocket {
            self.route = updates
                .lock()
                .await
                .route_pair
                .clone()
                .map(|pair| (pair, RouteSource::Connection));
            self.connection = Some(updates.clone());
            return;
        }
        self.route = RoutePairRef::issued(set_cookie_headers)
            .map(|pair| (pair, RouteSource::Response))
            .or_else(|| RoutePairRef::sent(sent_cookies).map(|pair| (pair, RouteSource::Request)));
    }

    /// 在本块的 metadata 合并之后、会话更新挂到事件上之前调用。
    pub(super) async fn observe(
        &mut self,
        decoder: &CodexCanonicalDecoder,
        pin: Option<&mut PinAttempt>,
        session_capture: &mut Option<OpenAiSessionCapture>,
        selector: &CodexCredentialSelector,
        account: &ProviderAccount,
    ) {
        if !self.active {
            return;
        }
        if decoder.served_mismatch() {
            self.verdict = ServedMatch::Mismatch;
        }
        if self.verdict != ServedMatch::Mismatch {
            let (header, body) = decoder.declared_models();
            match turn_state::served::compare(&self.sent_model, header, body) {
                // 没有新声明不推翻已有的一致结论。
                ServedMatch::Unknown => {}
                verdict => self.verdict = verdict,
            }
        }
        if self.verdict != ServedMatch::Mismatch {
            return;
        }
        // 先读设置。模拟运行只记日志，不改 pin、会话、pair，也不要求重试。
        let settings = self.pins.service().settings();
        let disposition = mismatch_disposition(settings.served_mismatch_action, settings.dry_run);
        if disposition.invalidate_pin
            && let Some(pin) = pin
        {
            pin.served_mismatch(SystemTime::now());
        }
        // 这一轮的 state 是换模型后的服务签发的：同轮后续请求不能再回放它。
        // 每个块都清一次，因为本块开头可能刚把新票写进会话捕获。
        if disposition.clear_session_ticket
            && let Some(capture) = session_capture.as_mut()
        {
            capture.turn_state = None;
        }
        if std::mem::replace(&mut self.handled, true) {
            return;
        }
        self.block_response = disposition.block_response;
        let dropped = match &self.route {
            Some((pair, _)) if disposition.drop_pair => {
                selector.drop_route_pair(account, &pair.fingerprint).await
            }
            _ => false,
        };
        if disposition.discard_connection
            && let Some(connection) = &self.connection
        {
            connection.lock().await.discard_connection = true;
        }
        tracing::warn!(
            target: "turn_state",
            account = self.account.as_str(),
            model = self.sent_model.as_str(),
            gateway = self.gateway().unwrap_or(""),
            action = settings.served_mismatch_action.as_str(),
            dry_run = settings.dry_run,
            pair_dropped = dropped,
            block_response = self.block_response,
            "[turn-state] upstream served a different model than the one requested"
        );
    }

    /// 把路由节点和模型对照结果并入 Provider 元数据，随请求记录落库
    /// （`model_requests.provider_observation_json`），按节点统计换模型率用。
    pub(super) fn annotate(&self, observation: &mut OpenAiResponseObservationState) {
        let metadata = &mut observation.fork_metadata;
        if let Some((pair, source)) = &self.route {
            if let Some(gateway) = &pair.gateway {
                metadata.insert("upstreamGateway".to_owned(), json!(gateway));
            }
            metadata.insert("upstreamGatewaySource".to_owned(), json!(source.as_str()));
        }
        if self.active && self.verdict != ServedMatch::Unknown {
            metadata.insert("servedMatch".to_owned(), json!(self.verdict.as_str()));
        }
    }

    /// 响应完整结束；只有完整的响应才记「一致」或「没有声明」。
    pub(super) fn completed(&mut self) {
        self.completed = true;
    }

    /// 把模型对照结果和路由节点补进请求日志的响应侧补丁；不计入的请求不写对照结果。
    pub(super) fn log_patch(
        &self,
        mut patch: gateway_core::request_log::ResponsePatch,
    ) -> gateway_core::request_log::ResponsePatch {
        patch.served_match = self.active.then(|| self.verdict.as_str());
        patch.gateway = self.gateway().map(str::to_owned);
        patch.gateway_source = self.route.as_ref().map(|(_, source)| source.as_str());
        patch
    }

    /// 路由 cookie 里的节点标签（`unified-N`）。
    fn gateway(&self) -> Option<&str> {
        self.route
            .as_ref()
            .and_then(|(pair, _)| pair.gateway.as_deref())
    }
}

/// 换模型之后哪些状态可以动。模拟运行一律不动。
struct MismatchDisposition {
    invalidate_pin: bool,
    clear_session_ticket: bool,
    drop_pair: bool,
    discard_connection: bool,
    block_response: bool,
}

fn mismatch_disposition(action: ServedMismatchAction, dry_run: bool) -> MismatchDisposition {
    if dry_run {
        return MismatchDisposition {
            invalidate_pin: false,
            clear_session_ticket: false,
            drop_pair: false,
            discard_connection: false,
            block_response: false,
        };
    }
    let drop_pair = action == ServedMismatchAction::Block;
    MismatchDisposition {
        invalidate_pin: true,
        clear_session_ticket: true,
        drop_pair,
        discard_connection: drop_pair,
        block_response: drop_pair,
    }
}

impl Drop for ServedWatch {
    fn drop(&mut self) {
        if !self.active || !(self.completed || self.verdict == ServedMatch::Mismatch) {
            return;
        }
        self.pins.service().record_served(
            &self.account,
            &self.sent_model,
            self.verdict,
            SystemTime::now(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::mismatch_disposition;
    use turn_state::ServedMismatchAction;

    #[test]
    fn blocked_model_error_never_authorizes_replay_or_account_rotation() {
        let error = crate::provider::execution::served_mismatch_error();
        assert!(!error.replay_is_safe());
        assert!(error.pre_delivery_retry().is_none());
        assert_eq!(
            error.client_visible_upstream_error().unwrap().code(),
            Some("degraded_model_blocked")
        );
    }

    #[test]
    fn dry_run_does_not_mutate_or_retry() {
        for action in [ServedMismatchAction::Observe, ServedMismatchAction::Block] {
            let disposition = mismatch_disposition(action, true);
            assert!(!disposition.invalidate_pin);
            assert!(!disposition.clear_session_ticket);
            assert!(!disposition.drop_pair);
            assert!(!disposition.discard_connection);
            assert!(!disposition.block_response);
        }
    }

    #[test]
    fn observe_invalidates_the_ticket_without_retrying() {
        let disposition = mismatch_disposition(ServedMismatchAction::Observe, false);
        assert!(disposition.invalidate_pin);
        assert!(disposition.clear_session_ticket);
        assert!(!disposition.drop_pair);
        assert!(!disposition.discard_connection);
        assert!(!disposition.block_response);
    }

    #[test]
    fn block_mode_stops_the_response_without_requesting_replay() {
        let disposition = mismatch_disposition(ServedMismatchAction::Block, false);
        assert!(disposition.invalidate_pin);
        assert!(disposition.clear_session_ticket);
        assert!(disposition.drop_pair);
        assert!(disposition.discard_connection);
        assert!(disposition.block_response);
    }
}
