use std::time::{Duration, SystemTime, UNIX_EPOCH};

use turn_state::observe::{ObservationInput, Observations};
use turn_state::{Decision, LengthClass};

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

fn input(
    len: Option<usize>,
    class: Option<LengthClass>,
    injected: bool,
    when: u64,
) -> ObservationInput {
    ObservationInput {
        account: "acct".into(),
        model: "gpt-6-astra".into(),
        issued_len: len,
        class,
        decision: if injected {
            Decision::Inject
        } else {
            Decision::Pass
        },
        injected,
        at: at(when),
    }
}

#[test]
fn blind_zone_and_ineffective_template_are_counted_separately() {
    let obs = Observations::in_memory();
    obs.record(&input(None, None, true, 1_800_000_000));
    obs.record(&input(
        Some(312),
        Some(LengthClass::Degraded),
        true,
        1_800_000_001,
    ));
    obs.record(&input(
        Some(292),
        Some(LengthClass::Normal),
        false,
        1_800_000_002,
    ));
    obs.record(&input(
        Some(999),
        Some(LengthClass::Unknown),
        false,
        1_800_000_003,
    ));
    let snapshot = obs.snapshot(at(1_800_000_010));
    let tally = &snapshot.buckets["acct/gpt-6-astra"];
    assert_eq!(tally.silent, 1);
    assert_eq!(tally.injected_silent, 1);
    assert_eq!(tally.injected_degraded, 1);
    assert_eq!(tally.degraded, 1);
    assert_eq!(tally.normal, 1);
    assert_eq!(tally.unknown, 1);
    assert_eq!(tally.injected_total, 2);
    assert_eq!(tally.last_issued_len, Some(999));
    assert_eq!(tally.lengths[&292], 1);
    assert_eq!(snapshot.histogram[&312], 1);
    assert_eq!(snapshot.events.len(), 4);
    assert_eq!(snapshot.hourly.len(), 1);
}

#[test]
fn two_handles_on_one_file_merge_deltas_instead_of_overwriting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blue = Observations::open(dir.path());
    let green = Observations::open(dir.path());
    blue.record(&input(
        Some(292),
        Some(LengthClass::Normal),
        false,
        1_800_000_000,
    ));
    green.record(&input(
        Some(292),
        Some(LengthClass::Normal),
        false,
        1_800_000_001,
    ));
    let _ = blue.snapshot(at(1_800_000_002));
    let merged = green.snapshot(at(1_800_000_003));
    assert_eq!(merged.buckets["acct/gpt-6-astra"].normal, 2);
    assert_eq!(merged.events.len(), 2);
    let reread = Observations::open(dir.path()).snapshot(at(1_800_000_004));
    assert_eq!(reread.buckets["acct/gpt-6-astra"].normal, 2);
}

#[test]
fn ring_is_capped_and_old_hours_are_pruned() {
    let obs = Observations::in_memory();
    for index in 0..150u64 {
        obs.record(&input(
            Some(292),
            Some(LengthClass::Normal),
            false,
            1_800_000_000 + index,
        ));
    }
    let snapshot = obs.snapshot(at(1_800_000_200));
    assert_eq!(snapshot.events.len(), 100);
    obs.record(&input(
        Some(292),
        Some(LengthClass::Normal),
        false,
        1_800_000_000 + 60 * 3600,
    ));
    let later = obs.snapshot(at(1_800_000_000 + 60 * 3600));
    assert_eq!(later.hourly.len(), 1);
    assert_eq!(later.buckets["acct/gpt-6-astra"].normal, 151);
}
