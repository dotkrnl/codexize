mod app;
mod codex;
mod tmux;
mod tui;

use anyhow::Result;

fn main() -> Result<()> {
    let tmux = tmux::current_context()?;
    let mut terminal = tui::start()?;
    let result = app::App::new(tmux).run(&mut terminal);
    tui::stop(&mut terminal)?;
    result
}
