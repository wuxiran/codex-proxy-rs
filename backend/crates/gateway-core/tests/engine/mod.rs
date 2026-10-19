//! 执行引擎各阶段与端口合同的测试入口

mod admission;
mod connection;
mod continuation;
mod coordinator;
mod execution;
mod extensions;
mod fork_coordinator; // fork: attribution
mod fork_provider; // fork: attribution
mod middleware;
mod policy;
mod probe; // fork: warm verification trace
mod provider;
mod response_control;

use gateway_core::engine::AttemptTrigger;

#[test]
fn attempt_trigger_names_should_match_ops_event_contract() {
    assert_eq!(AttemptTrigger::AccountRetry.as_str(), "account_retry");
}
