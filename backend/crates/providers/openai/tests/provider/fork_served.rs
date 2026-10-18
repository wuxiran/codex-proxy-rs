//! fork：上游换模型后的处置——模拟运行不动状态，观察只作废票，阻断才丢 pair 与连接。

use provider_openai::{mismatch_disposition, served_mismatch_error};
use turn_state::ServedMismatchAction;

#[test]
fn blocked_model_error_never_authorizes_replay_or_account_rotation() {
    let error = served_mismatch_error();
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
