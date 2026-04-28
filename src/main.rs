use anyhow::Result;
use clap::{Parser, Subcommand};
use codexize::{
    app, picker, preflight, runner,
    state::{self},
    tmux, tui,
};
#[derive(Parser)]
#[command(name = "codexize")]
#[command(about = "Agentic development orchestrator", long_about = None)]
struct Cli {
    /// Seed newly created sessions with Cheap mode.
    #[arg(long)]
    cheap: bool,
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

            preflight::check(&mut terminal, &tmux)?;

            let create_modes = state::Modes {
                yolo: false,
                cheap: cli.cheap,
            };
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

            let mut state = state::SessionState::load(&selection.session_id)?;
            let _ = state::resume::resume_session(&mut state);

            let result = app::App::new(tmux, state).run(&mut terminal);
            tui::stop(&mut terminal)?;
            result
        }
    }
}

fn resume_ignored_mode_warnings(modes: state::Modes) -> Vec<&'static str> {
    let mut warnings = Vec::new();
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
}
