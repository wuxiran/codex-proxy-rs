//! OpenAI Provider、凭据与上游传输的测试入口

mod admin;
mod cf_cookie_pool; // fork: cf cookie pool
mod config;
mod credential;
mod jitter;
mod provider;
mod support;
mod transport;
mod turn_state_mint; // fork: cloud mint
mod turn_state_pin; // fork: turn-state-pin
mod ws_warm_pool; // fork: ws-warm-pool

#[derive(Default)]
struct RecordingDiagnostics(std::sync::Mutex<Vec<gateway_core::diagnostics::OperationalFailure>>);
#[async_trait::async_trait]
impl gateway_core::diagnostics::OperationalDiagnostics for RecordingDiagnostics {
    async fn record_failure(
        &self,
        failure: gateway_core::diagnostics::OperationalFailure,
    ) -> Result<(), gateway_core::error::StoreError> {
        self.0.lock().unwrap().push(failure);
        Ok(())
    }
}
