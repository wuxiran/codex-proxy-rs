//! fork：上游声明的模型与请求不一致时，终态连接不回池；声明缺失不算不一致。

use provider_openai::transport::websocket::{
    TerminalRelease, WebSocketTerminalKind, note_served_model, terminal_release,
};

#[test]
fn a_mismatch_on_the_terminal_frame_discards_without_a_consumer_write() {
    let mut discard = false;
    note_served_model(
        "gpt-6-astra",
        Some("gpt-6-astra"),
        Some("gpt-5.6-luna"),
        &mut discard,
    );
    assert!(discard);
    assert!(matches!(
        terminal_release(WebSocketTerminalKind::Completed, discard),
        TerminalRelease::DiscardMismatch
    ));
}

#[test]
fn a_header_mismatch_discards_even_when_the_body_matches() {
    let mut discard = false;
    note_served_model(
        "gpt-6-astra",
        Some("gpt-5.6-luna"),
        Some("gpt-6-astra"),
        &mut discard,
    );
    assert!(matches!(
        terminal_release(WebSocketTerminalKind::Interrupted, discard),
        TerminalRelease::DiscardMismatch
    ));
}

#[test]
fn a_matching_declaration_returns_the_connection() {
    let mut discard = false;
    note_served_model(
        "gpt-6-astra",
        Some("GPT-6-ASTRA"),
        Some("gpt-6-astra"),
        &mut discard,
    );
    assert!(!discard);
    assert!(matches!(
        terminal_release(WebSocketTerminalKind::Completed, discard),
        TerminalRelease::ReturnToPool
    ));
}

#[test]
fn a_missing_declaration_does_not_discard() {
    let mut discard = false;
    note_served_model("gpt-6-astra", None, None, &mut discard);
    assert!(matches!(
        terminal_release(WebSocketTerminalKind::Completed, discard),
        TerminalRelease::ReturnToPool
    ));
    assert!(matches!(
        terminal_release(WebSocketTerminalKind::Failed, true),
        TerminalRelease::DiscardFailed
    ));
}
