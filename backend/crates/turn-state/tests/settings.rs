use turn_state::settings::SettingsStore;
use turn_state::{InjectMode, Settings, SettingsError};

#[test]
fn defaults_reproduce_production_behaviour() {
    let settings = Settings::default();
    assert_eq!(settings.ttl_seconds, 3600);
    assert_eq!(settings.inject_mode, InjectMode::Always);
    assert!(!settings.dry_run);
    assert!(settings.log_decisions);
    assert!(settings.template_lengths.is_empty());
    assert!(settings.degraded_lengths.is_empty());
    assert_eq!(settings.validate(), Ok(()));
}

#[test]
fn validation_rejects_bad_ttl_short_lengths_and_overlap() {
    let ttl = Settings {
        ttl_seconds: 10,
        ..Settings::default()
    };
    assert_eq!(ttl.validate(), Err(SettingsError::Ttl));
    let short = Settings {
        template_lengths: vec![10],
        ..Settings::default()
    };
    assert_eq!(short.validate(), Err(SettingsError::LengthTooShort));
    let overlap = Settings {
        template_lengths: vec![292],
        degraded_lengths: vec![292],
        ..Settings::default()
    };
    assert_eq!(overlap.validate(), Err(SettingsError::Overlap));
    let dup = Settings {
        template_lengths: vec![312, 292, 292],
        ..Settings::default()
    }
    .normalized()
    .expect("normalized");
    assert_eq!(dup.template_lengths, vec![292, 312]);
}

#[test]
fn json_uses_camel_case_and_kebab_case_modes() {
    let json = serde_json::to_string(&Settings {
        inject_mode: InjectMode::ReplaceOnly,
        ..Settings::default()
    })
    .expect("json");
    assert!(json.contains("\"injectMode\":\"replace-only\""));
    assert!(json.contains("\"ttlSeconds\":3600"));
    let partial: Settings = serde_json::from_str("{\"dryRun\":true}").expect("partial");
    assert!(partial.dry_run);
    assert_eq!(partial.inject_mode, InjectMode::Always);
}

#[test]
fn store_persists_and_another_handle_sees_the_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let writer = SettingsStore::open(dir.path());
    let saved = writer
        .set(Settings {
            dry_run: true,
            degraded_lengths: vec![312],
            ..Settings::default()
        })
        .expect("saved");
    assert!(saved.dry_run);
    let reader = SettingsStore::open(dir.path());
    assert_eq!(reader.get(), saved);
    assert!(
        writer
            .set(Settings {
                ttl_seconds: 1,
                ..Settings::default()
            })
            .is_err()
    );
    assert_eq!(reader.get(), saved);
}

#[test]
fn corrupt_file_keeps_last_good_settings() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("settings.json"), b"{not json").expect("write");
    let store = SettingsStore::open(dir.path());
    assert_eq!(store.get(), Settings::default());
}
