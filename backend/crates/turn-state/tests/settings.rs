use turn_state::settings::SettingsStore;
use turn_state::{InjectMode, Settings, SettingsError};

#[test]
fn verified_only_mode_requires_probe_checks_and_business_reuse() {
    let mut settings = Settings::default();
    settings.warm_pool.require_verified = true;
    assert!(settings.validate().is_ok());
    settings.warm_pool.probe = false;
    assert_eq!(settings.validate(), Err(SettingsError::WarmVerification));
    settings.warm_pool.probe = true;
    settings.warm_pool.business_reuse = false;
    assert_eq!(settings.validate(), Err(SettingsError::WarmVerification));
}

#[test]
fn warm_probe_rejects_empty_expectations_and_invalid_model_lists() {
    let mut settings = Settings::default();
    settings.warm_pool.probe_expect.clear();
    assert_eq!(settings.validate(), Err(SettingsError::WarmProbe));
    settings.warm_pool.probe = false;
    assert!(settings.validate().is_ok());
    settings.warm_pool.models = vec![" ".to_owned()];
    assert_eq!(settings.validate(), Err(SettingsError::WarmProbe));
    settings.warm_pool.models = (0..17).map(|i| format!("model-{i}")).collect();
    assert_eq!(settings.validate(), Err(SettingsError::WarmProbe));
}

#[test]
fn warm_settings_keep_old_files_compatible_and_deduplicate_only_ascii_case() {
    let mut settings: Settings =
        serde_json::from_str(r#"{"warmPool":{"enabled":true,"models":["gpt-6-astra"]}}"#).unwrap();
    assert!(!settings.warm_pool.require_verified);
    assert!(
        serde_json::to_value(&settings).unwrap()["warmPool"]
            .get("requireVerified")
            .is_none()
    );
    settings.warm_pool.models = vec![
        " gpt-6-astra ".to_owned(),
        "GPT-6-ASTRA".to_owned(),
        "gpt-6-astra-2026-09-15".to_owned(),
    ];
    let normalized = settings.normalized().unwrap();
    assert_eq!(
        normalized.warm_pool.models,
        vec!["gpt-6-astra", "gpt-6-astra-2026-09-15"]
    );
}

#[test]
fn defaults_reproduce_production_behaviour() {
    let settings = Settings::default();
    assert_eq!(settings.ttl_seconds, 240);
    assert_eq!(settings.inject_mode, InjectMode::FillMissing);
    assert_eq!(
        settings.served_mismatch_action,
        turn_state::ServedMismatchAction::Observe
    );
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
    assert!(json.contains("\"ttlSeconds\":240"));
    assert!(json.contains("\"servedMismatchAction\":\"observe\""));
    let partial: Settings = serde_json::from_str("{\"dryRun\":true}").expect("partial");
    assert!(partial.dry_run);
    assert_eq!(partial.inject_mode, InjectMode::FillMissing);
    let fill = serde_json::to_string(&Settings::default()).expect("json");
    assert!(fill.contains("\"injectMode\":\"fill-missing\""));
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

#[test]
fn mint_proxy_secrets_are_redacted_and_survive_unchanged_form_submission() {
    let mut current = Settings::default();
    current.cloud_mint.upstream_proxy_url =
        "socks5h://example-user:example-password@proxy.example:1080".to_owned();
    current.cloud_mint.proxy_url = "http://relay-user:relay-password@proxy.example:8080".to_owned();
    let view = current.redacted();
    assert_eq!(view.cloud_mint.upstream_proxy_url, "<set>");
    assert_eq!(view.cloud_mint.proxy_url, "<set>");
    let json = serde_json::to_string(&view).unwrap();
    assert!(!json.contains("example-password"));
    assert!(!json.contains("relay-password"));
    assert!(!format!("{current:?}").contains("example-password"));
    assert!(!format!("{current:?}").contains("relay-password"));
    assert_eq!(view.merge_secret_placeholders(&current), current);
    let mut cleared = current.redacted();
    cleared.cloud_mint.upstream_proxy_url.clear();
    assert!(
        cleared
            .merge_secret_placeholders(&current)
            .cloud_mint
            .upstream_proxy_url
            .is_empty()
    );
}

#[test]
fn old_drop_pair_settings_load_as_block_without_business_retry() {
    let settings: Settings =
        serde_json::from_str(r#"{"servedMismatchAction":"drop-pair"}"#).unwrap();
    assert_eq!(
        settings.served_mismatch_action,
        turn_state::ServedMismatchAction::Block
    );
    assert!(
        serde_json::to_string(&settings)
            .unwrap()
            .contains(r#""servedMismatchAction":"block""#)
    );
}
