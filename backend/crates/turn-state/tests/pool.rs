use turn_state::pool::{PoolAccount, PoolActivity, PoolConnection, PoolSnapshot, PoolTicket};

fn account(id: &str, model: &str, verification: &str, until: u64) -> PoolAccount {
    PoolAccount {
        account_id: id.to_owned(),
        name: id.to_owned(),
        participating: true,
        schedulable: true,
        phase: "ready".to_owned(),
        tickets: vec![PoolTicket {
            model: model.to_owned(),
            gateway: Some("unified-26".to_owned()),
            expires_at_ms: until,
        }],
        connections: vec![PoolConnection {
            id: format!("connection-{id}"),
            model: model.to_owned(),
            gateway: Some("unified-26".to_owned()),
            verification: verification.to_owned(),
            available: true,
            verified_at_ms: Some(500),
            verification_expires_at_ms: Some(until),
            expires_at_ms: 60_000,
        }],
        mint: PoolActivity::default(),
        warm: PoolActivity::default(),
    }
}

#[test]
fn shared_gateway_keeps_account_model_and_verification_counts_distinct() {
    let snapshot = PoolSnapshot::new(
        1000,
        vec![
            account("a", "model-a", "fresh", 4000),
            account("b", "model-b", "unchecked", 9000),
        ],
    );
    assert_eq!(snapshot.totals.gateways, 1);
    assert_eq!(snapshot.totals.tickets, 2);
    assert_eq!(snapshot.totals.available_connections, 2);
    assert_eq!(snapshot.totals.verified_connections, 1);
    assert_eq!(snapshot.gateways[0].account_ids.len(), 2);
    assert_eq!(snapshot.gateways[0].models.len(), 2);
    assert_eq!(snapshot.valid_until_ms, 4000);
}

#[test]
fn expired_or_missing_proof_never_counts_as_verified_supply() {
    let mut missing = account("b", "model-b", "fresh", 5000);
    missing.connections[0].verification_expires_at_ms = None;
    let snapshot = PoolSnapshot::new(1000, vec![account("a", "model-a", "fresh", 999), missing]);
    assert_eq!(snapshot.totals.tickets, 1);
    assert_eq!(snapshot.totals.available_connections, 0);
    assert_eq!(snapshot.totals.verified_connections, 0);
    assert!(snapshot.accounts.iter().all(|a| a.phase == "idle"));
    assert!(
        snapshot
            .accounts
            .iter()
            .all(|a| a.connections[0].verification == "expired")
    );
}

#[test]
fn disabled_account_does_not_turn_a_fresh_proof_into_available_capacity() {
    let mut disabled = account("a", "model-a", "fresh", 5000);
    disabled.schedulable = false;
    disabled.phase = "unavailable".to_owned();
    let snapshot = PoolSnapshot::new(1000, vec![disabled]);
    assert_eq!(snapshot.totals.available_connections, 0);
    assert_eq!(snapshot.accounts[0].connections[0].verification, "fresh");
    let wire = serde_json::to_value(snapshot).unwrap();
    assert_eq!(wire["scope"], "current_process");
    assert!(
        wire["accounts"][0]["connections"][0]
            .get("turnState")
            .is_none()
    );
    assert!(wire["accounts"][0].get("credentials").is_none());
}
