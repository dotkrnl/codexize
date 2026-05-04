use crate::state::{Message, MessageKind, MessageSender, RunStatus, SessionState};
use crate::{
    acp::{
        AcpCompletionEvent, AcpConfig, AcpConnector, AcpLaunchPolicy, AcpLaunchRequest,
        AcpRuntimeEvent, AcpTextAccumulator, AcpTextBoundary, PromptPayload, SubprocessConnector,
        ToolCallActivityKind, translate_update,
    },
    adapters::AgentRun,
    selection::VendorKind,
};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Default)]
pub struct ChildLaunch {
    program: String,
    args: Vec<String>,
    envs: Vec<(String, String)>,
    stdin_null: bool,
    stdout_null: bool,
    stderr_null: bool,
}

impl ChildLaunch {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            ..Self::default()
        }
    }

    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.envs.push((key.into(), value.into()));
        self
    }

    pub fn stdin_null(mut self) -> Self {
        self.stdin_null = true;
        self
    }

    pub fn stdout_null(mut self) -> Self {
        self.stdout_null = true;
        self
    }

    pub fn stderr_null(mut self) -> Self {
        self.stderr_null = true;
        self
    }
}

/// Finish stamp written by the runner-owned wrapper after every agent attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinishStamp {
    pub finished_at: String,
    pub exit_code: i32,
    pub head_before: String,
    pub head_after: String,
    pub head_state: String,
    #[serde(default)]
    pub signal_received: String,
    #[serde(default)]
    pub working_tree_clean: bool,
}

/// Atomic write of a finish stamp: write to a temp file in the same directory,
/// then rename into place.
pub fn write_finish_stamp(path: &Path, stamp: &FinishStamp) -> Result<()> {
    let dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    fs::create_dir_all(&dir)?;

    let tmp_path = dir.join(format!(".tmp.{}.toml", std::process::id()));
    let text = toml::to_string_pretty(stamp).context("failed to serialize finish stamp")?;
    fs::write(&tmp_path, text)
        .with_context(|| format!("failed to write temp stamp {}", tmp_path.display()))?;
    fs::rename(&tmp_path, path)
        .with_context(|| format!("failed to rename stamp to {}", path.display()))?;
    Ok(())
}

/// Read and parse a finish stamp from disk.
pub fn read_finish_stamp(path: &Path) -> Result<FinishStamp> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read finish stamp {}", path.display()))?;
    let stamp: FinishStamp = toml::from_str(&text)
        .with_context(|| format!("failed to parse finish stamp {}", path.display()))?;
    Ok(stamp)
}

/// Default stabilization budget in milliseconds.
const DEFAULT_STAMP_STABILIZE_BUDGET_MS: u64 = 1500;
/// Default interval between HEAD reads in milliseconds.
const DEFAULT_STAMP_STABILIZE_INTERVAL_MS: u64 = 100;

/// Environment variable overrides for stabilization timing.
const ENV_STAMP_STABILIZE_MS: &str = "CODEXIZE_STAMP_STABILIZE_MS";
const ENV_STAMP_STABILIZE_INTERVAL_MS: &str = "CODEXIZE_STAMP_STABILIZE_INTERVAL_MS";

const ACP_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcpCancelReason {
    Terminate,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AcpInput {
    Prompt(String),
    Interrupt(String),
}

/// Runner→App tool-call lifecycle transition. The runner stamps
/// `observed_at` at the moment it receives the ACP `session/update` for
/// the transition; the App applies transitions in arrival (timestamp)
/// order so the watchdog idle-clock correctly accounts for tool calls
/// that begin and end between consecutive App polls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallTransition {
    pub tool_call_id: String,
    pub kind: ToolCallActivityKind,
    pub observed_at: Instant,
}

#[derive(Debug)]
struct ManagedAcpRun {
    cancel_tx: mpsc::Sender<AcpCancelReason>,
    input_tx: mpsc::Sender<AcpInput>,
    /// Receives lifecycle transitions emitted by the runner thread. The
    /// App drains this on its main poll cadence and applies them to the
    /// per-run watchdog state. Held inside the mutex-protected
    /// `active_acp_runs()` map, so consumers must lock before draining.
    tool_call_transition_rx: mpsc::Receiver<ToolCallTransition>,
    finished: std::sync::Arc<AtomicBool>,
    waiting_for_input: std::sync::Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone)]
struct ManagedAcpLaunch {
    resolved: crate::acp::AcpResolvedLaunch,
    window_name: String,
    session_id: Option<String>,
    stamp_path: PathBuf,
    cause_path: PathBuf,
    required_artifact: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct ManagedAcpOutcome {
    exit_code: i32,
    signal_received: String,
}

fn active_acp_runs() -> &'static Mutex<std::collections::HashMap<String, ManagedAcpRun>> {
    static ACTIVE: OnceLock<Mutex<std::collections::HashMap<String, ManagedAcpRun>>> =
        OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
fn test_input_receivers()
-> &'static Mutex<std::collections::HashMap<String, mpsc::Receiver<AcpInput>>> {
    static RECEIVERS: OnceLock<Mutex<std::collections::HashMap<String, mpsc::Receiver<AcpInput>>>> =
        OnceLock::new();
    RECEIVERS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
fn test_cancel_receivers()
-> &'static Mutex<std::collections::HashMap<String, mpsc::Receiver<AcpCancelReason>>> {
    static RECEIVERS: OnceLock<
        Mutex<std::collections::HashMap<String, mpsc::Receiver<AcpCancelReason>>>,
    > = OnceLock::new();
    RECEIVERS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

#[allow(clippy::too_many_arguments)]
fn build_managed_acp_launch(
    window_name: &str,
    vendor: VendorKind,
    run: &AgentRun,
    run_key: &str,
    artifacts_dir: &Path,
    required_artifact: Option<&Path>,
    interactive: bool,
    policy: AcpLaunchPolicy,
) -> Result<ManagedAcpLaunch> {
    let cwd = std::env::current_dir().context("failed to capture launch cwd")?;
    let request = AcpLaunchRequest {
        vendor,
        cwd,
        prompt: PromptPayload::File(run.prompt_path.clone()),
        model: run.model.clone(),
        // The current launch sites already pass the codexize-computed effective
        // effort. Task 2 keeps artifact/finalization ownership in codexize and
        // defers the requested-vs-effective UI split to the later ACP UX work.
        requested_effort: run.effort,
        effective_effort: run.effort,
        interactive,
        modes: run.modes,
        required_artifacts: required_artifact
            .into_iter()
            .map(Path::to_path_buf)
            .collect(),
        policy,
    };
    let mut resolved = AcpConfig::default()
        .resolve(&request)
        .map_err(|err| anyhow!("{err}"))?;
    ensure_program_exists(&resolved.spawn.program)?;
    let cause_path = artifacts_dir
        .join("run-finish")
        .join(format!("{run_key}.cause.txt"));
    resolved.session.metadata.insert(
        "codexize.acp_trace_path".to_string(),
        acp_trace_path_from_cause_path(&cause_path)
            .display()
            .to_string(),
    );

    Ok(ManagedAcpLaunch {
        resolved,
        window_name: window_name.to_string(),
        session_id: session_id_from_artifacts_dir(artifacts_dir),
        stamp_path: artifacts_dir
            .join("run-finish")
            .join(format!("{run_key}.toml")),
        // Keep transport-boundary diagnostics adjacent to finish stamps so
        // postmortems can inspect one per-run directory.
        cause_path,
        required_artifact: required_artifact.map(Path::to_path_buf),
    })
}

fn ensure_program_exists(program: &str) -> Result<()> {
    if crate::acp::program_is_executable(program) {
        Ok(())
    } else {
        bail!("ACP agent CLI not found — install it first");
    }
}

fn session_id_from_artifacts_dir(artifacts_dir: &Path) -> Option<String> {
    artifacts_dir
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(str::to_string)
}

fn write_launch_cause(path: &Path, cause: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create cause dir {}", parent.display()))?;
    }
    fs::write(path, cause).with_context(|| format!("failed to write cause {}", path.display()))
}

fn append_launch_cause(path: &Path, cause: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let existing = fs::read_to_string(path).unwrap_or_default();
    let text = if existing.is_empty() {
        cause.to_string()
    } else {
        format!("{existing}\n{cause}")
    };
    let _ = fs::write(path, text);
}

fn acp_trace_path_from_cause_path(cause_path: &Path) -> PathBuf {
    let Some(file_name) = cause_path.file_name().and_then(|name| name.to_str()) else {
        return cause_path.with_extension("acp.jsonl");
    };
    let trace_name = file_name
        .strip_suffix(".cause.txt")
        .map(|stem| format!("{stem}.acp.jsonl"))
        .unwrap_or_else(|| format!("{file_name}.acp.jsonl"));
    cause_path.with_file_name(trace_name)
}

fn acp_text_trace_path(launch: &ManagedAcpLaunch) -> PathBuf {
    acp_trace_path_from_cause_path(&launch.cause_path)
}

fn append_acp_text_trace(launch: &ManagedAcpLaunch, event: &crate::acp::AcpTextEvent) {
    let path = acp_text_trace_path(launch);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let record = serde_json::json!({
        "type": "text_event",
        "ts": chrono::Utc::now().to_rfc3339(),
        "stream": if event.thought { "thought" } else { "agent" },
        "interactive": event.interactive,
        "boundary": format!("{:?}", event.boundary),
        "identity": event.identity,
        "text": event.text,
    });
    let Ok(line) = serde_json::to_string(&record) else {
        return;
    };
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

fn git_rev_parse_head() -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_string())
}

fn working_tree_clean() -> bool {
    Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| output.stdout.is_empty())
        .unwrap_or(false)
}

fn git_status_porcelain() -> Result<String> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .context("failed to run git status --porcelain")?;
    if !output.status.success() {
        bail!("git status --porcelain failed with exit {}", output.status);
    }
    String::from_utf8(output.stdout).context("git status --porcelain emitted non-UTF-8 output")
}

fn stamp_stabilize_budget() -> Duration {
    std::env::var(ENV_STAMP_STABILIZE_MS)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_STAMP_STABILIZE_BUDGET_MS))
}

fn stamp_stabilize_interval() -> Duration {
    std::env::var(ENV_STAMP_STABILIZE_INTERVAL_MS)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_STAMP_STABILIZE_INTERVAL_MS))
}

fn wait_for_stable_head() -> (String, String) {
    let budget = stamp_stabilize_budget();
    let interval = stamp_stabilize_interval();
    let deadline = Instant::now() + budget;

    loop {
        let lock_path = Path::new(".git").join("index.lock");
        while lock_path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }

        let first = git_rev_parse_head().unwrap_or_default();
        thread::sleep(interval);
        let second = git_rev_parse_head().unwrap_or_default();
        if first == second {
            return (second, "stable".to_string());
        }
        if Instant::now() >= deadline {
            return (second, "unstable".to_string());
        }
    }
}

fn find_transcript_run(session_id: &str, window_name: &str) -> Option<(u64, String, String)> {
    let state = SessionState::load(session_id).ok()?;
    state
        .agent_runs
        .iter()
        .rev()
        .find(|run| run.window_name == window_name && run.status == RunStatus::Running)
        .or_else(|| {
            state
                .agent_runs
                .iter()
                .rev()
                .find(|run| run.window_name == window_name)
        })
        .map(|run| (run.id, run.model.clone(), run.vendor.clone()))
}

fn persist_agent_text_block(
    launch: &ManagedAcpLaunch,
    text: String,
    kind: MessageKind,
) -> Option<chrono::DateTime<chrono::Utc>> {
    if text.is_empty() {
        return None;
    }
    let session_id = launch.session_id.as_deref()?;

    // ACP output can arrive before the app thread finishes saving the run
    // record, so transcript persistence waits briefly for that handoff.
    let run = (0..80).find_map(|_| {
        let found = find_transcript_run(session_id, &launch.window_name);
        if found.is_none() {
            thread::sleep(Duration::from_millis(25));
        }
        found
    });
    let Some((run_id, model, vendor)) = run else {
        append_launch_cause(
            &launch.cause_path,
            "failed to persist ACP text: run record was not available",
        );
        return None;
    };

    let ts = chrono::Utc::now();
    let msg = Message {
        ts,
        run_id,
        kind,
        sender: MessageSender::Agent { model, vendor },
        text,
    };
    if let Err(err) = SessionState::load(session_id).and_then(|state| state.append_message(&msg)) {
        append_launch_cause(
            &launch.cause_path,
            &format!("failed to persist ACP text for run {run_id}: {err:#}"),
        );
        return None;
    }
    Some(ts)
}

fn update_agent_text_block(
    launch: &ManagedAcpLaunch,
    ts: chrono::DateTime<chrono::Utc>,
    text: &str,
) -> bool {
    let Some(session_id) = launch.session_id.as_deref() else {
        return false;
    };
    match SessionState::load(session_id).and_then(|state| state.update_message_text(ts, text)) {
        Ok(true) => true,
        Ok(false) => {
            append_launch_cause(
                &launch.cause_path,
                "failed to update live ACP text: message was not available",
            );
            false
        }
        Err(err) => {
            append_launch_cause(
                &launch.cause_path,
                &format!("failed to update live ACP text: {err:#}"),
            );
            false
        }
    }
}

struct AcpTextStream {
    accumulator: AcpTextAccumulator,
    live_ts: Option<chrono::DateTime<chrono::Utc>>,
}

impl AcpTextStream {
    fn new() -> Self {
        Self {
            accumulator: AcpTextAccumulator::new(),
            live_ts: None,
        }
    }

    #[cfg(test)]
    fn push_text(&mut self, launch: &ManagedAcpLaunch, chunk: &str, kind: MessageKind) {
        self.push_text_boundary(launch, chunk, kind, AcpTextBoundary::Continue);
    }

    fn push_text_boundary(
        &mut self,
        launch: &ManagedAcpLaunch,
        chunk: &str,
        kind: MessageKind,
        boundary: AcpTextBoundary,
    ) {
        if boundary == AcpTextBoundary::StartNewMessage {
            // ACP only emits Continue when stable identity proves continuity;
            // otherwise this intentionally over-splits rather than rewriting
            // an unrelated previous live message.
            self.finish_turn(launch, kind);
            self.live_ts = None;
        }
        if let Some(text) = self.accumulator.push(chunk) {
            self.persist_ready(launch, text, kind);
        }
        while let Some(text) = self.accumulator.next_ready() {
            self.persist_ready(launch, text, kind);
        }
        if let Some(text) = self.accumulator.current_text().map(str::to_string) {
            self.persist_live(launch, &text, kind);
        }
    }

    fn finish_turn(&mut self, launch: &ManagedAcpLaunch, kind: MessageKind) {
        while let Some(text) = self.accumulator.finish_prompt_turn() {
            self.persist_ready(launch, text, kind);
        }
    }

    fn persist_ready(&mut self, launch: &ManagedAcpLaunch, text: String, kind: MessageKind) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if let Some(ts) = self.live_ts.take()
            && update_agent_text_block(launch, ts, &text)
        {
            return;
        }
        let _ = persist_agent_text_block(launch, text, kind);
    }

    fn persist_live(&mut self, launch: &ManagedAcpLaunch, text: &str, kind: MessageKind) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if let Some(ts) = self.live_ts
            && update_agent_text_block(launch, ts, text)
        {
            return;
        }
        self.live_ts = persist_agent_text_block(launch, text.to_string(), kind);
    }
}

fn write_finish_stamp_for_outcome(
    stamp_path: &Path,
    head_before: String,
    outcome: &ManagedAcpOutcome,
) -> Result<()> {
    let (head_after, head_state) = wait_for_stable_head();
    let stamp = FinishStamp {
        finished_at: chrono::Utc::now().to_rfc3339(),
        exit_code: outcome.exit_code,
        head_before,
        head_after,
        head_state,
        signal_received: outcome.signal_received.clone(),
        working_tree_clean: working_tree_clean(),
    };
    write_finish_stamp(stamp_path, &stamp)
}

fn enforce_readonly_workspace_policy(
    launch: &ManagedAcpLaunch,
    head_before: &str,
    git_status_before: Option<&str>,
) -> Result<()> {
    if !launch.resolved.session.policy.enforce_readonly_workspace {
        return Ok(());
    }

    let head_after = git_rev_parse_head().unwrap_or_default();
    if head_after != head_before {
        bail!(
            "ACP launch violated read-only workspace policy: HEAD changed from {head_before} to {head_after}"
        );
    }

    let Some(git_status_before) = git_status_before else {
        bail!("ACP launch violated read-only workspace policy: missing pre-run git status");
    };
    let git_status_after = git_status_porcelain()?;
    if git_status_after != git_status_before {
        bail!(
            "ACP launch violated read-only workspace policy: git status changed from {:?} to {:?}",
            git_status_before,
            git_status_after
        );
    }

    Ok(())
}

fn run_managed_acp_launch(
    launch: ManagedAcpLaunch,
    cancel_rx: mpsc::Receiver<AcpCancelReason>,
    input_rx: mpsc::Receiver<AcpInput>,
    transition_tx: mpsc::Sender<ToolCallTransition>,
    waiting_for_input: std::sync::Arc<AtomicBool>,
) -> Result<ManagedAcpOutcome> {
    let head_before = git_rev_parse_head().unwrap_or_default();
    // Final validation is allowed to update its ignored artifact files, so the
    // ACP-side write allowlist carries those exact paths while the runner
    // enforces that no git-visible workspace state changes during the run.
    let git_status_before = launch
        .resolved
        .session
        .policy
        .enforce_readonly_workspace
        .then(git_status_porcelain)
        .transpose()?;
    let connector = SubprocessConnector;
    let mut session = connector
        .connect(&launch.resolved)
        .map_err(|err| anyhow!("{err}"))?;
    let mut agent_text = AcpTextStream::new();
    let mut thought_text = AcpTextStream::new();
    let mut pending_input = VecDeque::new();
    let mut waiting_for_interactive_prompt = false;
    let mut interrupting_turn = false;

    let outcome = loop {
        if let Ok(reason) = cancel_rx.try_recv() {
            waiting_for_input.store(false, Ordering::SeqCst);
            thought_text.finish_turn(&launch, MessageKind::AgentThought);
            agent_text.finish_turn(&launch, MessageKind::AgentText);
            session.close().map_err(|err| anyhow!("{err}"))?;
            match reason {
                AcpCancelReason::Terminate => {
                    break ManagedAcpOutcome {
                        exit_code: 143,
                        signal_received: "TERM".to_string(),
                    };
                }
                AcpCancelReason::Complete => {
                    if let Some(path) = launch.required_artifact.as_deref() {
                        validate_toml_artifacts(&[path])?;
                    }
                    break ManagedAcpOutcome {
                        exit_code: 0,
                        signal_received: String::new(),
                    };
                }
            }
        }

        while let Ok(input) = input_rx.try_recv() {
            match input {
                AcpInput::Prompt(text) => pending_input.push_back(text),
                AcpInput::Interrupt(text) => {
                    pending_input.push_back(text);
                    if !waiting_for_interactive_prompt && !interrupting_turn {
                        session.cancel_prompt().map_err(|err| anyhow!("{err}"))?;
                        interrupting_turn = true;
                        waiting_for_input.store(false, Ordering::SeqCst);
                    }
                }
            }
        }

        if waiting_for_interactive_prompt && let Some(text) = pending_input.pop_front() {
            waiting_for_input.store(false, Ordering::SeqCst);
            session
                .submit_prompt(&text)
                .map_err(|err| anyhow!("{err}"))?;
            waiting_for_interactive_prompt = false;
            interrupting_turn = false;
        }

        let event = session
            .try_next_update()
            .map_err(|err| anyhow!("{err}"))?
            .and_then(|update| translate_update(update, launch.resolved.interactive));

        match event {
            Some(AcpRuntimeEvent::Completion(AcpCompletionEvent::PromptTurnFinished)) => {
                thought_text.finish_turn(&launch, MessageKind::AgentThought);
                agent_text.finish_turn(&launch, MessageKind::AgentText);
                if launch.resolved.interactive {
                    if let Some(text) = pending_input.pop_front() {
                        waiting_for_input.store(false, Ordering::SeqCst);
                        session
                            .submit_prompt(&text)
                            .map_err(|err| anyhow!("{err}"))?;
                        waiting_for_interactive_prompt = false;
                        interrupting_turn = false;
                    } else {
                        waiting_for_interactive_prompt = true;
                        interrupting_turn = false;
                        waiting_for_input.store(true, Ordering::SeqCst);
                    }
                    thread::sleep(ACP_POLL_INTERVAL);
                    continue;
                }
                waiting_for_input.store(false, Ordering::SeqCst);
                session.close().map_err(|err| anyhow!("{err}"))?;
                if let Some(path) = launch.required_artifact.as_deref() {
                    validate_toml_artifacts(&[path])?;
                }
                break ManagedAcpOutcome {
                    exit_code: 0,
                    signal_received: String::new(),
                };
            }
            Some(AcpRuntimeEvent::Completion(AcpCompletionEvent::PromptTurnFailed { .. })) => {
                thought_text.finish_turn(&launch, MessageKind::AgentThought);
                agent_text.finish_turn(&launch, MessageKind::AgentText);
                if launch.resolved.interactive && interrupting_turn {
                    if let Some(text) = pending_input.pop_front() {
                        waiting_for_input.store(false, Ordering::SeqCst);
                        session
                            .submit_prompt(&text)
                            .map_err(|err| anyhow!("{err}"))?;
                        waiting_for_interactive_prompt = false;
                        interrupting_turn = false;
                    } else {
                        waiting_for_interactive_prompt = true;
                        interrupting_turn = false;
                        waiting_for_input.store(true, Ordering::SeqCst);
                    }
                    thread::sleep(ACP_POLL_INTERVAL);
                    continue;
                }
                waiting_for_input.store(false, Ordering::SeqCst);
                session.close().map_err(|err| anyhow!("{err}"))?;
                break ManagedAcpOutcome {
                    exit_code: 1,
                    signal_received: String::new(),
                };
            }
            Some(AcpRuntimeEvent::Text(text_event)) => {
                append_acp_text_trace(&launch, &text_event);
                let text = text_event.text;
                if text_event.thought {
                    thought_text.push_text_boundary(
                        &launch,
                        &text,
                        MessageKind::AgentThought,
                        text_event.boundary,
                    );
                } else {
                    agent_text.push_text_boundary(
                        &launch,
                        &text,
                        MessageKind::AgentText,
                        text_event.boundary,
                    );
                }
                thread::sleep(ACP_POLL_INTERVAL)
            }
            Some(AcpRuntimeEvent::ToolCallActivity { tool_call_id, kind }) => {
                // Stamp `observed_at` at the moment the runner saw the
                // transition. The send is best-effort: if the receiver has
                // been dropped (e.g. App teardown), a missing pause/resume
                // signal is preferable to crashing the runner.
                let _ = transition_tx.send(ToolCallTransition {
                    tool_call_id,
                    kind,
                    observed_at: Instant::now(),
                });
            }
            Some(AcpRuntimeEvent::Lifecycle(_)) | None => thread::sleep(ACP_POLL_INTERVAL),
        }
    };

    enforce_readonly_workspace_policy(&launch, &head_before, git_status_before.as_deref())?;
    write_finish_stamp_for_outcome(&launch.stamp_path, head_before, &outcome)?;
    Ok(outcome)
}

fn finalize_managed_acp_launch(
    launch: ManagedAcpLaunch,
    cancel_rx: mpsc::Receiver<AcpCancelReason>,
    input_rx: mpsc::Receiver<AcpInput>,
    transition_tx: mpsc::Sender<ToolCallTransition>,
    waiting_for_input: std::sync::Arc<AtomicBool>,
) {
    match run_managed_acp_launch(
        launch.clone(),
        cancel_rx,
        input_rx,
        transition_tx,
        std::sync::Arc::clone(&waiting_for_input),
    ) {
        Ok(_) => {
            waiting_for_input.store(false, Ordering::SeqCst);
            let _ = fs::remove_file(&launch.cause_path);
            return;
        }
        Err(err) => {
            waiting_for_input.store(false, Ordering::SeqCst);
            let _ = write_launch_cause(&launch.cause_path, &format!("{err:#}"));
        }
    }

    let fallback_head_before = git_rev_parse_head().unwrap_or_default();
    let outcome = ManagedAcpOutcome {
        exit_code: 1,
        signal_received: String::new(),
    };
    let _ = write_finish_stamp_for_outcome(&launch.stamp_path, fallback_head_before, &outcome);
}

fn cleanup_finished_acp_runs() {
    let mut finished = Vec::new();
    {
        let guard = active_acp_runs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (window_name, run) in guard.iter() {
            if run.finished.load(Ordering::SeqCst) {
                finished.push(window_name.clone());
            }
        }
    }
    for window_name in finished {
        if let Some(mut run) = take_managed_acp_run(&window_name)
            && let Some(handle) = run.join.take()
        {
            let _ = handle.join();
        }
    }
}

fn take_managed_acp_run(window_name: &str) -> Option<ManagedAcpRun> {
    #[cfg(test)]
    {
        test_input_receivers()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(window_name);
    }
    active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(window_name)
}

fn launch_managed_acp_window(window_name: &str, launch: ManagedAcpLaunch) -> Result<()> {
    cleanup_finished_acp_runs();

    let mut guard = active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard
        .values()
        .any(|run| !run.finished.load(Ordering::SeqCst))
    {
        bail!("codexize only supports one active ACP run at a time");
    }

    let (cancel_tx, cancel_rx) = mpsc::channel();
    let (input_tx, input_rx) = mpsc::channel();
    let (transition_tx, transition_rx) = mpsc::channel();
    let finished = std::sync::Arc::new(AtomicBool::new(false));
    let waiting_for_input = std::sync::Arc::new(AtomicBool::new(false));
    let finished_flag = std::sync::Arc::clone(&finished);
    let waiting_for_input_flag = std::sync::Arc::clone(&waiting_for_input);
    let launch_window = window_name.to_string();
    let handle = thread::spawn(move || {
        finalize_managed_acp_launch(
            launch,
            cancel_rx,
            input_rx,
            transition_tx,
            waiting_for_input_flag,
        );
        finished_flag.store(true, Ordering::SeqCst);
    });
    guard.insert(
        launch_window,
        ManagedAcpRun {
            cancel_tx,
            input_tx,
            tool_call_transition_rx: transition_rx,
            finished,
            waiting_for_input,
            join: Some(handle),
        },
    );
    Ok(())
}

pub fn run_label_is_active(window_name: &str) -> bool {
    cleanup_finished_acp_runs();
    active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(window_name)
        .is_some_and(|run| !run.finished.load(Ordering::SeqCst))
}

pub fn run_label_is_waiting_for_input(window_name: &str) -> bool {
    cleanup_finished_acp_runs();
    active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(window_name)
        .is_some_and(|run| {
            !run.finished.load(Ordering::SeqCst) && run.waiting_for_input.load(Ordering::SeqCst)
        })
}

#[cfg(test)]
pub fn request_run_label_interactive_input_for_test(window_name: &str) {
    request_run_label_for_test(window_name, true);
}

#[cfg(test)]
pub fn request_run_label_active_for_test(window_name: &str) {
    request_run_label_for_test(window_name, false);
}

/// Test-only: drain queued `AcpInput` messages on the per-window test
/// receiver registered by `request_run_label_*_for_test`. Returns each
/// queued input as a stable `(kind, text)` pair so callers do not need
/// access to the private `AcpInput` enum.
#[cfg(test)]
pub fn drain_test_input_receiver_for(window_name: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let map = test_input_receivers();
    let guard = map.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(rx) = guard.get(window_name) {
        while let Ok(input) = rx.try_recv() {
            match input {
                AcpInput::Prompt(text) => out.push(("prompt", text)),
                AcpInput::Interrupt(text) => out.push(("interrupt", text)),
            }
        }
    }
    out
}

/// Test-only: drain queued `AcpCancelReason` messages on the per-window
/// test receiver. Returns each reason as a stable string so callers do
/// not need access to the private `AcpCancelReason` enum.
#[cfg(test)]
pub fn drain_test_cancel_receiver_for(window_name: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    let map = test_cancel_receivers();
    let guard = map.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(rx) = guard.get(window_name) {
        while let Ok(reason) = rx.try_recv() {
            out.push(match reason {
                AcpCancelReason::Terminate => "terminate",
                AcpCancelReason::Complete => "complete",
            });
        }
    }
    out
}

#[cfg(test)]
fn request_run_label_for_test(window_name: &str, waiting: bool) {
    let mut guard = active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (cancel_tx, cancel_rx) = mpsc::channel();
    let (input_tx, input_rx) = mpsc::channel();
    let (_transition_tx, transition_rx) = mpsc::channel();
    let finished = std::sync::Arc::new(AtomicBool::new(false));
    let waiting_for_input = std::sync::Arc::new(AtomicBool::new(waiting));
    test_input_receivers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(window_name.to_string(), input_rx);
    test_cancel_receivers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(window_name.to_string(), cancel_rx);
    guard.insert(
        window_name.to_string(),
        ManagedAcpRun {
            cancel_tx,
            input_tx,
            tool_call_transition_rx: transition_rx,
            finished,
            waiting_for_input,
            join: None,
        },
    );
}

pub fn cancel_run_labels_matching(base: &str) {
    let prefix = format!("{base} ");
    let matching = {
        let guard = active_acp_runs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard
            .keys()
            .filter(|name| *name == base || name.starts_with(&prefix))
            .cloned()
            .collect::<Vec<_>>()
    };

    for window_name in matching {
        if let Some(mut run) = take_managed_acp_run(&window_name) {
            let _ = run.cancel_tx.send(AcpCancelReason::Terminate);
            if let Some(handle) = run.join.take() {
                let _ = handle.join();
            }
        }
    }
}

pub fn request_run_label_exit(window_name: &str) {
    if let Some(mut run) = take_managed_acp_run(window_name) {
        let _ = run.cancel_tx.send(AcpCancelReason::Complete);
        if let Some(handle) = run.join.take() {
            let _ = handle.join();
        }
    }
}

pub fn send_run_label_input(window_name: &str, text: String) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    cleanup_finished_acp_runs();
    let guard = active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .get(window_name)
        .filter(|run| {
            !run.finished.load(Ordering::SeqCst) && run.waiting_for_input.load(Ordering::SeqCst)
        })
        .is_some_and(|run| run.input_tx.send(AcpInput::Prompt(text)).is_ok())
}

pub fn interrupt_run_label_input(window_name: &str, text: String) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    cleanup_finished_acp_runs();
    let guard = active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.get(window_name).is_some_and(|run| {
        if run.finished.load(Ordering::SeqCst) {
            return false;
        }
        let input = if run.waiting_for_input.load(Ordering::SeqCst) {
            AcpInput::Prompt(text)
        } else {
            AcpInput::Interrupt(text)
        };
        run.input_tx.send(input).is_ok()
    })
}

/// Push an `AcpInput::Interrupt(text)` onto the run's input channel
/// regardless of whether the runner reports it is waiting for input. Used
/// by the watchdog warning path (spec §3.4) where the spec requires
/// cancelling the in-flight ACP turn and queueing the warning as the next
/// prompt — converting to `Prompt` (as `interrupt_run_label_input` would
/// when waiting) would skip the cancel_prompt() side effect.
pub fn force_interrupt_run_label(window_name: &str, text: String) -> bool {
    if text.is_empty() {
        return false;
    }
    cleanup_finished_acp_runs();
    let guard = active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.get(window_name).is_some_and(|run| {
        if run.finished.load(Ordering::SeqCst) {
            return false;
        }
        run.input_tx.send(AcpInput::Interrupt(text)).is_ok()
    })
}

/// Best-effort `AcpCancelReason::Terminate` for the named run (spec §3.5).
/// Unlike `cancel_run_labels_matching`, this does not remove the run from
/// the active map or join the runner thread — the existing
/// `poll_agent_run` finalize path observes `!active_run_exists` once the
/// runner thread exits and routes the non-zero exit through the standard
/// failed-run vendor failover. Returns `false` if no such run is active.
pub fn terminate_run_label(window_name: &str) -> bool {
    cleanup_finished_acp_runs();
    let guard = active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.get(window_name).is_some_and(|run| {
        if run.finished.load(Ordering::SeqCst) {
            return false;
        }
        run.cancel_tx.send(AcpCancelReason::Terminate).is_ok()
    })
}

/// Drain all queued tool-call lifecycle transitions across every managed
/// ACP run currently active. Returned in `(window_name, transition)` pairs
/// in arrival order per run; cross-run interleaving is preserved by the
/// timestamp on each transition. Callers should apply transitions in
/// `observed_at` order.
pub fn drain_tool_call_transitions() -> Vec<(String, ToolCallTransition)> {
    let guard = active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut out = Vec::new();
    for (window_name, run) in guard.iter() {
        while let Ok(transition) = run.tool_call_transition_rx.try_recv() {
            out.push((window_name.clone(), transition));
        }
    }
    out
}

pub fn shutdown_all_runs() {
    let runs = {
        let mut guard = active_acp_runs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::take(&mut *guard)
            .into_values()
            .collect::<Vec<_>>()
    };
    #[cfg(test)]
    {
        test_input_receivers()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        test_cancel_receivers()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    for mut run in runs {
        let _ = run.cancel_tx.send(AcpCancelReason::Terminate);
        if let Some(handle) = run.join.take() {
            let _ = handle.join();
        }
    }
}

/// Validate that all required TOML artifacts exist and are parseable.
/// Missing or malformed artifacts signal an incomplete agent turn; the
/// orchestrator should retry the agent execution phase.
pub fn validate_toml_artifacts(paths: &[&Path]) -> Result<()> {
    let mut errors = Vec::new();
    for path in paths {
        if !path.exists() {
            errors.push(format!("missing: {}", path.display()));
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            let text = fs::read_to_string(path)
                .with_context(|| format!("cannot read {}", path.display()))?;
            if let Err(e) = toml::from_str::<toml::Value>(&text) {
                errors.push(format!("malformed TOML in {}: {e}", path.display()));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!(
            "incomplete agent turn — artifact validation failed:\n{}",
            errors.join("\n")
        )
    }
}

/// Launch an agent interactively inside the managed ACP runtime boundary.
/// All agent child-process launches must route through the runner so that
/// finish-stamp logic is guaranteed to run.
#[allow(clippy::too_many_arguments)]
pub fn launch_interactive(
    window_name: &str,
    run: &AgentRun,
    vendor: VendorKind,
    run_key: &str,
    artifacts_dir: &Path,
    required_artifact: Option<&Path>,
) -> Result<()> {
    let launch = build_managed_acp_launch(
        window_name,
        vendor,
        run,
        run_key,
        artifacts_dir,
        required_artifact,
        true,
        AcpLaunchPolicy::default(),
    )?;
    launch_managed_acp_window(window_name, launch)
}

/// Launch an agent non-interactively inside the managed ACP runtime boundary.
/// All agent child-process launches must route through the runner so that
/// finish-stamp logic is guaranteed to run.
pub fn launch_noninteractive(
    window_name: &str,
    run: &AgentRun,
    vendor: VendorKind,
    run_key: &str,
    artifacts_dir: &Path,
    required_artifact: Option<&Path>,
) -> Result<()> {
    let launch = build_managed_acp_launch(
        window_name,
        vendor,
        run,
        run_key,
        artifacts_dir,
        required_artifact,
        false,
        AcpLaunchPolicy::default(),
    )?;
    launch_managed_acp_window(window_name, launch)
}

pub fn launch_noninteractive_with_policy(
    window_name: &str,
    run: &AgentRun,
    vendor: VendorKind,
    run_key: &str,
    artifacts_dir: &Path,
    required_artifact: Option<&Path>,
    policy: AcpLaunchPolicy,
) -> Result<()> {
    let launch = build_managed_acp_launch(
        window_name,
        vendor,
        run,
        run_key,
        artifacts_dir,
        required_artifact,
        false,
        policy,
    )?;
    launch_managed_acp_window(window_name, launch)
}

pub fn run_child_with_timeout(
    launch: &ChildLaunch,
    timeout: Duration,
) -> Result<Option<ExitStatus>> {
    let mut command = Command::new(&launch.program);
    command.args(&launch.args);
    for (key, value) in &launch.envs {
        command.env(key, value);
    }
    if launch.stdin_null {
        command.stdin(Stdio::null());
    }
    if launch.stdout_null {
        command.stdout(Stdio::null());
    }
    if launch.stderr_null {
        command.stderr(Stdio::null());
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn: {:?}", launch))?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            child.kill()?;
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests_mod;
