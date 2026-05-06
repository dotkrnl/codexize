use super::*;

#[test]
#[serial_test::serial(process_cwd)]
fn diagnostics_path_is_session_local_jsonl() {
    let _guard = crate::state::test_fs_lock()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let root = tempfile::TempDir::new().unwrap();
    let previous = std::env::var_os("CODEXIZE_ROOT");
    unsafe {
        std::env::set_var("CODEXIZE_ROOT", root.path());
    }

    let path = session_diagnostics_path("session-1");

    assert_eq!(
        path,
        root.path()
            .join("sessions")
            .join("session-1")
            .join("diagnostics.jsonl")
    );

    unsafe {
        match previous {
            Some(value) => std::env::set_var("CODEXIZE_ROOT", value),
            None => std::env::remove_var("CODEXIZE_ROOT"),
        }
    }
}
