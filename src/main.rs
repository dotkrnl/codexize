use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use codexize::{
    app, app_runtime,
    data::notifications,
    picker, preflight,
    state::{self},
    tui,
};
use std::process::ExitCode;
use tracing::{warn, warn_span};
#[derive(Debug, Parser)]
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
    command: Option<Command>,
}
#[derive(Debug, Subcommand)]
enum Command {
    Ntfy(NtfyCommand),
}
#[derive(Debug, Parser)]
struct NtfyCommand {
    /// Generate and persist a new ntfy topic.
    #[arg(long)]
    reset: bool,
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
    if cli.command.is_some() {
        bail!("error: subcommand cannot be used as a launch plan");
    }
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
    let cli = Cli::parse();
    if cli.command.is_some() {
        return run_cli_command(&cli);
    }
    let plan = plan_launch(&cli)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("codexize-runtime")
        .build()?;
    runtime.block_on(try_main_async(plan))
}
fn run_cli_command(cli: &Cli) -> Result<()> {
    if cli.yolo || cli.cheap || cli.message.is_some() {
        bail!("error: ntfy subcommand does not accept launch flags");
    }
    match cli.command.as_ref().expect("command checked by caller") {
        Command::Ntfy(command) => {
            let config = notifications::ensure_ntfy_config(command.reset)?;
            print_ntfy_config(&config);
            Ok(())
        }
    }
}
fn print_ntfy_config(config: &notifications::NtfyConfig) {
    println!("ntfy topic: {}", config.topic);
    println!("subscribe: {}", config.subscribe_url());
}
async fn try_main_async(plan: LaunchPlan) -> Result<()> {
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
#[path = "main_tests.rs"]
mod tests;
