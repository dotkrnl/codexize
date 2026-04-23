use crate::selection::{ModelStatus, QuotaError};
use ratatui::style::{Color, Style};
use std::sync::mpsc;
use std::time::Instant;

#[derive(Debug)]
pub(crate) struct PipelineSection {
    pub(super) name: String,
    pub(super) status: SectionStatus,
    pub(super) summary: String,
    pub(super) events: Vec<String>,
    pub(super) transcript: Vec<String>,
    pub(super) input_placeholder: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SectionStatus {
    Pending,
    Running,
    WaitingUser,
    Done,
}

#[derive(Debug)]
pub(super) enum ModelRefreshState {
    Fetching {
        rx: mpsc::Receiver<(Vec<ModelStatus>, Vec<QuotaError>)>,
        started_at: Instant,
    },
    Idle(Instant),
}

impl PipelineSection {
    pub(super) fn done(
        name: impl Into<String>,
        summary: impl Into<String>,
        events: Vec<impl Into<String>>,
        transcript: Vec<impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            status: SectionStatus::Done,
            summary: summary.into(),
            events: events.into_iter().map(Into::into).collect(),
            transcript: transcript.into_iter().map(Into::into).collect(),
            input_placeholder: None,
        }
    }

    pub(super) fn waiting_user(
        name: impl Into<String>,
        summary: impl Into<String>,
        events: Vec<impl Into<String>>,
        transcript: Vec<impl Into<String>>,
        input_placeholder: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            status: SectionStatus::WaitingUser,
            summary: summary.into(),
            events: events.into_iter().map(Into::into).collect(),
            transcript: transcript.into_iter().map(Into::into).collect(),
            input_placeholder: Some(input_placeholder.into()),
        }
    }

    pub(super) fn action(
        name: impl Into<String>,
        summary: impl Into<String>,
        events: Vec<impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            status: SectionStatus::WaitingUser,
            summary: summary.into(),
            events: events.into_iter().map(Into::into).collect(),
            transcript: Vec::new(),
            input_placeholder: None,
        }
    }

    pub(super) fn pending(name: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: SectionStatus::Pending,
            summary: summary.into(),
            events: Vec::new(),
            transcript: Vec::new(),
            input_placeholder: None,
        }
    }

    pub(super) fn running(
        name: impl Into<String>,
        summary: impl Into<String>,
        events: Vec<impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            status: SectionStatus::Running,
            summary: summary.into(),
            events: events.into_iter().map(Into::into).collect(),
            transcript: Vec::new(),
            input_placeholder: None,
        }
    }
}

impl SectionStatus {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::WaitingUser => "waiting-user",
            Self::Done => "done",
        }
    }

    pub(super) fn style(self) -> Style {
        match self {
            Self::Pending => Style::default().fg(Color::DarkGray),
            Self::Running => Style::default().fg(Color::Cyan),
            Self::WaitingUser => Style::default().fg(Color::Yellow),
            Self::Done => Style::default().fg(Color::Green),
        }
    }
}
