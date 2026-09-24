use turn_state::fs_util::{atomic_write, lock, sanitize_component};

#[test]
fn sanitize_rejects_traversal_and_encodes_the_rest() {
    assert_eq!(sanitize_component(".."), None);
    assert_eq!(sanitize_component("."), None);
    assert_eq!(sanitize_component(""), None);
    assert_eq!(sanitize_component("a\0b"), None);
    assert_eq!(sanitize_component("a\nb"), None);
    assert_eq!(
        sanitize_component("acct/../x").as_deref(),
        Some("acct%2F..%2Fx")
    );
    assert_eq!(
        sanitize_component("gpt-6-astra").as_deref(),
        Some("gpt-6-astra")
    );
    assert_eq!(sanitize_component("a\\b").as_deref(), Some("a%5Cb"));
    assert_ne!(sanitize_component("a%2Fb"), sanitize_component("a/b"));
}

#[test]
fn atomic_write_leaves_no_temp_files_and_lock_is_reentrant_across_handles() {
    let dir = tempfile::tempdir().expect("tempdir");
    atomic_write(dir.path(), "x.json", b"{}").expect("write");
    assert_eq!(
        std::fs::read(dir.path().join("x.json")).expect("read"),
        b"{}"
    );
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .expect("dir")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
    let guard = lock(dir.path()).expect("lock");
    drop(guard);
    let _again = lock(dir.path()).expect("lock again");
}
