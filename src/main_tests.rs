use super::*;
use codexize::state::Modes;

#[test]
fn cheap_flag_parses_as_create_mode_seed() {
    let cli = Cli::try_parse_from(["codexize", "--cheap"]).expect("parse --cheap");
    assert!(cli.cheap);
    assert!(!cli.yolo);
}

#[test]
fn yolo_flag_parses_as_create_mode_seed() {
    let cli = Cli::try_parse_from(["codexize", "--yolo"]).expect("parse --yolo");
    assert!(cli.yolo);
    assert!(!cli.cheap);
}

#[test]
fn yolo_and_cheap_flags_combine() {
    let cli =
        Cli::try_parse_from(["codexize", "--yolo", "--cheap"]).expect("parse --yolo --cheap");
    assert!(cli.yolo);
    assert!(cli.cheap);
}

#[test]
fn resume_warning_mentions_ignored_cheap_flag() {
    let warnings = resume_ignored_mode_warnings(Modes {
        yolo: false,
        cheap: true,
    });
    assert_eq!(
        warnings,
        vec!["warning: --cheap ignored on resume; persisted modes win"]
    );
}

#[test]
fn resume_warning_mentions_ignored_yolo_flag() {
    let warnings = resume_ignored_mode_warnings(Modes {
        yolo: true,
        cheap: false,
    });
    assert_eq!(
        warnings,
        vec!["warning: --yolo ignored on resume; persisted modes win"]
    );
}

#[test]
fn resume_warning_mentions_both_ignored_flags() {
    let warnings = resume_ignored_mode_warnings(Modes {
        yolo: true,
        cheap: true,
    });
    assert_eq!(
        warnings,
        vec![
            "warning: --yolo ignored on resume; persisted modes win",
            "warning: --cheap ignored on resume; persisted modes win",
        ]
    );
}

#[test]
fn message_short_flag_parses_with_yolo() {
    let cli = Cli::try_parse_from(["codexize", "--yolo", "-m", "ship the dashboard"])
        .expect("parse --yolo -m ...");
    assert!(cli.yolo);
    assert_eq!(cli.message.as_deref(), Some("ship the dashboard"));
}

#[test]
fn message_long_flag_with_equals_parses() {
    let cli = Cli::try_parse_from(["codexize", "--yolo", "--message=ship it"])
        .expect("parse --yolo --message=...");
    assert_eq!(cli.message.as_deref(), Some("ship it"));
}

#[test]
fn message_can_appear_before_yolo() {
    let cli = Cli::try_parse_from(["codexize", "-m", "ship it", "--yolo"])
        .expect("parse -m ... --yolo");
    assert!(cli.yolo);
    assert_eq!(cli.message.as_deref(), Some("ship it"));
}

fn cli(args: &[&str]) -> Cli {
    Cli::try_parse_from(args).expect("parse cli args")
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
