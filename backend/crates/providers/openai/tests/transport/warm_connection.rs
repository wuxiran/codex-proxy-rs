//! fork：暖池验收证明的寿命与作用域——240s 到期、票/模型/策略变了不能沿用、复探不延长旧票证明。

use std::time::Duration;

use provider_openai::transport::{
    WarmConnectionApproval, WarmVerificationContext, WarmVerificationStatus,
};

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
