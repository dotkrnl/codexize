use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use codexize::{
    app, picker, preflight, runner,
    state::{self},
    tmux, tui,
};
use std::process::ExitCode;
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
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run an agent for a specific phase (used by orchestrated windows)
    AgentRun {
        #[arg(long)]
        session_id: String,
        #[arg(long)]
        phase: String,
        #[arg(long)]
        role: String,
        /// Required artifact paths — agent is blocked from stopping until all exist
        #[arg(long = "artifact")]
        artifacts: Vec<String>,
        #[arg(last = true)]
        command: Vec<String>,
    },
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
            // This crate does not install a tracing subscriber today, so
            // stderr is the existing non-interactive boundary-error sink.
            eprintln!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn try_main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::AgentRun {
            session_id,
            phase,
            role,
            artifacts,
            command,
        }) => runner::run(session_id, phase, role, artifacts, command),
        None => {
            let plan = plan_launch(&cli)?;
            let tmux = tmux::current_context()?;
            let mut terminal = tui::start()?;

            preflight::check(&mut terminal, &tmux)?;

            let session_id = match plan {
                LaunchPlan::DirectCreate { idea, modes } => {
                    // Direct creation always produces a fresh session, so the
                    // resume-ignored warnings do not apply here.
                    picker::create_session(&idea, modes)?
                }
                LaunchPlan::Picker { create_modes } => {
                    let mut picker = picker::SessionPicker::new_with_create_modes(create_modes)?;
                    let selection = match picker.run(&mut terminal)? {
                        Some(selection) => selection,
                        None => {
                            tui::stop(&mut terminal)?;
                            return Ok(());
                        }
                    };

                    if !selection.created {
                        for warning in resume_ignored_mode_warnings(create_modes) {
                            eprintln!("{warning}");
                        }
                    }
                    selection.session_id
                }
            };

            let mut state = state::SessionState::load(&session_id)?;
            let _ = state::resume::resume_session(&mut state);

            let result = app::App::new(tmux, state).run(&mut terminal);
            tui::stop(&mut terminal)?;
            result
        }
    }
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
