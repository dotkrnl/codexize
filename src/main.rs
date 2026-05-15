use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use codexize::{
    app, app_shell,
    data::{app_lock, config::cli as config_cli},
    state::{self},
    ui::{preflight, tui, widgets::picker::state as picker},
};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
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
    /// Mint and print the active ntfy topic.
    Ntfy,
    /// Inspect or mutate the unified config file.
    Config(ConfigCommand),
}
#[derive(Debug, Parser)]
struct ConfigCommand {
    #[command(subcommand)]
    sub: ConfigSubcommand,
}
#[derive(Debug, Subcommand)]
enum ConfigSubcommand {
    /// Print the resolved config path on its own line.
    Path,
    /// Print the canonical fully-annotated default dump.
    Defaults,
    /// Write the annotated default dump to the resolved path.
    Init {
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
    },
    /// Print the effective config (or one section) in TOML form.
    List { section: Option<String> },
    /// Print the scalar (or sub-table) at the given dotted key.
    Get { key: String },
    /// Set a key to a value (parsed per declared type).
    Set { key: String, value: String },
    /// Drop the override at the given key.
    Unset { key: String },
    /// With `<section>`: drop overrides in that section. Without: delete
    /// the file entirely (requires --yes on a non-tty stderr).
    Reset {
        section: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Validate the file at `<path>` (default: resolved config path).
    Validate { path: Option<PathBuf> },
    /// Spawn $EDITOR (fallback $VISUAL, then `vi`) on the config path
    /// and validate on exit.
    Edit,
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
        bail!("error: subcommands do not accept launch flags");
    }
    match &cli.command {
        Some(Command::Ntfy) => run_ntfy_command(),
        Some(Command::Config(command)) => run_config_command(command),
        None => bail!("error: no subcommand provided"),
    }
}
fn run_ntfy_command() -> Result<()> {
    let config = config_cli::ntfy_reset_topic()?;
    let topic = config.ntfy.topic.value();
    let server = config.ntfy.server.value().trim_end_matches('/');
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "ntfy topic: {topic}")?;
    writeln!(stdout, "subscribe: {server}/{topic}")?;
    Ok(())
}
fn run_config_command(command: &ConfigCommand) -> Result<()> {
    let mut stdout = io::stdout().lock();
    match &command.sub {
        ConfigSubcommand::Path => config_cli::run_path(&mut stdout),
        ConfigSubcommand::Defaults => config_cli::run_defaults(&mut stdout),
        ConfigSubcommand::Init { force } => config_cli::run_init(*force, &mut stdout),
        ConfigSubcommand::List { section } => config_cli::run_list(section.as_deref(), &mut stdout),
        ConfigSubcommand::Get { key } => config_cli::run_get(key, &mut stdout),
        ConfigSubcommand::Set { key, value } => config_cli::run_set(key, value, &mut stdout),
        ConfigSubcommand::Unset { key } => config_cli::run_unset(key, &mut stdout),
        ConfigSubcommand::Reset { section, yes } => config_cli::run_reset(
            section.as_deref(),
            *yes,
            io::stderr().is_terminal(),
            &mut stdout,
        ),
        ConfigSubcommand::Validate { path } => {
            config_cli::run_validate(path.as_deref(), &mut stdout)
        }
        ConfigSubcommand::Edit => config_cli::run_edit(io::stdin().is_terminal(), &mut stdout),
    }
}
/// Resolve the config for launch. A missing file returns baked defaults
/// silently; any other loader error (I/O, parse, unknown key, type
/// mismatch, unsupported version, validation) is fatal so the binary
/// refuses to launch on a malformed file rather than silently degrading.
fn load_config_for_launch() -> Result<codexize::data::config::Config> {
    codexize::data::config::load_or_default().map_err(anyhow::Error::from)
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
    // Load config BEFORE taking over the terminal so any error message
    // lands on a normal stderr (alternate-screen + raw mode would swallow
    // it). Missing-file is handled inside the loader as "use baked
    // defaults"; only true I/O, parse, unknown-key, type-mismatch,
    // unsupported-version, and validation errors abort launch.
    let config = std::sync::Arc::new(load_config_for_launch()?);
    // Acquire `<.codexize>/app.lock` before the terminal switches to raw
    // mode so the spec-pinned refusal messages reach a normal stderr.
    // The guard is moved into AppShell after the startup picker selects the
    // first session. Terminal shutdown still waits for active runners before
    // the shell drops, so the lock is removed after runner finalization.
    let project_root = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("failed to read current directory: {e}"))?;
    let lock_path = state::codexize_root().join(app_lock::APP_LOCK_FILENAME);
    let app_lock_guard = app_lock::acquire(&lock_path, &project_root).map_err(|err| match err {
        app_lock::AcquireError::Io(io_err) => io_err,
        other => anyhow::Error::new(other),
    })?;
    let mut terminal_guard = TerminalGuard::start()?;
    // When running inside tmux, label the active window with the working
    // directory so an operator with several codexize panes can tell them
    // apart at a glance. No-op outside tmux.
    codexize::data::tmux::maybe_set_window_title();
    let install_view = config.acp_install_view();
    if preflight::check(terminal_guard.terminal_mut(), &install_view.claude_acp_root)?
        == preflight::PreflightOutcome::Exit
    {
        let mut terminal = terminal_guard.into_terminal();
        tui::stop(&mut terminal)?;
        return Ok(());
    }
    let paths_view = config.paths_view();
    let sessions_root = codexize::data::picker_io::sessions_root_for(&config);
    let memory_root_override: Option<std::path::PathBuf> = config
        .paths
        .memory_root
        .is_explicit()
        .then(|| paths_view.memory_root.clone());
    let (session_id, startup_origin, resume_warnings) = match plan {
        LaunchPlan::DirectCreate { idea, modes } => {
            // Direct creation always produces a fresh session, so the
            // resume-ignored warnings do not apply here.
            (
                picker::create_session(&idea, modes, memory_root_override.as_deref())?,
                app::AppStartupOrigin::Default,
                Vec::new(),
            )
        }
        LaunchPlan::Picker { create_modes } => {
            let mut picker = picker::SessionPicker::new_with_paths(
                create_modes,
                sessions_root,
                memory_root_override,
            )?;
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
    let diag = config.diagnostics_view();
    codexize::diagnostics::init_session_tracing(&session_id, &diag)?;
    if !resume_warnings.is_empty() {
        let _span = warn_span!("resume_warnings", session_id = %session_id).entered();
        for warning in resume_warnings {
            warn!("{warning}");
        }
    }
    let mut state = state::SessionState::load(&session_id)?;
    if let Err(e) = state::resume_session(&mut state) {
        tracing::warn!("session resume produced warnings: {e:#}");
    }
    let mut shell = app_shell::AppShell::new_with_app_lock(
        state,
        startup_origin,
        config,
        Some(app_lock_guard),
    )?;
    let mut terminal = terminal_guard.into_terminal();
    let result = shell.run_focused_terminal_app(&mut terminal);
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
