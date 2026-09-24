use turn_state::decision::decide;
use turn_state::{Decision, InjectMode, Settings};

fn replace_only() -> Settings {
    Settings {
        inject_mode: InjectMode::ReplaceOnly,
        template_lengths: vec![292],
        degraded_lengths: vec![312],
        ..Settings::default()
    }
}

#[test]
fn no_live_template_always_passes() {
    let verdict = decide(None, Some(&"x".repeat(312)), &Settings::default());
    assert_eq!(verdict.decision, Decision::Pass);
    assert!(verdict.replacement.is_none());
}

#[test]
fn identical_value_is_already_current() {
    let live = "t".repeat(292);
    let verdict = decide(Some(&live), Some(&live), &replace_only());
    assert_eq!(verdict.decision, Decision::Pass);
    assert_eq!(verdict.reason, "header already current");
}

#[test]
fn always_mode_adds_or_replaces() {
    let live = "t".repeat(292);
    let added = decide(Some(&live), None, &Settings::default());
    assert_eq!(added.decision, Decision::Inject);
    assert_eq!(added.replacement.as_deref(), Some(live.as_str()));
    let replaced = decide(Some(&live), Some(&"c".repeat(780)), &Settings::default());
    assert_eq!(replaced.decision, Decision::Inject);
    assert_eq!(replaced.replacement.as_deref(), Some(live.as_str()));
}

#[test]
fn replace_only_touches_only_degraded_lengths() {
    let live = "t".repeat(292);
    let settings = replace_only();
    let substituted = decide(Some(&live), Some(&"d".repeat(312)), &settings);
    assert_eq!(substituted.decision, Decision::Substitute);
    assert_eq!(substituted.replacement.as_deref(), Some(live.as_str()));
    let other = decide(Some(&live), Some(&"o".repeat(292)), &settings);
    assert_eq!(other.decision, Decision::Pass);
    assert!(other.replacement.is_none());
    let none = decide(Some(&live), None, &settings);
    assert_eq!(none.decision, Decision::Pass);
    assert!(none.replacement.is_none());
}
