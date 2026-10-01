//! fork：请求设置重编译与 rebase 不得丢失宿主的日志采集边界。

use super::*;

#[test]
fn request_log_policy_survives_unrelated_overrides_and_rebases_with_the_host() {
    let host = snapshot(1, "first");
    let values = host
        .settings()
        .clone()
        .with_request_log(false, Some("capture-test-key".to_owned()));
    let decoded: SettingsValues =
        serde_json::from_value(serde_json::to_value(values).unwrap()).unwrap();
    let frozen = Arc::new(host.with_settings(&decoded).unwrap());
    let request = RequestSettings::new(frozen.clone());
    let changed = request
        .replace(
            request
                .values()
                .clone()
                .with_model_mappings(BTreeMap::from([("alias".to_owned(), "model".to_owned())])),
            "plugin",
        )
        .unwrap();

    assert!(!changed.snapshot().request_log_enabled());
    assert_eq!(
        changed.snapshot().request_log_test_key_id(),
        Some("capture-test-key")
    );
    assert_eq!(changed.snapshot().mapped_model("alias"), "model");
    assert_eq!(frozen.mapped_model("alias"), "alias");

    let rebased = changed.rebase(snapshot(2, "next")).unwrap();
    assert!(rebased.snapshot().request_log_enabled());
    assert_eq!(rebased.snapshot().request_log_test_key_id(), None);
    assert!(!frozen.request_log_enabled());
}
