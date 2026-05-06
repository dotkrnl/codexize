use anyhow::{Result, bail};
use clap::Parser;
use codexize::{
    app, app_runtime, picker, preflight,
    state::{self},
    tui,
};
use std::process::ExitCode;
use tracing::{warn, warn_span};
#[derive(Parser)]
#[command(name = "codexize")]
#[command(about = "Agentic development orchestrator", long_about = None)]
struct Cli {
    /// Seed newly created sessions with YOLO mode.
    #[arg(long)]
    yolo: bool,
    /// Seed newly created sessions with Cheap mode.
    #[arg(long)]
    cheap: bool,
    /// Idea text for a direct-create YOLO session (skips the picker).
    /// Requires --yolo. The message is trimmed; internal whitespace and
    /// newlines are preserved verbatim. Blank-after-trim is rejected.
    #[arg(short = 'm', long = "message")]
    message: Option<String>,
}

/// Result of validating CLI flags for the top-level (no-subcommand) path.
///
/// `DirectCreate` skips the picker and creates a fresh YOLO session from
/// the trimmed idea text. `Picker` runs the existing interactive flow.
#[derive(Debug)]
enum LaunchPlan {
    DirectCreate { idea: String, modes: state::Modes },
    Picker { create_modes: state::Modes },
}

fn plan_launch(cli: &Cli) -> Result<LaunchPlan> {
    let create_modes = state::Modes {
        yolo: cli.yolo,
        cheap: cli.cheap,
    };
    match cli.message.as_deref() {
        Some(raw) => {
            if !cli.yolo {
                bail!("error: -m/--message requires --yolo");
            }
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                bail!("error: -m/--message must not be empty after trimming");
            }
            Ok(LaunchPlan::DirectCreate {
                idea: trimmed.to_string(),
                modes: create_modes,
            })
        }
        None => Ok(LaunchPlan::Picker { create_modes }),
    }
}

fn main() -> ExitCode {
    match try_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // By the time we reach this point we should no longer be in an
            // alternate-screen / raw-mode terminal session (see `TerminalGuard`
            // in `try_main_async`). Stderr remains the non-interactive boundary
            // sink for fatal errors before per-session tracing is initialized.
            eprintln!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn try_main() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("codexize-runtime")
        .build()?;
    runtime.block_on(try_main_async())
}

async fn try_main_async() -> Result<()> {
    let cli = Cli::parse();

    let plan = plan_launch(&cli)?;
    struct TerminalGuard {
        terminal: Option<tui::AppTerminal>,
    }

    impl TerminalGuard {
        fn start() -> Result<Self> {
            Ok(Self {
                terminal: Some(tui::start()?),
            })
        }

        fn terminal_mut(&mut self) -> &mut tui::AppTerminal {
            self.terminal
                .as_mut()
                .expect("terminal already taken from TerminalGuard")
        }

        fn into_terminal(mut self) -> tui::AppTerminal {
            self.terminal
                .take()
                .expect("terminal already taken from TerminalGuard")
        }
    }

    impl Drop for TerminalGuard {
        fn drop(&mut self) {
            if let Some(mut terminal) = self.terminal.take() {
                let _ = tui::stop(&mut terminal);
            }
        }
    }

    let mut terminal_guard = TerminalGuard::start()?;

    if preflight::check(terminal_guard.terminal_mut())? == preflight::PreflightOutcome::Exit {
        let mut terminal = terminal_guard.into_terminal();
        tui::stop(&mut terminal)?;
        return Ok(());
    }

    let (session_id, startup_origin, resume_warnings) = match plan {
        LaunchPlan::DirectCreate { idea, modes } => {
            // Direct creation always produces a fresh session, so the
            // resume-ignored warnings do not apply here.
            (
                picker::create_session(&idea, modes)?,
                app::AppStartupOrigin::Default,
                Vec::new(),
            )
        }
        LaunchPlan::Picker { create_modes } => {
            let mut picker = picker::SessionPicker::new_with_create_modes(create_modes)?;
            let selection = match picker.run(terminal_guard.terminal_mut())? {
                Some(selection) => selection,
                None => {
                    let mut terminal = terminal_guard.into_terminal();
                    tui::stop(&mut terminal)?;
                    return Ok(());
                }
            };

            let resume_warnings = if !selection.created {
                resume_ignored_mode_warnings(create_modes)
            } else {
                Vec::new()
            };
            (
                selection.session_id,
                if selection.created {
                    app::AppStartupOrigin::PickerCreated
                } else {
                    app::AppStartupOrigin::Default
                },
                resume_warnings,
            )
        }
    };

    codexize::diagnostics::init_session_tracing(&session_id)?;
    if !resume_warnings.is_empty() {
        let _span = warn_span!("resume_warnings", session_id = %session_id).entered();
        for warning in resume_warnings {
            warn!("{warning}");
        }
    }

    let mut state = state::SessionState::load(&session_id)?;
    let _ = state::resume::resume_session(&mut state);

    let mut app = app::App::new_with_startup_origin(state, startup_origin);
    let mut terminal = terminal_guard.into_terminal();
    let result = app_runtime::run_terminal_app(&mut app, &mut terminal);
    tui::stop(&mut terminal)?;
    result
}

fn resume_ignored_mode_warnings(modes: state::Modes) -> Vec<&'static str> {
    let mut warnings = Vec::new();
    if modes.yolo {
        warnings.push("warning: --yolo ignored on resume; persisted modes win");
    }
    if modes.cheap {
        warnings.push("warning: --cheap ignored on resume; persisted modes win");
    }
    warnings
}

#[cfg(test)]
mod tests {
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
}
