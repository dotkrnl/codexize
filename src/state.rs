use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{BufRead, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Phase {
    IdeaInput,
    BrainstormRunning,
    SpecReviewRunning,
    PlanningRunning,
    PlanReviewRunning,
    AwaitingPlanApproval,
    ImplementationRound(u32),
    ReviewRound(u32),
    Done,
    BlockedNeedsUser,
}

impl Phase {
    pub fn label(&self) -> String {
        match self {
            Phase::IdeaInput => "Idea Input".to_string(),
            Phase::BrainstormRunning => "Brainstorming".to_string(),
            Phase::SpecReviewRunning => "Spec Review".to_string(),
            Phase::PlanningRunning => "Planning".to_string(),
            Phase::PlanReviewRunning => "Plan Review".to_string(),
            Phase::AwaitingPlanApproval => "Awaiting Approval".to_string(),
            Phase::ImplementationRound(r) => format!("Implementation (Round {r})"),
            Phase::ReviewRound(r) => format!("Review (Round {r})"),
            Phase::Done => "Done".to_string(),
            Phase::BlockedNeedsUser => "Blocked".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub timestamp: String,
    pub phase: Phase,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunState {
    pub run_id: String,
    pub current_phase: Phase,
    #[serde(default)]
    pub idea_text: Option<String>,
    #[serde(default)]
    pub selected_model: Option<String>,
    #[serde(default)]
    pub agent_error: Option<String>,
}

impl RunState {
    pub fn new(run_id: String) -> Self {
        Self {
            run_id,
            current_phase: Phase::IdeaInput,
            idea_text: None,
            selected_model: None,
            agent_error: None,
        }
    }

    pub fn load(run_id: &str) -> Result<Self> {
        let path = run_dir(run_id).join("run.toml");
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read run state from {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("failed to parse run state from {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let dir = run_dir(&self.run_id);
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create run directory {}", dir.display()))?;
        let path = dir.join("run.toml");
        let text = toml::to_string_pretty(self).context("failed to serialize run state")?;
        fs::write(&path, text)
            .with_context(|| format!("failed to write run state to {}", path.display()))?;
        Ok(())
    }

    pub fn load_events(&self) -> Result<Vec<Event>> {
        let path = run_dir(&self.run_id).join("events.jsonl");
        if !path.exists() {
            return Ok(Vec::new());
        }

        let file = fs::File::open(&path)?;
        let reader = std::io::BufReader::new(file);
        let mut events = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let event: Event = serde_json::from_str(&line)
                .with_context(|| format!("failed to parse event line: {}", line))?;
            events.push(event);
        }

        Ok(events)
    }

    pub fn log_event(&self, message: impl Into<String>) -> Result<()> {
        let dir = run_dir(&self.run_id);
        fs::create_dir_all(&dir)?;
        let path = dir.join("events.jsonl");

        let event = Event {
            timestamp: chrono::Utc::now().to_rfc3339(),
            phase: self.current_phase,
            message: message.into(),
        };

        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        let line = serde_json::to_string(&event).context("failed to serialize event")?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    pub fn transition_to(&mut self, next_phase: Phase) -> Result<()> {
        let old_phase = self.current_phase;
        self.current_phase = next_phase;
        self.log_event(format!(
            "transitioned phase from {:?} to {:?}",
            old_phase, next_phase
        ))?;
        self.save()?;
        Ok(())
    }
}

pub fn run_dir(run_id: &str) -> PathBuf {
    Path::new(".codexize").join("runs").join(run_id)
}
