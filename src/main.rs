mod app;
mod artifacts;
mod claude;
mod codex;
mod dashboard;
mod gemini;
mod kimi;
mod runner;
mod selection;
mod state;
mod tmux;
mod tui;
mod warmup;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::{env, fs};

#[derive(Parser)]
#[command(name = "codexize")]
#[command(about = "Agentic development orchestrator", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run an agent for a specific phase (used by orchestrated windows)
    AgentRun {
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        phase: String,
        #[arg(long)]
        role: String,
        #[arg(last = true)]
        command: Vec<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::AgentRun {
            run_id,
            phase,
            role,
            command,
        }) => runner::run(run_id, phase, role, command),
        None => {
            let tmux = tmux::current_context()?;
            let state = load_or_create_state()?;
            let mut terminal = tui::start()?;
            let result = app::App::new(tmux, state).run(&mut terminal);
            tui::stop(&mut terminal)?;
            result
        }
    }
}

fn load_or_create_state() -> Result<state::RunState> {
    let runs_dir = state::run_dir("");
    if runs_dir.exists() {
        let mut entries: Vec<_> = fs::read_dir(&runs_dir)?
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .collect();
        entries.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());

        if let Some(latest) = entries.last() {
            if let Some(run_id) = latest.file_name().to_str() {
                if let Ok(state) = state::RunState::load(run_id) {
                    state.log_event("resuming run")?;
                    return Ok(state);
                }
            }
        }
    }

    let run_id = env::var("CODEXIZE_RUN_ID").unwrap_or_else(|_| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        format!("{now}")
    });

    let state = state::RunState::new(run_id);
    state.save()?;
    state.log_event("starting new run")?;
    Ok(state)
}
