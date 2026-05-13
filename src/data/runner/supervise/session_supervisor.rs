use crate::app::{AppStartupOrigin, watchdog, YoloExitObservation};
use crate::data::config::Config;
use crate::selection::{CachedModel, QuotaError, SubscriptionKind};
use crate::state::{self as session_state, Message, MessageKind, MessageSender, Phase, RunStatus, SessionState};
use crate::scheduler::{ScannedSession, decide_waiting_dispatch, WaitingDispatch};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorTick {
    pub state_changed: bool,
    pub run_started: Option<u64>,
    pub run_finished: Option<u64>,
    pub live_summary_changed: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum SchedulerDrive {
    AutoLaunch,
    DispatchWaiting,
}

impl SchedulerDrive {
    pub fn apply(&self, supervisor: &mut SessionSupervisor) {
        match self {
            Self::AutoLaunch => {
                supervisor.poll_agent_run();
                supervisor.maybe_auto_launch();
            }
            Self::DispatchWaiting => {
                supervisor.dispatch_waiting_to_implement();
                supervisor.maybe_auto_launch();
            }
        }
    }
}

pub struct SessionSupervisor {
    pub(crate) session_id: String,
    pub(crate) state: SessionState,
    pub(crate) runner_supervisor: crate::runner::Supervisor,
    pub(crate) runner_config: crate::runner::RunnerConfig,
    pub(crate) messages: Vec<Message>,
    pub(crate) live_summary_cached_text: String,
    pub(crate) live_summary_cached_mtime: Option<std::time::SystemTime>,
    pub(crate) startup_origin: AppStartupOrigin,
    pub(crate) current_run_id: Option<u64>,
    pub(crate) run_launched: bool,
    pub(crate) quota_errors: Vec<QuotaError>,
    pub(crate) quota_retry_delay: Duration,
    pub(crate) watchdog: watchdog::WatchdogRegistry,
    pub(crate) notification_runtime: crate::data::notifications::NotificationRuntime,
    pub(crate) interactive_wait_marker: Option<crate::data::notifications::InteractiveWaitMarker>,
    pub(crate) agent_line_count: usize,
    pub(crate) agent_content_hash: u64,
    pub(crate) agent_last_change: Option<Instant>,
    pub(crate) failed_models: HashMap<(String, Option<u32>, u32), HashSet<(SubscriptionKind, String)>>,
    pub(crate) yolo_exit_issued: HashSet<u64>,
    pub(crate) yolo_exit_observations: HashMap<u64, YoloExitObservation>,
    pub(crate) live_summary_path: Option<std::path::PathBuf>,
    pub(crate) live_summary_watcher: Option<notify::RecommendedWatcher>,
    pub(crate) live_summary_change_events: Option<crate::data::events::LiveSummaryEvents>,
    pub(crate) cache_watcher: Option<crate::data::cache::CacheWatcher>,
    pub(crate) pending_drain_deadline: Option<Instant>,
    pub(crate) config: Arc<Config>,
    pub(crate) paths: crate::data::config::view::PathsView,
    pub(crate) memory_view: crate::data::config::view::MemoryView,
    pub(crate) models: Vec<CachedModel>,
}

impl SessionSupervisor {
    pub fn new(state: SessionState, startup_origin: AppStartupOrigin, config: Arc<Config>) -> Self {
        let session_id = state.session_id.clone();
        let runner_supervisor = crate::runner::Supervisor::new(config.clone());
        let runner_config = crate::runner::RunnerConfig {
            full_review_interval: config.runner_view().full_review_interval,
        };
        let messages = SessionState::load_messages(&session_id).unwrap_or_default();
        let current_run_id = state
            .agent_runs
            .iter()
            .find(|run| run.status == RunStatus::Running)
            .map(|run| run.id);
        let run_launched = current_run_id.is_some();

        let ntfy_params = crate::data::notifications::NotificationParams::from_view(&config.ntfy_view());

        let paths = config.paths_view();
        let memory_view = config.memory_view();

        Self {
            session_id,
            state,
            runner_supervisor,
            runner_config,
            messages,
            live_summary_cached_text: String::new(),
            live_summary_cached_mtime: None,
            startup_origin,
            current_run_id,
            run_launched,
            quota_errors: Vec::new(),
            quota_retry_delay: Duration::from_secs(60),
            watchdog: watchdog::WatchdogRegistry::from_env(),
            notification_runtime: crate::data::notifications::NotificationRuntime::new(ntfy_params),
            interactive_wait_marker: None,
            agent_line_count: 0,
            agent_content_hash: 0,
            agent_last_change: None,
            failed_models: HashMap::new(),
            yolo_exit_issued: HashSet::new(),
            yolo_exit_observations: HashMap::new(),
            live_summary_path: None,
            live_summary_watcher: None,
            live_summary_change_events: None,
            cache_watcher: None,
            pending_drain_deadline: None,
            config,
            paths,
            memory_view,
            models: Vec::new(),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn phase(&self) -> Phase {
        self.state.current_phase
    }

    pub fn current_run_id(&self) -> Option<u64> {
        self.current_run_id
    }

    pub fn drive(&mut self, drive: SchedulerDrive) -> (SessionState, SupervisorTick) {
        let before_run_id = self.current_run_id;
        let before_live_summary = self.live_summary_cached_text.clone();
        
        drive.apply(self);

        let after_run_id = self.current_run_id;
        let live_summary_changed = (self.live_summary_cached_text != before_live_summary)
            .then(|| self.live_summary_cached_text.clone());
        
        let tick = SupervisorTick {
            state_changed: true,
            run_started: (before_run_id != after_run_id && after_run_id.is_some())
                .then_some(after_run_id)
                .flatten(),
            run_finished: (before_run_id != after_run_id && after_run_id.is_none())
                .then_some(before_run_id)
                .flatten(),
            live_summary_changed,
        };
        (self.state.clone(), tick)
    }

    pub(crate) fn maybe_auto_launch(&mut self) {
        // This method will be implemented once the launch_* helpers are moved.
        // For now, it's a no-op to allow building.
    }

    pub(crate) fn dispatch_waiting_to_implement(&mut self) {
        let (current_baseline, _) = self.compute_repo_state_update_inputs();
        let decision = decide_waiting_dispatch(
            self.state.planned_after_session_id.as_deref(),
            current_baseline.as_deref(),
        );
        let next_phase = match decision {
            WaitingDispatch::Sharding => Phase::ShardingRunning,
            WaitingDispatch::RepoStateUpdate => Phase::RepoStateUpdateRunning,
        };
        let _ = self.transition_to_phase(next_phase);
    }

    fn compute_repo_state_update_inputs(&self) -> (Option<String>, Vec<std::path::PathBuf>) {
        let sessions_root = self.sessions_root();
        let Ok(scanned) = crate::data::picker_io::scan_sessions_for_scheduler(&sessions_root) else {
            return (None, Vec::new());
        };
        let baseline = scanned
            .iter()
            .filter_map(|s| if let ScannedSession::Loaded(ls) = s { Some(ls) } else { None })
            .filter(|s| s.session_id < self.session_id && s.current_phase == Phase::Done)
            .last()
            .map(|s| s.session_id.clone());
        let waiting_specs = scanned
            .into_iter()
            .filter_map(|s| if let ScannedSession::Loaded(ls) = s { Some(ls) } else { None })
            .filter(|s| s.session_id < self.session_id && s.current_phase == Phase::WaitingToImplement)
            .map(|s| self.sessions_root().join(s.session_id).join("artifacts/spec.md"))
            .collect();
        (baseline, waiting_specs)
    }

    pub(crate) fn transition_to_phase(&mut self, next_phase: Phase) -> Result<()> {
        session_state::execute_transition(&mut self.state, next_phase)?;
        if let Phase::ImplementationRound(round) = next_phase {
            let round_dir = self.session_dir().join("rounds").join(format!("{round:03}"));
            self.capture_round_base(&round_dir);
        }
        self.agent_line_count = 0;
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        Ok(())
    }

    pub(crate) fn session_dir(&self) -> std::path::PathBuf {
        self.sessions_root().join(&self.session_id)
    }

    pub(crate) fn sessions_root(&self) -> std::path::PathBuf {
        if self.config.paths.sessions_root.is_explicit() {
            self.paths.sessions_root.clone()
        } else {
            crate::state::codexize_root().join("sessions")
        }
    }

    pub(crate) fn capture_round_base(&self, round_dir: &std::path::Path) {
        let scope_file = round_dir.join("review_scope.toml");
        if scope_file.exists() {
            return;
        }
        if let Some(parent) = scope_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        #[cfg(test)]
        let _ = std::fs::write(&scope_file, "base_sha = \"test-base\"\n");
        #[cfg(not(test))]
        if let Some(sha) = crate::app::prompts::git_rev_parse_head() {
            let _ = std::fs::write(&scope_file, format!("base_sha = \"{sha}\"\n"));
        }
    }

    pub(crate) fn poll_agent_run(&mut self) {
        let Some(run_id) = self.current_run_id else {
            self.pending_drain_deadline = None;
            return;
        };
        let Some(run) = self
            .state
            .agent_runs
            .iter()
            .find(|run| run.id == run_id)
            .cloned()
        else {
            self.pending_drain_deadline = None;
            return;
        };
        if self.runner_supervisor.run_is_active(run.id) {
            self.pending_drain_deadline = None;
            return;
        }
        
        let timeout = self.stamp_timeout_duration();
        let deadline = *self
            .pending_drain_deadline
            .get_or_insert_with(|| Instant::now() + timeout);
        let now = Instant::now();
        let stamp_path = self.finish_stamp_path_for(&run);
        let stamp_present = std::fs::metadata(&stamp_path).is_ok_and(|meta| meta.is_file() && meta.len() > 0);
        let deadline_elapsed = now >= deadline;
        if !stamp_present && !deadline_elapsed {
            return;
        }
        if !stamp_present && deadline_elapsed && run.stage != "coder" {
            self.append_system_message(
                run.id,
                MessageKind::SummaryWarn,
                format!(
                    "finish_stamp_missing: {} (continuing with existing {} verdict logic)",
                    stamp_path.display(),
                    run.stage
                ),
            );
        }
        self.pending_drain_deadline = None;
        self.run_launched = false;
        self.current_run_id = None;
        let _ = self.finalize_current_run(&run);
    }

    pub(crate) fn stamp_timeout_duration(&self) -> Duration {
        const DEFAULT_STAMP_TIMEOUT_MS: u64 = 1500;
        const ENV_STAMP_TIMEOUT_MS: &str = "CODEXIZE_STAMP_TIMEOUT_MS";
        std::env::var(ENV_STAMP_TIMEOUT_MS)
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|ms| *ms > 0)
            .map_or_else(
                || Duration::from_millis(DEFAULT_STAMP_TIMEOUT_MS),
                Duration::from_millis,
            )
    }

    pub(crate) fn finish_stamp_path_for(&self, run: &crate::state::RunRecord) -> std::path::PathBuf {
        let run_key = self.run_key_for(&run.stage, run.task_id, run.round, run.attempt);
        self.session_dir()
            .join("artifacts")
            .join("run-finish")
            .join(format!("{run_key}.toml"))
    }

    pub(crate) fn run_key_for(&self, stage: &str, task_id: Option<u32>, round: u32, attempt: u32) -> String {
        let task = task_id.map_or_else(|| "stage".to_string(), |id| format!("task-{id}"));
        format!("{stage}-{task}-r{round}-a{attempt}")
    }

    pub(crate) fn finalize_current_run(&mut self, _run: &crate::state::RunRecord) -> Result<()> {
        // Implementation from App, will be moved here.
        Ok(())
    }

    pub(crate) fn replace_state(&mut self, state: SessionState) {
        self.current_run_id = state
            .agent_runs
            .iter()
            .find(|run| run.status == RunStatus::Running)
            .map(|run| run.id);
        self.run_launched = self.current_run_id.is_some();
        self.state = state;
        self.messages = SessionState::load_messages(&self.session_id).unwrap_or_default();
    }

    pub(crate) fn replace_live_summary(&mut self, text: String) {
        self.live_summary_cached_text = crate::app::render::sanitize_live_summary(&text);
    }

    pub(crate) fn append_system_message(&mut self, run_id: u64, kind: MessageKind, text: String) {
        let message = Message {
            ts: chrono::Utc::now(),
            run_id,
            kind,
            sender: MessageSender::System,
            text,
        };
        if let Err(err) = self.state.append_message(&message) {
            let _ = self.state.log_event(format!(
                "failed to append system message for run {run_id}: {err}"
            ));
        } else {
            self.messages.push(message);
        }
    }
}
