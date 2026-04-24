use anyhow::Result;
use clap::{Parser, Subcommand};
use codexize::{
    app, runner,
    state::{self},
    tmux, tui,
    picker,
};
use tokio;

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

fn main() -> Result<()> {
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
            let tmux = tmux::current_context()?;
            let mut terminal = tui::start()?;

            let mut picker = picker::SessionPicker::new()?;
            let session_id = match picker.run(&mut terminal)? {
                Some(id) => id,
                None => {
                    tui::stop(&mut terminal)?;
                    return Ok(());
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
