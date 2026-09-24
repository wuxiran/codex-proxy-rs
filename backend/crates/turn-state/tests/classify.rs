use turn_state::classify::{classify, storable};
use turn_state::{LengthClass, MIN_TURN_STATE_LEN, Settings};

#[test]
fn empty_tables_fall_back_to_the_floor_rule() {
    let settings = Settings::default();
    assert_eq!(classify(MIN_TURN_STATE_LEN, &settings), LengthClass::Normal);
    assert_eq!(classify(780, &settings), LengthClass::Normal);
    assert_eq!(classify(199, &settings), LengthClass::Unknown);
    assert!(storable(&"a".repeat(292), &settings));
    assert!(!storable(&"a".repeat(100), &settings));
    assert!(!storable(&"\u{00e9}".repeat(300), &settings));
}

#[test]
fn configured_tables_classify_exactly() {
    let settings = Settings {
        template_lengths: vec![292],
        degraded_lengths: vec![312],
        ..Settings::default()
    };
    assert_eq!(classify(292, &settings), LengthClass::Normal);
    assert_eq!(classify(312, &settings), LengthClass::Degraded);
    assert_eq!(classify(780, &settings), LengthClass::Unknown);
    assert!(storable(&"a".repeat(292), &settings));
    assert!(!storable(&"a".repeat(312), &settings));
    assert!(!storable(&"a".repeat(780), &settings));
}

#[test]
fn degraded_table_wins_even_under_the_floor_rule() {
    let settings = Settings {
        degraded_lengths: vec![312],
        ..Settings::default()
    };
    assert_eq!(classify(312, &settings), LengthClass::Degraded);
    assert!(!storable(&"a".repeat(312), &settings));
    assert!(storable(&"a".repeat(292), &settings));
}
