use crate::{tmux::TmuxContext, tui::AppTerminal};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};
use std::time::Duration;

#[derive(Debug)]
pub struct App {
    tmux: TmuxContext,
    phase: PipelinePhase,
    steps: Vec<PipelineStep>,
}

#[derive(Debug, Clone, Copy)]
enum PipelinePhase {
    Idle,
}

#[derive(Debug)]
struct PipelineStep {
    name: &'static str,
    status: StepStatus,
}

#[derive(Debug, Clone, Copy)]
enum StepStatus {
    Pending,
}

impl App {
    pub fn new(tmux: TmuxContext) -> Self {
        Self {
            tmux,
            phase: PipelinePhase::Idle,
            steps: vec![
                PipelineStep::pending("Idea"),
                PipelineStep::pending("Brainstorm"),
                PipelineStep::pending("Spec Review"),
                PipelineStep::pending("Planning"),
                PipelineStep::pending("Plan Reviews"),
                PipelineStep::pending("Plan Approval"),
                PipelineStep::pending("Implementation Loop"),
            ],
        }
    }

    pub fn run(&mut self, terminal: &mut AppTerminal) -> Result<()> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;

            if event::poll(Duration::from_millis(250))? {
                if let Event::Key(key) = event::read()? {
                    if should_quit(key) {
                        return Ok(());
                    }
                }
            }
        }
    }

    fn draw(&self, frame: &mut Frame<'_>) {
        let root = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(10),
                Constraint::Length(1),
            ])
            .split(frame.area());

        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(root[1]);

        let left = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(7), Constraint::Min(8)])
            .split(body[0]);

        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(body[1]);

        frame.render_widget(self.header(), root[0]);
        frame.render_widget(self.status(), left[0]);
        frame.render_widget(self.pipeline(), left[1]);
        frame.render_widget(self.models(), right[0]);
        frame.render_widget(self.rankings(), right[1]);
        frame.render_widget(self.footer(), root[2]);
    }

    fn header(&self) -> Paragraph<'_> {
        let title = Line::from(vec![
            Span::styled(
                "Codexize",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                "tmux-owned agent pipeline",
                Style::default().fg(Color::Gray),
            ),
        ]);

        Paragraph::new(title).block(Block::default().borders(Borders::BOTTOM))
    }

    fn status(&self) -> Paragraph<'_> {
        let lines = vec![
            Line::from(vec![
                Span::styled("Phase: ", Style::default().fg(Color::Gray)),
                Span::styled(self.phase.label(), Style::default().fg(Color::Yellow)),
            ]),
            Line::from(vec![
                Span::styled("Tmux:  ", Style::default().fg(Color::Gray)),
                Span::raw(format!(
                    "{}:{} {}",
                    self.tmux.session_name, self.tmux.window_index, self.tmux.window_name
                )),
            ]),
            Line::from(vec![
                Span::styled("Run:   ", Style::default().fg(Color::Gray)),
                Span::raw("none"),
            ]),
            Line::from(vec![
                Span::styled("State: ", Style::default().fg(Color::Gray)),
                Span::raw("waiting for an idea"),
            ]),
        ];

        Paragraph::new(lines)
            .block(
                Block::default()
                    .title("Current Status")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: true })
    }

    fn pipeline(&self) -> List<'_> {
        let items = self
            .steps
            .iter()
            .map(|step| {
                let marker = step.status.marker();
                ListItem::new(Line::from(vec![
                    Span::styled(marker, Style::default().fg(step.status.color())),
                    Span::raw(" "),
                    Span::raw(step.name),
                ]))
            })
            .collect::<Vec<_>>();

        List::new(items).block(Block::default().title("Pipeline").borders(Borders::ALL))
    }

    fn models(&self) -> Paragraph<'_> {
        let lines = vec![
            Line::from(vec![
                Span::styled("Available models: ", Style::default().fg(Color::Gray)),
                Span::raw("not loaded"),
            ]),
            Line::from(vec![
                Span::styled("Stupid level:     ", Style::default().fg(Color::Gray)),
                Span::raw("unknown"),
            ]),
            Line::from(vec![
                Span::styled("Remaining quota:  ", Style::default().fg(Color::Gray)),
                Span::raw("unknown"),
            ]),
            Line::from(""),
            Line::from("No provider adapter has reported telemetry."),
        ];

        Paragraph::new(lines)
            .block(Block::default().title("Models").borders(Borders::ALL))
            .wrap(Wrap { trim: true })
    }

    fn rankings(&self) -> Paragraph<'_> {
        let lines = vec![
            Line::from(vec![
                Span::styled("Brainstorm: ", Style::default().fg(Color::Gray)),
                Span::raw("unranked"),
            ]),
            Line::from(vec![
                Span::styled("Planning:   ", Style::default().fg(Color::Gray)),
                Span::raw("unranked"),
            ]),
            Line::from(vec![
                Span::styled("Coding:     ", Style::default().fg(Color::Gray)),
                Span::raw("unranked"),
            ]),
            Line::from(vec![
                Span::styled("Review:     ", Style::default().fg(Color::Gray)),
                Span::raw("unranked"),
            ]),
        ];

        Paragraph::new(lines)
            .block(
                Block::default()
                    .title("Selection Rank")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: true })
    }

    fn footer(&self) -> Paragraph<'_> {
        Paragraph::new("q / Esc / Ctrl-C quit")
    }
}

impl PipelineStep {
    fn pending(name: &'static str) -> Self {
        Self {
            name,
            status: StepStatus::Pending,
        }
    }
}

impl PipelinePhase {
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
        }
    }
}

impl StepStatus {
    fn marker(self) -> &'static str {
        match self {
            Self::Pending => "[ ]",
        }
    }

    fn color(self) -> Color {
        match self {
            Self::Pending => Color::DarkGray,
        }
    }
}

fn should_quit(key: KeyEvent) -> bool {
    if key.kind != KeyEventKind::Press {
        return false;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => true,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
        _ => false,
    }
}
