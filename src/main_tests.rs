use super::*;
use codexize::state::Modes;
use serial_test::serial;
use std::ffi::OsString;

/// RAII override of `CODEXIZE_CONFIG`. Mirrors the guard used by the
/// `data::config::cli` tests so the env mutation is undone even on
/// panic, and so concurrent tests serialized via `#[serial]` see a
/// consistent prior value on Drop.
struct ConfigEnvGuard {
    prev: Option<OsString>,
}

impl ConfigEnvGuard {
    fn set(path: &std::path::Path) -> Self {
        let prev = std::env::var_os("CODEXIZE_CONFIG");
        // SAFETY: callers hold the `serial_test` global lock for the
        // duration of the test, and Drop restores the prior value.
        unsafe {
            std::env::set_var("CODEXIZE_CONFIG", path);
        }
        Self { prev }
    }
}

impl Drop for ConfigEnvGuard {
    fn drop(&mut self) {
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var("CODEXIZE_CONFIG", v),
                None => std::env::remove_var("CODEXIZE_CONFIG"),
            }
        }
    }
}

#[test]
#[serial]
fn launch_load_silently_uses_defaults_when_file_missing() {
    let dir = tempfile::tempdir().unwrap();
    // File is intentionally not created — exercises the missing-file
    // fall-through that must NOT be fatal at first launch.
    let path = dir.path().join("config.toml");
    let _g = ConfigEnvGuard::set(&path);

    let cfg = load_config_for_launch().expect("missing file falls back to baked defaults");
    let baked = codexize::data::config::Config::baked_defaults();
    assert_eq!(
        cfg.meta.version, baked.meta.version,
        "missing-file path returns baked defaults, not a synthesized config"
    );
}

#[test]
fn resume_warning_mentions_ignored_launch_flags_in_order() {
    for (modes, expected) in [
        (
            Modes {
                yolo: false,
                cheap: true,
            },
            vec!["warning: --cheap ignored on resume; persisted modes win"],
        ),
        (
            Modes {
                yolo: true,
                cheap: false,
            },
            vec!["warning: --yolo ignored on resume; persisted modes win"],
        ),
        (
            Modes {
                yolo: true,
                cheap: true,
            },
            vec![
                "warning: --yolo ignored on resume; persisted modes win",
                "warning: --cheap ignored on resume; persisted modes win",
            ],
        ),
    ] {
        assert_eq!(resume_ignored_mode_warnings(modes), expected);
    }
}

fn cli(args: &[&str]) -> Cli {
    Cli::try_parse_from(args).expect("parse cli args")
}

#[test]
fn ntfy_command_rejects_launch_flags() {
    let cli = cli(&["codexize", "--yolo", "ntfy"]);

    let err = run_cli_command(&cli).expect_err("ntfy rejects launch flags");

    assert!(
        err.to_string().contains("launch flags"),
        "error mentions launch flags: {err}"
    );
}

#[test]
fn plan_launch_rejects_subcommands() {
    let err = plan_launch(&cli(&["codexize", "ntfy"])).expect_err("subcommand is not a launch");
    assert!(
        err.to_string().contains("subcommand"),
        "error mentions subcommand path: {err}"
    );
}

#[test]
fn plan_launch_yolo_message_returns_direct_create() {
    let plan = plan_launch(&cli(&["codexize", "--yolo", "-m", "  ship it  "]))
        .expect("plan accepts trimmed message");
    match plan {
        LaunchPlan::DirectCreate { idea, modes } => {
            assert_eq!(idea, "ship it", "message is trimmed before storage");
            assert!(modes.yolo);
            assert!(!modes.cheap);
        }
        LaunchPlan::Picker { .. } => panic!("expected DirectCreate"),
    }
}

#[test]
fn plan_launch_yolo_cheap_message_carries_both_modes() {
    let plan = plan_launch(&cli(&["codexize", "--yolo", "--cheap", "-m", "ship it"]))
        .expect("plan accepts --cheap with -m");
    match plan {
        LaunchPlan::DirectCreate { modes, .. } => {
            assert!(modes.yolo);
            assert!(modes.cheap, "--cheap must propagate to direct create");
        }
        LaunchPlan::Picker { .. } => panic!("expected DirectCreate"),
    }
}

#[test]
fn plan_launch_message_without_yolo_errors() {
    let err = plan_launch(&cli(&["codexize", "-m", "ship it"]))
        .expect_err("plan rejects -m without --yolo");
    assert!(
        err.to_string().contains("--yolo"),
        "error mentions --yolo requirement: {err}"
    );
}

#[test]
fn plan_launch_blank_message_after_trim_errors() {
    let err = plan_launch(&cli(&["codexize", "--yolo", "-m", "   \t  "]))
        .expect_err("plan rejects whitespace-only message");
    assert!(
        err.to_string().contains("empty"),
        "error mentions empty message: {err}"
    );
}

#[test]
fn plan_launch_no_message_returns_picker() {
    let plan = plan_launch(&cli(&["codexize", "--yolo"])).expect("plan with no -m");
    assert!(matches!(plan, LaunchPlan::Picker { .. }));
}

#[test]
fn plan_launch_preserves_internal_whitespace() {
    // Internal newlines and runs of whitespace are preserved verbatim;
    // only leading/trailing whitespace is trimmed.
    let plan = plan_launch(&cli(&["codexize", "--yolo", "-m", "  line 1\nline 2  "]))
        .expect("plan accepts multiline trimmed message");
    let LaunchPlan::DirectCreate { idea, .. } = plan else {
        panic!("expected DirectCreate");
    };
    assert_eq!(idea, "line 1\nline 2");
}
