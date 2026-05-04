use super::{
    AcpError, AcpResolvedLaunch, AcpResult, AcpTextBoundary, ClientUpdate, PromptPayload,
    ToolCallActivityKind,
    tool_call::{
        ToolCallDisplayState, ToolCallMap, ToolCallPayload, format_invocation_line,
        format_result_line, is_terminal_status,
    },
};
use crate::selection::vendor::vendor_kind_to_str;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{BufRead, BufReader, ErrorKind, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
};

type PendingRequests = Arc<Mutex<BTreeMap<u64, Sender<AcpResult<Value>>>>>;
type SharedWriter = Arc<Mutex<Option<ChildStdin>>>;

pub trait AcpSession: Send {
    fn session_id(&self) -> &str;
    fn try_next_update(&mut self) -> AcpResult<Option<ClientUpdate>>;
    fn submit_prompt(&mut self, text: &str) -> AcpResult<()>;
    fn cancel_prompt(&mut self) -> AcpResult<()>;
    fn close(&mut self) -> AcpResult<()>;
}

pub trait AcpConnector {
    fn connect(&self, launch: &AcpResolvedLaunch) -> AcpResult<Box<dyn AcpSession>>;
}

trait RpcCaller {
    fn call(&mut self, method: &str, params: Value) -> AcpResult<Value>;
}

#[derive(Debug, Clone, Default)]
pub struct SubprocessConnector;

impl AcpConnector for SubprocessConnector {
    fn connect(&self, launch: &AcpResolvedLaunch) -> AcpResult<Box<dyn AcpSession>> {
        let mut command = Command::new(&launch.spawn.program);
        command
            .args(&launch.spawn.args)
            .envs(&launch.spawn.env)
            .current_dir(&launch.session.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Keep stderr from backing up an unread pipe; protocol diagnostics
            // flow through ACP updates and request failures.
            .stderr(Stdio::null());

        let mut child = command.spawn().map_err(|err| {
            AcpError::human_block(format!(
                "ACP agent for vendor {} failed to start ({}): {err}",
                vendor_kind_to_str(launch.vendor),
                launch.spawn.program
            ))
        })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AcpError::protocol("ACP child stdout was not captured"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AcpError::protocol("ACP child stdin was not captured"))?;
        let mut rpc = RpcPeer::new(stdin, stdout);
        let initialize = match rpc.call(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": {
                        "readTextFile": false,
                        "writeTextFile": false
                    },
                    "terminal": false
                },
                "clientInfo": {
                    "name": "codexize",
                    "title": "codexize",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        ) {
            Ok(value) => value,
            Err(err) => return cleanup_failed_connect(child, rpc, err),
        };
        let init = match parse_initialize_result(initialize) {
            Ok(init) => init,
            Err(err) => return cleanup_failed_connect(child, rpc, err),
        };
        if init.protocol_version != 1 {
            return cleanup_failed_connect(
                child,
                rpc,
                AcpError::human_block(format!(
                    "ACP agent negotiated unsupported protocol version {}",
                    init.protocol_version
                )),
            );
        }

        let new_session = match rpc.call(
            "session/new",
            json!({
                "cwd": launch.session.cwd,
                "mcpServers": []
            }),
        ) {
            Ok(value) => value,
            Err(err) => return cleanup_failed_connect(child, rpc, err),
        };
        let mut session = match parse_new_session_result(new_session) {
            Ok(session) => session,
            Err(err) => return cleanup_failed_connect(child, rpc, err),
        };
        if let Err(err) = apply_session_config(
            &mut rpc,
            &session.session_id,
            &launch.session,
            &mut session.config_options,
        ) {
            return cleanup_failed_connect(child, rpc, err);
        }
        let prompt_params = match prompt_request_params(&session.session_id, &launch.session.prompt)
        {
            Ok(params) => params,
            Err(err) => return cleanup_failed_connect(child, rpc, err),
        };
        let prompt_response = match rpc.start_request("session/prompt", prompt_params) {
            Ok(response) => response,
            Err(err) => return cleanup_failed_connect(child, rpc, err),
        };

        Ok(Box::new(SubprocessSession {
            session_id: session.session_id,
            rpc,
            child: Some(child),
            supports_close: init.supports_close,
            prompt_response: Some(prompt_response),
            prompt_finished: false,
            closed: false,
            cwd: launch.session.cwd.clone(),
            tool_calls: ToolCallMap::new(),
            boundary_state: AcpBoundaryState::new(),
            pending_updates: VecDeque::new(),
            acp_trace_path: launch
                .session
                .metadata
                .get("codexize.acp_trace_path")
                .map(PathBuf::from),
        }))
    }
}

fn cleanup_failed_connect(
    mut child: Child,
    mut rpc: RpcPeer,
    err: AcpError,
) -> AcpResult<Box<dyn AcpSession>> {
    let _ = child.kill();
    let _ = child.wait();
    rpc.shutdown();
    Err(err)
}

struct SubprocessSession {
    session_id: String,
    rpc: RpcPeer,
    child: Option<Child>,
    supports_close: bool,
    prompt_response: Option<Receiver<AcpResult<Value>>>,
    prompt_finished: bool,
    closed: bool,
    cwd: PathBuf,
    tool_calls: ToolCallMap,
    boundary_state: AcpBoundaryState,
    pending_updates: VecDeque<ClientUpdate>,
    acp_trace_path: Option<PathBuf>,
}

/// Per-stream identity + restart-flag tracking used to classify text chunks
/// as `Continue` vs. `StartNewMessage`.
///
/// `last_identity` retains the most recent stable ACP message id observed for
/// the stream, when the payload exposes one. `restart_pending` is set at
/// every explicit boundary (session start, prompt-turn reset, tool-call
/// interleave) and forces the next chunk on the stream to be classified as
/// `StartNewMessage`. Once a chunk is classified, the flag is cleared, so
/// subsequent chunks default to `Continue` until the next explicit boundary.
#[derive(Debug, Clone)]
struct StreamIdentity {
    last_identity: Option<String>,
    restart_pending: bool,
}

impl StreamIdentity {
    fn fresh() -> Self {
        Self {
            last_identity: None,
            restart_pending: true,
        }
    }
}

/// Per-stream boundary classification state.
///
/// Real ACP servers emit `agent_message_chunk` events without any stable
/// message id, so the classifier defaults mid-stream chunks to `Continue` and
/// only emits `StartNewMessage` at explicit boundaries: the very first chunk
/// after session start, after a prompt-turn reset, or after a tool-call
/// interleave. When a payload does carry a stable id, the classifier honors
/// it: matching ids stay `Continue`, differing ids start a new message.
#[derive(Debug, Clone)]
struct AcpBoundaryState {
    agent_message: StreamIdentity,
    agent_thought: StreamIdentity,
}

impl AcpBoundaryState {
    fn new() -> Self {
        Self {
            agent_message: StreamIdentity::fresh(),
            agent_thought: StreamIdentity::fresh(),
        }
    }

    /// Reset both streams at a prompt-turn boundary.
    ///
    /// ACP servers may legally reuse message ids across turns, so the next
    /// turn must always restart at `StartNewMessage` even when the first
    /// chunk repeats an earlier id.
    fn reset_for_prompt_turn(&mut self) {
        self.agent_message = StreamIdentity::fresh();
        self.agent_thought = StreamIdentity::fresh();
    }

    /// Reset both streams so the next agent or thought chunk is classified
    /// as `StartNewMessage`. Called whenever a tool-call invocation/result
    /// interleaves the visible stream.
    fn reset_for_tool_call(&mut self) {
        self.reset_for_prompt_turn();
    }
}

impl SubprocessSession {
    fn finish_prompt_turn(&mut self) {
        self.prompt_finished = true;
        self.prompt_response = None;
        self.boundary_state.reset_for_prompt_turn();
    }
}

impl AcpSession for SubprocessSession {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn try_next_update(&mut self) -> AcpResult<Option<ClientUpdate>> {
        // Drain queued visible updates before pulling more wire messages so a
        // single `tool_call` payload that yields both invocation and result
        // lines surfaces across two successive calls.
        if let Some(queued) = self.pending_updates.pop_front() {
            return Ok(Some(queued));
        }

        // Non-terminal `tool_call_update` events are silently absorbed into
        // merge state; keep pulling wire messages until either a visible
        // update is queued or the channel runs dry.
        loop {
            match self.rpc.try_next_update() {
                Ok(Some(value)) => {
                    append_raw_acp_update_trace(self.acp_trace_path.as_deref(), &value);
                    dispatch_update(
                        &value,
                        &self.cwd,
                        &mut self.tool_calls,
                        &mut self.boundary_state,
                        &mut self.pending_updates,
                    );
                    if let Some(queued) = self.pending_updates.pop_front() {
                        return Ok(Some(queued));
                    }
                }
                Ok(None) => break,
                Err(err) => {
                    self.finish_prompt_turn();
                    return Ok(Some(ClientUpdate::PromptTurnFailed {
                        message: err.to_string(),
                    }));
                }
            }
        }

        if self.prompt_finished {
            return Ok(None);
        }

        let Some(prompt_response) = self.prompt_response.as_ref() else {
            return Ok(None);
        };

        match prompt_response.try_recv() {
            Ok(Ok(result)) => {
                self.finish_prompt_turn();
                let update = match parse_prompt_result(result) {
                    Ok(PromptTurnOutcome::Finished) => ClientUpdate::PromptTurnFinished,
                    Ok(PromptTurnOutcome::Failed { message }) => {
                        ClientUpdate::PromptTurnFailed { message }
                    }
                    Err(err) => ClientUpdate::PromptTurnFailed {
                        message: err.to_string(),
                    },
                };
                Ok(Some(update))
            }
            Ok(Err(err)) => {
                self.finish_prompt_turn();
                Ok(Some(ClientUpdate::PromptTurnFailed {
                    message: err.to_string(),
                }))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                self.finish_prompt_turn();
                Ok(Some(ClientUpdate::PromptTurnFailed {
                    message: "ACP prompt turn channel disconnected".to_string(),
                }))
            }
        }
    }

    fn submit_prompt(&mut self, text: &str) -> AcpResult<()> {
        if !self.prompt_finished {
            return Err(AcpError::protocol(
                "ACP prompt turn is still running".to_string(),
            ));
        }
        // Starting a new prompt turn must clear any stale continuation cache
        // before the server can reuse a prior turn's messageId on its first
        // chunk. The conservative reset avoids cross-turn gluing.
        self.boundary_state.reset_for_prompt_turn();
        let prompt_params =
            prompt_request_params(&self.session_id, &PromptPayload::Text(text.to_string()))?;
        let prompt_response = self.rpc.start_request("session/prompt", prompt_params)?;
        self.prompt_response = Some(prompt_response);
        self.prompt_finished = false;
        Ok(())
    }

    fn cancel_prompt(&mut self) -> AcpResult<()> {
        self.rpc.notify(
            "session/cancel",
            json!({
                "sessionId": self.session_id,
            }),
        )
    }

    fn close(&mut self) -> AcpResult<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.pending_updates.clear();
        self.tool_calls = ToolCallMap::new();

        if self.supports_close {
            let _ = self
                .rpc
                .start_request("session/close", json!({ "sessionId": self.session_id }));
        }

        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                Err(err) => {
                    return Err(AcpError::io(format!(
                        "failed to inspect ACP child process: {err}"
                    )));
                }
            }
        }

        self.rpc.shutdown();
        Ok(())
    }
}

impl Drop for SubprocessSession {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

struct RpcPeer {
    writer: SharedWriter,
    pending: PendingRequests,
    updates_rx: Receiver<AcpResult<Value>>,
    reader_handle: Option<JoinHandle<()>>,
    next_request_id: u64,
}

impl RpcPeer {
    fn new(stdin: ChildStdin, stdout: ChildStdout) -> Self {
        let writer = Arc::new(Mutex::new(Some(stdin)));
        let pending = Arc::new(Mutex::new(BTreeMap::<u64, Sender<AcpResult<Value>>>::new()));
        let (updates_tx, updates_rx) = mpsc::channel();
        let reader_writer = Arc::clone(&writer);
        let reader_pending = Arc::clone(&pending);
        let reader_handle =
            thread::spawn(move || read_loop(stdout, reader_writer, reader_pending, updates_tx));
        Self {
            writer,
            pending,
            updates_rx,
            reader_handle: Some(reader_handle),
            next_request_id: 0,
        }
    }

    fn start_request(
        &mut self,
        method: &str,
        params: Value,
    ) -> AcpResult<Receiver<AcpResult<Value>>> {
        let id = self.next_request_id;
        self.next_request_id += 1;

        let (tx, rx) = mpsc::channel();
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id, tx);

        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        let write_result = write_json_rpc_line(&self.writer, &message);
        if let Err(err) = write_result {
            self.pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&id);
            return Err(AcpError::io(format!(
                "failed to write ACP request {method}: {err}"
            )));
        }
        Ok(rx)
    }

    fn notify(&mut self, method: &str, params: Value) -> AcpResult<()> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        write_json_rpc_line(&self.writer, &message).map_err(|err| {
            AcpError::io(format!("failed to write ACP notification {method}: {err}"))
        })
    }

    fn try_next_update(&mut self) -> AcpResult<Option<Value>> {
        match self.updates_rx.try_recv() {
            Ok(Ok(value)) => Ok(Some(value)),
            Ok(Err(err)) => Err(err),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Ok(None),
        }
    }

    fn shutdown(&mut self) {
        self.writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(handle) = self.reader_handle.take() {
            let _ = handle.join();
        }
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }
}

impl RpcCaller for RpcPeer {
    fn call(&mut self, method: &str, params: Value) -> AcpResult<Value> {
        let receiver = self.start_request(method, params)?;
        receiver
            .recv()
            .map_err(|_| AcpError::protocol(format!("ACP request {method} channel disconnected")))?
    }
}

fn write_json_rpc_line(writer: &SharedWriter, message: &Value) -> std::io::Result<()> {
    let encoded = serde_json::to_string(message)
        .map_err(|err| std::io::Error::new(ErrorKind::InvalidData, err))?;
    let mut guard = writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let writer = guard
        .as_mut()
        .ok_or_else(|| std::io::Error::new(ErrorKind::BrokenPipe, "ACP writer already closed"))?;
    writer
        .write_all(encoded.as_bytes())
        .and_then(|_| writer.write_all(b"\n"))
        .and_then(|_| writer.flush())
}

fn read_loop(
    stdout: ChildStdout,
    writer: SharedWriter,
    pending: PendingRequests,
    updates_tx: Sender<AcpResult<Value>>,
) {
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let line = match line {
            Ok(line) if !line.trim().is_empty() => line,
            Ok(_) => continue,
            Err(err) => {
                broadcast_transport_error(
                    &pending,
                    &updates_tx,
                    format!("failed to read ACP stdout: {err}"),
                );
                return;
            }
        };

        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                broadcast_transport_error(
                    &pending,
                    &updates_tx,
                    format!("invalid ACP JSON message: {err}"),
                );
                return;
            }
        };

        if let Some(method) = value.get("method").and_then(Value::as_str) {
            if method == "session/update" {
                // Forward the inner `update` field unchanged; the consumer
                // owns the per-session state needed to translate it. Null
                // signals "session/update without an update field" so the
                // dispatcher can emit Unknown { kind: "session/update" }.
                let update_value = value
                    .get("params")
                    .and_then(|params| params.get("update"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let _ = updates_tx.send(Ok(update_value));
                continue;
            }

            if let Some(id) = value.get("id") {
                let response = value
                    .get("params")
                    .and_then(|params| client_request_response(method, params))
                    .map(|result| {
                        json!({
                            "jsonrpc": "2.0",
                            "id": id.clone(),
                            "result": result,
                        })
                    })
                    .unwrap_or_else(|| {
                        json!({
                            "jsonrpc": "2.0",
                            "id": id.clone(),
                            "error": {
                                "code": -32601,
                                "message": format!("codexize client does not implement method {method}"),
                            }
                        })
                    });
                let _ = write_json_rpc_line(&writer, &response);
            }
            continue;
        }

        if let Some(id) = value.get("id").and_then(Value::as_u64) {
            let sender = pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&id);
            if let Some(sender) = sender {
                if let Some(error) = value.get("error") {
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("ACP request failed")
                        .to_string();
                    let _ = sender.send(Err(AcpError::protocol(message)));
                } else if let Some(result) = value.get("result") {
                    let _ = sender.send(Ok(result.clone()));
                } else {
                    let _ = sender.send(Err(AcpError::protocol(
                        "ACP response was missing both result and error".to_string(),
                    )));
                }
            }
            continue;
        }
    }

    broadcast_transport_error(
        &pending,
        &updates_tx,
        "ACP agent closed stdout before the prompt turn finished".to_string(),
    );
}

fn broadcast_transport_error(
    pending: &PendingRequests,
    updates_tx: &Sender<AcpResult<Value>>,
    message: String,
) {
    let err = AcpError::protocol(message);
    let pending_senders = {
        let mut guard = pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::take(&mut *guard)
            .into_values()
            .collect::<Vec<_>>()
    };
    for sender in pending_senders {
        let _ = sender.send(Err(err.clone()));
    }
    let _ = updates_tx.send(Err(err));
}

fn client_request_response(method: &str, params: &Value) -> Option<Value> {
    match method {
        "session/request_permission" => permission_response(params),
        _ => None,
    }
}

fn permission_response(params: &Value) -> Option<Value> {
    let options = params.get("options").and_then(Value::as_array)?;
    let selected = options
        .iter()
        .find(|option| {
            option.get("kind").and_then(Value::as_str) == Some("allow_once")
                || option.get("optionId").and_then(Value::as_str) == Some("approve")
        })
        .or_else(|| {
            options
                .iter()
                .find(|option| option.get("kind").and_then(Value::as_str) == Some("allow_always"))
        })?;
    let option_id = selected.get("optionId").and_then(Value::as_str)?;

    Some(json!({
        "outcome": {
            "outcome": "selected",
            "optionId": option_id
        }
    }))
}

#[derive(Debug)]
struct InitializeOutcome {
    protocol_version: u64,
    supports_close: bool,
}

#[derive(Debug, Deserialize)]
struct NewSessionResult {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "configOptions", default)]
    config_options: Vec<ConfigOption>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfigOption {
    id: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(rename = "currentValue", default)]
    current_value: Option<String>,
    #[serde(default)]
    options: Vec<ConfigChoice>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfigChoice {
    value: String,
}

fn parse_initialize_result(value: Value) -> AcpResult<InitializeOutcome> {
    let protocol_version = value
        .get("protocolVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| AcpError::protocol("ACP initialize response missing protocolVersion"))?;
    let supports_close = value
        .pointer("/agentCapabilities/sessionCapabilities/close")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(InitializeOutcome {
        protocol_version,
        supports_close,
    })
}

fn parse_new_session_result(value: Value) -> AcpResult<NewSessionResult> {
    serde_json::from_value(value).map_err(|err| {
        AcpError::protocol(format!("failed to parse ACP session/new response: {err}"))
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PromptTurnOutcome {
    Finished,
    Failed { message: String },
}

fn parse_prompt_result(value: Value) -> AcpResult<PromptTurnOutcome> {
    let stop_reason = value
        .get("stopReason")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::protocol("ACP prompt response missing stopReason"))?;
    if is_failed_stop_reason(stop_reason) {
        return Ok(PromptTurnOutcome::Failed {
            message: format!("ACP prompt turn failed with stopReason={stop_reason}"),
        });
    }
    Ok(PromptTurnOutcome::Finished)
}

fn is_failed_stop_reason(stop_reason: &str) -> bool {
    matches!(
        stop_reason,
        "cancelled"
            | "canceled"
            | "interrupted"
            | "error"
            | "errored"
            | "failed"
            | "timeout"
            | "timed_out"
    )
}

/// Translate one ACP `session/update` payload into zero or more visible
/// `ClientUpdate`s, mutating the per-session tool-call state map and
/// boundary state in the process. A single `tool_call` payload may yield two
/// updates (invocation followed by result) when its status is already
/// terminal; non-terminal `tool_call_update`s with prior state are absorbed
/// silently and emit nothing.
///
/// Each text-bearing update carries an `AcpTextBoundary`. The classification
/// rule is:
///
/// * `StartNewMessage` at every explicit boundary — session start, prompt-turn
///   reset, or tool-call interleave — and whenever a stable identity changes.
/// * `Continue` otherwise: mid-stream no-identity chunks default to continuing
///   the live block, and matching identities continue as well.
fn dispatch_update(
    value: &Value,
    cwd: &Path,
    map: &mut ToolCallMap,
    boundary_state: &mut AcpBoundaryState,
    out: &mut VecDeque<ClientUpdate>,
) {
    if value.is_null() {
        out.push_back(ClientUpdate::Unknown {
            kind: "session/update".to_string(),
        });
        return;
    }

    let kind = value
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match kind {
        "agent_message_chunk" => {
            let text = value
                .pointer("/content/text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let identity = extract_message_identity(value);
            let boundary =
                boundary_for_text_chunk(&mut boundary_state.agent_message, identity.as_deref());
            out.push_back(ClientUpdate::AgentMessageText {
                text,
                boundary,
                identity,
            });
        }
        "agent_thought_chunk" => {
            let text = value
                .pointer("/content/text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let identity = extract_message_identity(value);
            let boundary =
                boundary_for_text_chunk(&mut boundary_state.agent_thought, identity.as_deref());
            out.push_back(ClientUpdate::AgentThoughtText {
                text,
                boundary,
                identity,
            });
        }
        "session_info_update" => {
            out.push_back(ClientUpdate::SessionInfoUpdate {
                title: value
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
        "tool_call" => {
            // A tool-call invocation interleaves the visible stream and
            // therefore acts as a hard boundary for both agent and thought
            // streams. Any future free-form text gets `StartNewMessage` even
            // if it carries an identity we previously matched.
            boundary_state.reset_for_tool_call();
            handle_tool_call(ToolCallPayload::from_value(value), cwd, map, out);
        }
        "tool_call_update" => {
            // Mirror the `tool_call` behavior: a tool-call lifecycle update
            // (terminal or otherwise) prevents post-tool agent text from
            // gluing onto pre-tool live buffers.
            boundary_state.reset_for_tool_call();
            handle_tool_call_update(ToolCallPayload::from_value(value), map, out);
        }
        other => out.push_back(ClientUpdate::Unknown {
            kind: other.to_string(),
        }),
    }
}

#[cfg(test)]
pub fn client_updates_from_session_updates_for_test(
    values: impl IntoIterator<Item = Value>,
    cwd: &Path,
) -> Vec<ClientUpdate> {
    let mut map = ToolCallMap::new();
    let mut boundary_state = AcpBoundaryState::new();
    let mut out = VecDeque::new();
    for value in values {
        dispatch_update(&value, cwd, &mut map, &mut boundary_state, &mut out);
    }
    out.into_iter().collect()
}

/// Classify a single text chunk relative to the per-stream state we have
/// already observed.
///
/// `restart_pending` is the only source of `StartNewMessage` for no-identity
/// chunks: it is set at session start and at every explicit boundary
/// (prompt-turn reset, tool-call interleave), and cleared once a chunk has
/// been classified. Real ACP servers do not surface a stable message id on
/// `agent_message_chunk` events, so a no-identity mid-stream chunk defaults
/// to `Continue` rather than over-splitting one streamed response into one
/// persisted message per chunk.
///
/// When a payload does carry a stable id, the classifier honors it: a
/// matching id continues the live block; a differing id starts a new one.
/// The `restart_pending` flag still wins over a matching id, so the first
/// chunk after a tool-call or prompt-turn reset is `StartNewMessage` even if
/// it carries the previous turn's id.
fn boundary_for_text_chunk(state: &mut StreamIdentity, incoming: Option<&str>) -> AcpTextBoundary {
    let boundary = if state.restart_pending {
        if let Some(id) = incoming {
            state.last_identity = Some(id.to_string());
        }
        AcpTextBoundary::StartNewMessage
    } else {
        match (incoming, state.last_identity.as_deref()) {
            (Some(id), Some(last)) if last == id => AcpTextBoundary::Continue,
            (Some(id), _) => {
                state.last_identity = Some(id.to_string());
                AcpTextBoundary::StartNewMessage
            }
            (None, _) => AcpTextBoundary::Continue,
        }
    };
    state.restart_pending = false;
    boundary
}

/// Best-effort lookup of a stable ACP message identity on a `session/update`
/// payload. The ACP spec does not currently mandate a single field name, so
/// this checks the most plausible locations. Any future protocol revision
/// that surfaces a stable id should land here.
fn extract_message_identity(value: &Value) -> Option<String> {
    const CANDIDATES: &[&str] = &[
        "/messageId",
        "/message_id",
        "/id",
        "/content/messageId",
        "/content/message_id",
        "/content/id",
    ];
    for pointer in CANDIDATES {
        if let Some(id) = value.pointer(pointer).and_then(Value::as_str)
            && !id.is_empty()
        {
            return Some(id.to_string());
        }
    }
    None
}

fn handle_tool_call(
    payload: ToolCallPayload,
    cwd: &Path,
    map: &mut ToolCallMap,
    out: &mut VecDeque<ClientUpdate>,
) {
    let state = ToolCallDisplayState::from_payload(&payload);
    let terminal = state
        .status
        .as_deref()
        .map(is_terminal_status)
        .unwrap_or(false);

    let invocation = format_invocation_line(&state, cwd);

    if let Some(id) = payload.tool_call_id.clone() {
        map.insert(id.clone(), state.clone());
        out.push_back(tool_call_text(invocation));
        // Watchdog activity transitions: a `tool_call` payload represents a
        // freshly-observed tool-call id. If it is non-terminal (or missing
        // status, which we conservatively treat as in-flight), emit a Start
        // so the App can pause its idle clock from this moment. If it is
        // already terminal, skip Start — there was no observable in-flight
        // window for this id at the runner — and emit a single Finish.
        if terminal {
            out.push_back(tool_call_text(format_result_line(&state)));
            if !map.terminal_emitted(&id) {
                out.push_back(ClientUpdate::ToolCallActivity {
                    tool_call_id: id.clone(),
                    kind: ToolCallActivityKind::Finish,
                });
                map.mark_terminal_emitted(&id);
            }
            map.evict(&id);
            // Spec §Behavior rule 1: when the same payload carries terminal
            // status, emit the result block immediately afterward and evict.
        } else if !map.start_emitted(&id) {
            out.push_back(ClientUpdate::ToolCallActivity {
                tool_call_id: id.clone(),
                kind: ToolCallActivityKind::Start,
            });
            map.mark_start_emitted(&id);
        }
    } else {
        // Missing toolCallId: best-effort output only, never stored.
        // No watchdog activity emitted: dedup requires a stable id.
        out.push_back(tool_call_text(invocation));
        if terminal {
            out.push_back(tool_call_text(format_result_line(&state)));
        }
    }
}

fn handle_tool_call_update(
    payload: ToolCallPayload,
    map: &mut ToolCallMap,
    out: &mut VecDeque<ClientUpdate>,
) {
    let terminal = payload
        .status
        .as_deref()
        .map(is_terminal_status)
        .unwrap_or(false);
    // For watchdog activity tracking, only an explicit `pending` /
    // `in_progress` status on an update counts as a Start transition. An
    // update without an explicit status is just a property merge, not a
    // lifecycle signal, so we do not synthesize a Start from it.
    let active = payload
        .status
        .as_deref()
        .is_some_and(|status| matches!(status, "pending" | "in_progress"));

    let Some(id) = payload.tool_call_id.clone() else {
        // Missing toolCallId: best-effort result if terminal, otherwise drop.
        // No watchdog activity emitted: dedup requires a stable id.
        if terminal {
            let state = ToolCallDisplayState::from_payload(&payload);
            out.push_back(tool_call_text(format_result_line(&state)));
        }
        return;
    };

    if let Some(state) = map.merge(&id, &payload) {
        if terminal {
            let result = format_result_line(state);
            out.push_back(tool_call_text(result));
            if !map.terminal_emitted(&id) {
                out.push_back(ClientUpdate::ToolCallActivity {
                    tool_call_id: id.clone(),
                    kind: ToolCallActivityKind::Finish,
                });
                map.mark_terminal_emitted(&id);
            }
            map.evict(&id);
        } else if active && !map.start_emitted(&id) {
            // Server reported in_progress / pending without a prior
            // `tool_call`; treat this as the first observation of a
            // non-terminal status and emit a single Start.
            out.push_back(ClientUpdate::ToolCallActivity {
                tool_call_id: id.clone(),
                kind: ToolCallActivityKind::Start,
            });
            map.mark_start_emitted(&id);
        }
        // Non-terminal merges into prior state and produces no transcript
        // output (spec §Behavior rule 5).
    } else if terminal {
        if map.terminal_emitted(&id) {
            // Duplicate terminal update for an already-completed id: suppress
            // re-emission to keep the two-block contract append-only and to
            // satisfy the one-shot Finish contract for the watchdog.
            return;
        }
        // No prior state (never created or already evicted): emit a
        // best-effort result block from the payload only; never insert.
        let state = ToolCallDisplayState::from_payload(&payload);
        out.push_back(tool_call_text(format_result_line(&state)));
        out.push_back(ClientUpdate::ToolCallActivity {
            tool_call_id: id.clone(),
            kind: ToolCallActivityKind::Finish,
        });
        map.mark_terminal_emitted(&id);
    }
    // Non-terminal update with no prior state is silently dropped.
}

/// Build a `ClientUpdate::ToolCallText` with the boundary metadata required
/// by the runner. Tool-call invocation/result text is always tagged
/// `StartNewMessage` so the runner can finalize the thought stream's live
/// buffer before appending the synthetic paragraph and prevent post-tool
/// free-form text from gluing onto a pre-tool live buffer.
fn tool_call_text(text: String) -> ClientUpdate {
    ClientUpdate::ToolCallText {
        text,
        boundary: AcpTextBoundary::StartNewMessage,
        identity: None,
    }
}

fn prompt_blocks(prompt: &PromptPayload) -> AcpResult<Vec<Value>> {
    let text = match prompt {
        PromptPayload::Text(text) => text.clone(),
        PromptPayload::File(path) => std::fs::read_to_string(path).map_err(|err| {
            AcpError::io(format!(
                "failed to read ACP prompt payload {}: {err}",
                path.display()
            ))
        })?,
    };
    Ok(vec![json!({
        "type": "text",
        "text": text
    })])
}

fn prompt_request_params(session_id: &str, prompt: &PromptPayload) -> AcpResult<Value> {
    Ok(json!({
        "sessionId": session_id,
        "messageId": uuid::Uuid::new_v4().to_string(),
        "prompt": prompt_blocks(prompt)?,
    }))
}

fn append_raw_acp_update_trace(path: Option<&Path>, update: &Value) {
    let Some(path) = path else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let record = json!({
        "type": "raw_update",
        "ts": chrono::Utc::now().to_rfc3339(),
        "update": update,
    });
    let Ok(line) = serde_json::to_string(&record) else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

fn debug_protocol(message: impl AsRef<str>) {
    eprintln!("[codexize][acp][debug] {}", message.as_ref());
}

fn apply_session_config(
    rpc: &mut impl RpcCaller,
    session_id: &str,
    session: &super::AcpSessionSpec,
    config_options: &mut Vec<ConfigOption>,
) -> AcpResult<()> {
    // ACP standardizes categories, not concrete option values. The first seam
    // uses the common ask/code convention and falls back to the codexize env
    // contract whenever an agent exposes different labels.
    let desired = [
        ("mode", session.permission_mode.as_str().to_string()),
        ("model", session.model.clone()),
        (
            "thought_level",
            session.reasoning_effort.as_str().to_string(),
        ),
    ];
    let baseline_options = config_options.clone();

    for (category, value) in desired {
        let Some(option) = baseline_options
            .iter()
            .find(|option| option.category.as_deref() == Some(category) || option.id == category)
            .cloned()
        else {
            continue;
        };

        if option.current_value.as_deref() == Some(value.as_str()) {
            continue;
        }
        if !option.options.is_empty() && !option.options.iter().any(|choice| choice.value == value)
        {
            continue;
        }

        let response = match rpc.call(
            "session/set_config_option",
            json!({
                "sessionId": session_id,
                "configId": option.id,
                "value": value,
            }),
        ) {
            Ok(response) => response,
            Err(err) => {
                debug_protocol(format!(
                    "session/set_config_option failed for category={category} id={} value={value}: {err}",
                    option.id
                ));
                continue;
            }
        };
        match parse_config_options_response(response) {
            Ok(updated) => *config_options = updated,
            Err(err) => {
                debug_protocol(format!(
                    "session/set_config_option response parse failed for category={category} id={}: {err}",
                    option.id
                ));
            }
        }
    }

    Ok(())
}

fn parse_config_options_response(value: Value) -> AcpResult<Vec<ConfigOption>> {
    #[derive(Deserialize)]
    struct ConfigOptionsResponse {
        #[serde(rename = "configOptions", default)]
        config_options: Vec<ConfigOption>,
    }

    let response: ConfigOptionsResponse = serde_json::from_value(value).map_err(|err| {
        AcpError::protocol(format!(
            "failed to parse ACP session/set_config_option response: {err}"
        ))
    })?;
    Ok(response.config_options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{adapters::EffortLevel, state::LaunchModes};
    use std::{collections::BTreeMap, path::PathBuf};

    #[derive(Default)]
    struct StubRpcCaller {
        calls: Vec<(String, Value)>,
        responses: Vec<AcpResult<Value>>,
    }

    impl RpcCaller for StubRpcCaller {
        fn call(&mut self, method: &str, params: Value) -> AcpResult<Value> {
            self.calls.push((method.to_string(), params));
            if self.responses.is_empty() {
                return Err(AcpError::protocol(
                    "stub RPC missing response for session/set_config_option",
                ));
            }
            self.responses.remove(0)
        }
    }

    fn sample_session() -> super::super::AcpSessionSpec {
        super::super::AcpSessionSpec {
            cwd: PathBuf::from("/tmp/project"),
            prompt: PromptPayload::Text("ship it".to_string()),
            model: "model-next".to_string(),
            requested_effort: EffortLevel::Normal,
            effective_effort: EffortLevel::Normal,
            reasoning_effort: super::super::AcpReasoningEffort::High,
            permission_mode: super::super::AcpPermissionMode::Code,
            interactive: false,
            modes: LaunchModes::default(),
            required_artifacts: Vec::new(),
            policy: super::super::AcpLaunchPolicy::default(),
            metadata: BTreeMap::new(),
        }
    }

    fn configurable_option(
        id: &str,
        category: &str,
        current: &str,
        choices: &[&str],
    ) -> ConfigOption {
        ConfigOption {
            id: id.to_string(),
            category: Some(category.to_string()),
            current_value: Some(current.to_string()),
            options: choices
                .iter()
                .map(|choice| ConfigChoice {
                    value: (*choice).to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn parse_prompt_result_marks_failure_stop_reasons() {
        let result = parse_prompt_result(json!({ "stopReason": "interrupted" }))
            .expect("stop reason should parse");
        assert!(matches!(
            result,
            PromptTurnOutcome::Failed { message } if message.contains("interrupted")
        ));
    }

    #[test]
    fn parse_prompt_result_accepts_success_stop_reasons() {
        let result =
            parse_prompt_result(json!({ "stopReason": "end_turn" })).expect("stop reason parsed");
        assert_eq!(result, PromptTurnOutcome::Finished);
    }

    #[test]
    fn prompt_request_params_include_uuid_message_id() {
        let params = prompt_request_params("sess-1", &PromptPayload::Text("hello".to_string()))
            .expect("prompt params");
        let message_id = params
            .get("messageId")
            .and_then(Value::as_str)
            .expect("messageId");

        assert_eq!(message_id.len(), 36);
        assert_eq!(message_id.chars().filter(|ch| *ch == '-').count(), 4);
        assert_eq!(
            params.get("sessionId").and_then(Value::as_str),
            Some("sess-1")
        );
        assert_eq!(
            params.pointer("/prompt/0/text").and_then(Value::as_str),
            Some("hello")
        );
    }

    #[test]
    fn raw_acp_update_trace_records_payload() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let trace_path = temp.path().join("run.acp.jsonl");
        let update = json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "text": "Proposed correction" }
        });

        append_raw_acp_update_trace(Some(&trace_path), &update);

        let trace = std::fs::read_to_string(trace_path).expect("trace file");
        assert!(trace.contains(r#""type":"raw_update""#));
        assert!(trace.contains(r#""sessionUpdate":"agent_message_chunk""#));
        assert!(trace.contains(r#""text":"Proposed correction""#));
    }

    fn drain(value: Value, cwd: &Path, map: &mut ToolCallMap) -> Vec<ClientUpdate> {
        let mut state = AcpBoundaryState::new();
        let mut out = VecDeque::new();
        dispatch_update(&value, cwd, map, &mut state, &mut out);
        out.into_iter().collect()
    }

    fn drain_with_state(
        value: Value,
        cwd: &Path,
        map: &mut ToolCallMap,
        state: &mut AcpBoundaryState,
    ) -> Vec<ClientUpdate> {
        let mut out = VecDeque::new();
        dispatch_update(&value, cwd, map, state, &mut out);
        out.into_iter().collect()
    }

    fn tool_call_block(text: &str) -> ClientUpdate {
        ClientUpdate::ToolCallText {
            text: text.to_string(),
            boundary: AcpTextBoundary::StartNewMessage,
            identity: None,
        }
    }

    fn activity(id: &str, kind: ToolCallActivityKind) -> ClientUpdate {
        ClientUpdate::ToolCallActivity {
            tool_call_id: id.to_string(),
            kind,
        }
    }

    #[test]
    fn dispatch_renders_invocation_from_observed_codex_read_payload() {
        let mut map = ToolCallMap::new();
        let updates = drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_1",
                "title": "Read Cargo.toml",
                "kind": "read",
                "status": "in_progress",
                "locations": [{ "path": "/work/project/Cargo.toml" }],
                "rawInput": {
                    "command": ["/bin/zsh", "-lc", "sed -n '1,120p' Cargo.toml"]
                }
            }),
            Path::new("/work/project"),
            &mut map,
        );

        assert_eq!(
            updates,
            vec![
                tool_call_block("tool: read(Cargo.toml)"),
                activity("call_1", ToolCallActivityKind::Start),
            ]
        );
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn dispatch_emits_invocation_then_result_when_terminal_arrives_in_two_payloads() {
        // Spec §Behavior: the invocation block is emitted on `tool_call`, and
        // a separate result block is emitted on the terminal `tool_call_update`.
        let mut map = ToolCallMap::new();
        let invocation = drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_1",
                "title": "Read Cargo.toml",
                "kind": "read",
                "status": "in_progress",
                "locations": [{ "path": "/work/project/Cargo.toml" }],
            }),
            Path::new("/work/project"),
            &mut map,
        );
        assert_eq!(
            invocation,
            vec![
                tool_call_block("tool: read(Cargo.toml)"),
                activity("call_1", ToolCallActivityKind::Start),
            ]
        );
        assert!(map.contains("call_1"));

        let result = drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_1",
                "status": "completed",
                "rawOutput": { "exit_code": 0, "stdout": "[package] name = \"codexize\"" }
            }),
            Path::new("/work/project"),
            &mut map,
        );
        assert_eq!(
            result,
            vec![
                tool_call_block("result: completed, exit 0, output: [package] name = \"codexize\""),
                activity("call_1", ToolCallActivityKind::Finish),
            ]
        );
        // After eviction the entry must be gone.
        assert!(!map.contains("call_1"));
    }

    #[test]
    fn dispatch_emits_invocation_and_result_when_tool_call_payload_is_already_terminal() {
        // Spec §Behavior rule 1: a `tool_call` carrying terminal status emits
        // the invocation followed by the result, then evicts.
        let mut map = ToolCallMap::new();
        let updates = drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_q",
                "kind": "execute",
                "status": "completed",
                "rawInput": { "command": ["echo", "ok"] },
                "rawOutput": { "exit_code": 0, "stdout": "ok" }
            }),
            Path::new("/tmp"),
            &mut map,
        );
        assert_eq!(
            updates,
            vec![
                tool_call_block("tool: exec(echo ok)"),
                tool_call_block("result: completed, exit 0, output: ok"),
                activity("call_q", ToolCallActivityKind::Finish),
            ]
        );
        assert!(!map.contains("call_q"));
    }

    #[test]
    fn dispatch_silently_merges_non_terminal_update_into_existing_state() {
        // Spec §Behavior rule 5: non-terminal `tool_call_update` events
        // produce no transcript output but still merge into the merge state,
        // so a later terminal update can use the merged status snapshot.
        let mut map = ToolCallMap::new();
        let _ = drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_1",
                "kind": "execute",
                "rawInput": { "command": ["sleep", "1"] }
            }),
            Path::new("/tmp"),
            &mut map,
        );
        let progress = drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_1",
                "status": "in_progress",
                "rawOutput": { "stdout": "still working" }
            }),
            Path::new("/tmp"),
            &mut map,
        );
        assert!(
            progress.is_empty(),
            "non-terminal updates must produce no visible blocks"
        );
        let merged = map.get("call_1").expect("entry preserved");
        assert_eq!(merged.status.as_deref(), Some("in_progress"));
    }

    #[test]
    fn dispatch_terminal_update_without_prior_state_emits_best_effort_result_only() {
        // Spec §Behavior rule 4: terminal update with no prior state renders
        // a result block from the payload alone, with no synthesized
        // invocation and no map entry retained.
        let mut map = ToolCallMap::new();
        let updates = drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "stale_id",
                "status": "completed",
                "rawOutput": { "exit_code": 0, "stdout": "ok" }
            }),
            Path::new("/tmp"),
            &mut map,
        );
        assert_eq!(
            updates,
            vec![
                tool_call_block("result: completed, exit 0, output: ok"),
                activity("stale_id", ToolCallActivityKind::Finish),
            ]
        );
        assert!(
            !map.contains("stale_id"),
            "best-effort updates must never insert state"
        );
    }

    #[test]
    fn dispatch_second_terminal_update_for_evicted_id_is_suppressed() {
        // Once a terminal result has been emitted for an id, later terminal
        // updates for that same id are ignored unless a new `tool_call`
        // reuses the id and starts a fresh lifecycle.
        let mut map = ToolCallMap::new();
        let _invocation = drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_1",
                "kind": "execute",
                "rawInput": { "command": ["echo", "hi"] }
            }),
            Path::new("/tmp"),
            &mut map,
        );
        let first_result = drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_1",
                "status": "completed",
                "rawOutput": { "exit_code": 0, "stdout": "hi" }
            }),
            Path::new("/tmp"),
            &mut map,
        );
        // Result block + watchdog Finish transition.
        assert_eq!(
            first_result,
            vec![
                tool_call_block("result: completed, exit 0, output: hi"),
                activity("call_1", ToolCallActivityKind::Finish),
            ]
        );
        assert!(!map.contains("call_1"));

        // Duplicate terminal update for the now-evicted id must be ignored.
        let second_result = drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_1",
                "status": "completed",
                "rawOutput": { "exit_code": 0, "stdout": "stale" }
            }),
            Path::new("/tmp"),
            &mut map,
        );
        assert!(second_result.is_empty());
        assert!(!map.contains("call_1"));

        // A non-terminal stale update produces nothing.
        let stale = drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_1",
                "status": "in_progress"
            }),
            Path::new("/tmp"),
            &mut map,
        );
        assert!(stale.is_empty());
    }

    #[test]
    fn dispatch_id_reuse_after_terminal_renders_text_without_repeating_activity() {
        // Id reuse is permitted for the *display* contract (a fresh
        // invocation block + a fresh result block render under the reused
        // id), but the watchdog activity stream is one-shot per id within a
        // session: a previously-completed tool_call_id must not be allowed
        // to resurrect Start or Finish — doing so would let the App's idle
        // clock pause/resume on a phantom call.
        let mut map = ToolCallMap::new();
        let _ = drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_1",
                "kind": "execute",
                "rawInput": { "command": ["echo", "first"] }
            }),
            Path::new("/tmp"),
            &mut map,
        );
        let _ = drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_1",
                "status": "completed",
                "rawOutput": { "stdout": "first" }
            }),
            Path::new("/tmp"),
            &mut map,
        );

        let reused_invocation = drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_1",
                "kind": "execute",
                "rawInput": { "command": ["echo", "second"] }
            }),
            Path::new("/tmp"),
            &mut map,
        );
        assert_eq!(
            reused_invocation,
            vec![tool_call_block("tool: exec(echo second)")],
            "reused id must not re-emit Start"
        );
        let reused_result = drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_1",
                "status": "completed",
                "rawOutput": { "stdout": "second" }
            }),
            Path::new("/tmp"),
            &mut map,
        );
        assert_eq!(
            reused_result,
            vec![tool_call_block("result: completed, output: second")],
            "reused id must not re-emit Finish"
        );
    }

    #[test]
    fn dispatch_renders_exec_invocation_from_command_array() {
        let mut map = ToolCallMap::new();
        let updates = drain(
            json!({
                "sessionUpdate": "tool_call",
                "rawInput": { "command": ["cargo", "test", "--workspace"] }
            }),
            Path::new("/tmp"),
            &mut map,
        );
        assert_eq!(
            updates,
            vec![tool_call_block("tool: exec(cargo test --workspace)")]
        );
        // Missing toolCallId must never be stored.
        assert_eq!(map.len(), 0);
    }

    #[test]
    fn dispatch_falls_back_to_literal_tool_when_payload_is_empty() {
        let mut map = ToolCallMap::new();
        let updates = drain(
            json!({ "sessionUpdate": "tool_call" }),
            Path::new("/tmp"),
            &mut map,
        );
        assert_eq!(updates, vec![tool_call_block("tool: tool")]);
    }

    fn activity_only(updates: Vec<ClientUpdate>) -> Vec<ClientUpdate> {
        updates
            .into_iter()
            .filter(|update| matches!(update, ClientUpdate::ToolCallActivity { .. }))
            .collect()
    }

    #[test]
    fn activity_dedup_repeated_pending_in_progress_emits_single_start() {
        // The same id may surface across `tool_call` (pending) and one or
        // more non-terminal `tool_call_update`s (in_progress) before any
        // terminal status arrives. Exactly one Start must be emitted.
        let mut map = ToolCallMap::new();
        let initial = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_dup",
                "kind": "execute",
                "status": "pending",
                "rawInput": { "command": ["sleep", "1"] }
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        assert_eq!(
            initial,
            vec![activity("call_dup", ToolCallActivityKind::Start)]
        );

        let progress = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_dup",
                "status": "in_progress"
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        assert!(
            progress.is_empty(),
            "second non-terminal status must not re-emit Start"
        );

        let still_progress = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_dup",
                "status": "in_progress",
                "rawOutput": { "stdout": "tick" }
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        assert!(
            still_progress.is_empty(),
            "third non-terminal status must not re-emit Start"
        );
    }

    #[test]
    fn activity_dedup_terminal_then_terminal_emits_single_finish() {
        // A runner can plausibly observe `failed` followed by a `cancelled`
        // (or any second terminal) for the same id. The second terminal
        // must be suppressed: the Finish contract is one-shot per id.
        let mut map = ToolCallMap::new();
        let _ = drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_t",
                "kind": "execute",
                "status": "in_progress",
                "rawInput": { "command": ["sleep", "1"] }
            }),
            Path::new("/tmp"),
            &mut map,
        );
        let first_terminal = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_t",
                "status": "failed",
                "rawOutput": { "exit_code": 1, "stderr": "boom" }
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        assert_eq!(
            first_terminal,
            vec![activity("call_t", ToolCallActivityKind::Finish)]
        );

        let second_terminal = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_t",
                "status": "cancelled"
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        assert!(
            second_terminal.is_empty(),
            "second terminal status for the same id must not re-emit Finish"
        );

        let third_terminal = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_t",
                "status": "errored"
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        assert!(
            third_terminal.is_empty(),
            "later terminal statuses must remain suppressed"
        );
    }

    #[test]
    fn activity_dedup_repeated_tool_call_payload_emits_single_start() {
        // Some agents resend a `tool_call` (not `tool_call_update`) for the
        // same id — e.g. as part of a status refresh. The display map is
        // allowed to overwrite on id reuse, but the watchdog Start contract
        // is one-shot per id for the lifetime of the session.
        let mut map = ToolCallMap::new();
        let initial = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_repeat",
                "kind": "execute",
                "status": "in_progress",
                "rawInput": { "command": ["sleep", "1"] }
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        assert_eq!(
            initial,
            vec![activity("call_repeat", ToolCallActivityKind::Start)]
        );

        let resent = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_repeat",
                "kind": "execute",
                "status": "in_progress",
                "rawInput": { "command": ["sleep", "1"] }
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        assert!(
            resent.is_empty(),
            "repeated tool_call payload must not re-emit Start"
        );

        // Even if the resent payload bears `pending` (a different non-
        // terminal status), the contract still suppresses re-emission.
        let resent_pending = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_repeat",
                "kind": "execute",
                "status": "pending",
                "rawInput": { "command": ["sleep", "1"] }
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        assert!(
            resent_pending.is_empty(),
            "second non-terminal tool_call payload must not re-emit Start"
        );
    }

    #[test]
    fn activity_dedup_repeated_terminal_tool_call_payload_emits_single_finish() {
        // After a tool call has completed, an agent may resend a `tool_call`
        // for the same id with terminal status — e.g. duplicated transport
        // or a retry layer above the runner. The Finish contract is also
        // one-shot per id for the lifetime of the session.
        let mut map = ToolCallMap::new();
        let _ = drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_dt",
                "kind": "execute",
                "status": "in_progress",
                "rawInput": { "command": ["echo", "x"] }
            }),
            Path::new("/tmp"),
            &mut map,
        );
        let first_terminal = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_dt",
                "kind": "execute",
                "status": "completed",
                "rawInput": { "command": ["echo", "x"] },
                "rawOutput": { "exit_code": 0, "stdout": "x" }
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        assert_eq!(
            first_terminal,
            vec![activity("call_dt", ToolCallActivityKind::Finish)]
        );

        let second_terminal = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_dt",
                "kind": "execute",
                "status": "completed",
                "rawInput": { "command": ["echo", "x"] },
                "rawOutput": { "exit_code": 0, "stdout": "x" }
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        assert!(
            second_terminal.is_empty(),
            "repeated terminal tool_call payload must not re-emit Finish"
        );

        // A final mixed-flavor cross-check: if a `tool_call_update` arrives
        // with another terminal status after the duplicate-terminal stream,
        // it must also remain suppressed.
        let third_terminal = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_dt",
                "status": "errored"
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        assert!(
            third_terminal.is_empty(),
            "tool_call_update terminal after duplicate-terminal tool_call must remain suppressed"
        );
    }

    #[test]
    fn activity_dedup_terminal_tool_call_after_completion_does_not_resurrect_finish() {
        // Specifically exercises the bug fix: a terminal `tool_call_update`
        // completes the lifecycle and evicts the display entry, then a fresh
        // `tool_call` payload arrives with terminal status for the same id
        // (a re-acknowledgement or transport echo). Display is best-effort,
        // but Finish must not re-emit.
        let mut map = ToolCallMap::new();
        let _ = drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_echo",
                "kind": "execute",
                "status": "in_progress",
                "rawInput": { "command": ["echo", "y"] }
            }),
            Path::new("/tmp"),
            &mut map,
        );
        let _ = drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_echo",
                "status": "completed",
                "rawOutput": { "exit_code": 0, "stdout": "y" }
            }),
            Path::new("/tmp"),
            &mut map,
        );
        // Display-side entry is now evicted; markers must remain.
        let resurrected = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_echo",
                "kind": "execute",
                "status": "completed",
                "rawInput": { "command": ["echo", "y"] },
                "rawOutput": { "exit_code": 0, "stdout": "y" }
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        assert!(
            resurrected.is_empty(),
            "terminal tool_call payload for an already-finished id must not re-emit Finish"
        );
    }

    #[test]
    fn activity_distinct_ids_are_tracked_independently_with_correct_ordering() {
        // Two overlapping tool calls must each yield their own Start and
        // their own Finish, in arrival order. The order of dispatch through
        // `dispatch_update` is the order the App sees the activity stream.
        let mut map = ToolCallMap::new();

        let start_a = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_a",
                "kind": "execute",
                "status": "in_progress",
                "rawInput": { "command": ["echo", "a"] }
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        let start_b = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_b",
                "kind": "execute",
                "status": "in_progress",
                "rawInput": { "command": ["echo", "b"] }
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        let finish_a = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_a",
                "status": "completed",
                "rawOutput": { "exit_code": 0, "stdout": "a" }
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        let finish_b = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_b",
                "status": "completed",
                "rawOutput": { "exit_code": 0, "stdout": "b" }
            }),
            Path::new("/tmp"),
            &mut map,
        ));

        assert_eq!(
            start_a,
            vec![activity("call_a", ToolCallActivityKind::Start)]
        );
        assert_eq!(
            start_b,
            vec![activity("call_b", ToolCallActivityKind::Start)]
        );
        assert_eq!(
            finish_a,
            vec![activity("call_a", ToolCallActivityKind::Finish)]
        );
        assert_eq!(
            finish_b,
            vec![activity("call_b", ToolCallActivityKind::Finish)]
        );
    }

    #[test]
    fn activity_terminal_directly_emits_only_finish_no_start() {
        // A `tool_call` payload that arrives already terminal had no
        // observable in-flight window from the runner's perspective. Spec
        // §3.6: Start fires only on first observation of a non-terminal
        // status. Finish still fires once.
        let mut map = ToolCallMap::new();
        let updates = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "fast_call",
                "kind": "execute",
                "status": "completed",
                "rawInput": { "command": ["true"] },
                "rawOutput": { "exit_code": 0 }
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        assert_eq!(
            updates,
            vec![activity("fast_call", ToolCallActivityKind::Finish)]
        );
    }

    #[test]
    fn activity_missing_tool_call_id_emits_no_transitions() {
        // Dedup requires a stable id. Best-effort visible blocks may still
        // be produced, but no activity transitions are emitted because there
        // is nothing to attribute pause/resume to.
        let mut map = ToolCallMap::new();
        let initial = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call",
                "kind": "execute",
                "rawInput": { "command": ["echo", "ghost"] }
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        assert!(initial.is_empty());

        let terminal = activity_only(drain(
            json!({
                "sessionUpdate": "tool_call_update",
                "status": "completed",
                "rawOutput": { "exit_code": 0, "stdout": "ghost" }
            }),
            Path::new("/tmp"),
            &mut map,
        ));
        assert!(terminal.is_empty());
    }

    #[test]
    fn dispatch_routes_unrelated_kinds_containing_tool_to_unknown() {
        // Spec §Interfaces: only exact "tool_call" / "tool_call_update" route
        // through the new pipeline. Other kinds remain Unknown.
        let mut map = ToolCallMap::new();
        let updates = drain(
            json!({ "sessionUpdate": "tool_progress_chunk" }),
            Path::new("/tmp"),
            &mut map,
        );
        assert_eq!(
            updates,
            vec![ClientUpdate::Unknown {
                kind: "tool_progress_chunk".to_string()
            }]
        );
    }

    #[test]
    fn dispatch_emits_session_update_unknown_when_payload_is_null() {
        let mut map = ToolCallMap::new();
        let updates = drain(Value::Null, Path::new("/tmp"), &mut map);
        assert_eq!(
            updates,
            vec![ClientUpdate::Unknown {
                kind: "session/update".to_string()
            }]
        );
    }

    #[test]
    fn dispatch_passes_through_agent_message_and_thought_chunks() {
        let mut map = ToolCallMap::new();
        let messages = drain(
            json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "text": "hello" }
            }),
            Path::new("/tmp"),
            &mut map,
        );
        // No stable identity in the payload, so the conservative fallback
        // tags the chunk as `StartNewMessage` (see spec §Design — over-split
        // rather than risk merging unrelated logical messages).
        assert_eq!(
            messages,
            vec![ClientUpdate::AgentMessageText {
                text: "hello".to_string(),
                boundary: AcpTextBoundary::StartNewMessage,
                identity: None,
            }]
        );

        let thoughts = drain(
            json!({
                "sessionUpdate": "agent_thought_chunk",
                "content": { "text": "thinking" }
            }),
            Path::new("/tmp"),
            &mut map,
        );
        assert_eq!(
            thoughts,
            vec![ClientUpdate::AgentThoughtText {
                text: "thinking".to_string(),
                boundary: AcpTextBoundary::StartNewMessage,
                identity: None,
            }]
        );
    }

    #[test]
    fn dispatch_continues_no_identity_chunks_until_explicit_boundary() {
        // Real ACP servers emit `agent_message_chunk` events without any
        // stable message id; treating each chunk as a fresh logical message
        // would over-split a single streamed response into one persisted
        // block per chunk. The first chunk on a fresh stream is
        // StartNewMessage (initial restart_pending), but subsequent
        // no-identity chunks default to Continue. Explicit boundaries —
        // tool-call interleave or prompt-turn reset — are still honored by
        // the dedicated tests in this module.
        let mut map = ToolCallMap::new();
        let mut state = AcpBoundaryState::new();
        let first = drain_with_state(
            json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "text": "first " }
            }),
            Path::new("/tmp"),
            &mut map,
            &mut state,
        );
        let second = drain_with_state(
            json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "text": "second" }
            }),
            Path::new("/tmp"),
            &mut map,
            &mut state,
        );
        assert_eq!(
            first,
            vec![ClientUpdate::AgentMessageText {
                text: "first ".to_string(),
                boundary: AcpTextBoundary::StartNewMessage,
                identity: None,
            }]
        );
        assert_eq!(
            second,
            vec![ClientUpdate::AgentMessageText {
                text: "second".to_string(),
                boundary: AcpTextBoundary::Continue,
                identity: None,
            }]
        );
    }

    #[test]
    fn dispatch_restarts_no_identity_chunk_after_tool_call_interleave() {
        // Without identity, mid-stream chunks default to Continue, but a
        // tool-call interleave is still a hard boundary: the first chunk
        // after a tool_call (or tool_call_update) must reset to
        // StartNewMessage so the runner finalizes the pre-tool live buffer
        // before appending post-tool free-form text.
        let mut map = ToolCallMap::new();
        let mut state = AcpBoundaryState::new();
        let _ = drain_with_state(
            json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "text": "before" }
            }),
            Path::new("/tmp"),
            &mut map,
            &mut state,
        );
        let _ = drain_with_state(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_x",
                "kind": "execute",
                "rawInput": { "command": ["echo", "x"] }
            }),
            Path::new("/tmp"),
            &mut map,
            &mut state,
        );
        let after = drain_with_state(
            json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "text": "after" }
            }),
            Path::new("/tmp"),
            &mut map,
            &mut state,
        );
        assert_eq!(
            after,
            vec![ClientUpdate::AgentMessageText {
                text: "after".to_string(),
                boundary: AcpTextBoundary::StartNewMessage,
                identity: None,
            }]
        );
    }

    #[test]
    fn dispatch_restarts_no_identity_chunk_across_prompt_turns() {
        // Prompt-turn resets must restart no-identity continuity too:
        // there is no live block left to continue from the prior turn, so
        // the first chunk of a new turn is StartNewMessage even when no
        // tool-call interleaved.
        let mut map = ToolCallMap::new();
        let mut state = AcpBoundaryState::new();
        let _ = drain_with_state(
            json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "text": "turn one" }
            }),
            Path::new("/tmp"),
            &mut map,
            &mut state,
        );
        state.reset_for_prompt_turn();
        let next_turn = drain_with_state(
            json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "text": "turn two" }
            }),
            Path::new("/tmp"),
            &mut map,
            &mut state,
        );
        assert_eq!(
            next_turn,
            vec![ClientUpdate::AgentMessageText {
                text: "turn two".to_string(),
                boundary: AcpTextBoundary::StartNewMessage,
                identity: None,
            }]
        );
    }

    #[test]
    fn dispatch_emits_continue_when_message_identity_persists() {
        // When the ACP payload exposes a stable message identity that
        // matches the previous chunk on the same stream, dispatch must emit
        // `Continue` so the runner can append to the live block. The first
        // chunk on a stream is still `StartNewMessage` because there is no
        // prior live block to continue.
        let mut map = ToolCallMap::new();
        let mut state = AcpBoundaryState::new();
        let first = drain_with_state(
            json!({
                "sessionUpdate": "agent_message_chunk",
                "messageId": "msg-7",
                "content": { "text": "hel" }
            }),
            Path::new("/tmp"),
            &mut map,
            &mut state,
        );
        let second = drain_with_state(
            json!({
                "sessionUpdate": "agent_message_chunk",
                "messageId": "msg-7",
                "content": { "text": "lo" }
            }),
            Path::new("/tmp"),
            &mut map,
            &mut state,
        );
        assert_eq!(
            first,
            vec![ClientUpdate::AgentMessageText {
                text: "hel".to_string(),
                boundary: AcpTextBoundary::StartNewMessage,
                identity: Some("msg-7".to_string()),
            }]
        );
        assert_eq!(
            second,
            vec![ClientUpdate::AgentMessageText {
                text: "lo".to_string(),
                boundary: AcpTextBoundary::Continue,
                identity: Some("msg-7".to_string()),
            }]
        );
    }

    #[test]
    fn dispatch_resets_continuation_after_tool_call_interleave() {
        // A tool_call (or tool_call_update) interleave is a hard boundary:
        // even when the next agent chunk carries the same message identity
        // as the pre-tool chunk, dispatch must emit StartNewMessage so the
        // runner finalizes the pre-tool live buffer instead of gluing
        // synthetic tool text and post-tool free-form text together.
        let mut map = ToolCallMap::new();
        let mut state = AcpBoundaryState::new();
        // Establish a continuation token for msg-7.
        let _ = drain_with_state(
            json!({
                "sessionUpdate": "agent_message_chunk",
                "messageId": "msg-7",
                "content": { "text": "before" }
            }),
            Path::new("/tmp"),
            &mut map,
            &mut state,
        );
        // Tool call interleaves the visible stream.
        let _ = drain_with_state(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_x",
                "kind": "execute",
                "rawInput": { "command": ["echo", "x"] }
            }),
            Path::new("/tmp"),
            &mut map,
            &mut state,
        );
        let after = drain_with_state(
            json!({
                "sessionUpdate": "agent_message_chunk",
                "messageId": "msg-7",
                "content": { "text": "after" }
            }),
            Path::new("/tmp"),
            &mut map,
            &mut state,
        );
        assert_eq!(
            after,
            vec![ClientUpdate::AgentMessageText {
                text: "after".to_string(),
                boundary: AcpTextBoundary::StartNewMessage,
                identity: Some("msg-7".to_string()),
            }]
        );
    }

    #[test]
    fn dispatch_resets_continuation_across_prompt_turns() {
        // Prompt-turn boundaries also clear live logical-message continuity:
        // even if a later turn reuses the same messageId, its first chunk
        // must restart at StartNewMessage because there is no current live
        // block left to continue from the prior turn.
        let mut map = ToolCallMap::new();
        let mut state = AcpBoundaryState::new();
        let _ = drain_with_state(
            json!({
                "sessionUpdate": "agent_message_chunk",
                "messageId": "msg-7",
                "content": { "text": "turn one" }
            }),
            Path::new("/tmp"),
            &mut map,
            &mut state,
        );
        state.reset_for_prompt_turn();
        let next_turn = drain_with_state(
            json!({
                "sessionUpdate": "agent_message_chunk",
                "messageId": "msg-7",
                "content": { "text": "turn two" }
            }),
            Path::new("/tmp"),
            &mut map,
            &mut state,
        );
        assert_eq!(
            next_turn,
            vec![ClientUpdate::AgentMessageText {
                text: "turn two".to_string(),
                boundary: AcpTextBoundary::StartNewMessage,
                identity: Some("msg-7".to_string()),
            }]
        );
    }

    #[test]
    fn dispatch_tool_call_text_is_always_start_new_message() {
        // Tool-call invocation/result text carries StartNewMessage so the
        // runner can finalize the thought stream's current live buffer
        // before appending the synthetic paragraph. This is the same
        // contract whether the tool_call payload arrives terminal or as a
        // separate tool_call_update.
        let mut map = ToolCallMap::new();
        let mut state = AcpBoundaryState::new();
        let updates = drain_with_state(
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_y",
                "kind": "execute",
                "status": "completed",
                "rawInput": { "command": ["echo", "ok"] },
                "rawOutput": { "exit_code": 0, "stdout": "ok" }
            }),
            Path::new("/tmp"),
            &mut map,
            &mut state,
        );
        // Only the visible (text) updates are constrained to StartNewMessage;
        // ToolCallActivity carries no boundary metadata and is intentionally
        // excluded from the assertion below.
        assert!(updates.iter().all(|update| matches!(
            update,
            ClientUpdate::ToolCallText {
                boundary: AcpTextBoundary::StartNewMessage,
                identity: None,
                ..
            } | ClientUpdate::ToolCallActivity { .. }
        )));
        assert!(
            updates
                .iter()
                .any(|update| matches!(update, ClientUpdate::ToolCallText { .. })),
            "tool call should have produced at least one text block"
        );
    }

    #[test]
    fn permission_request_selects_approve_option() {
        let response = client_request_response(
            "session/request_permission",
            &json!({
                "options": [
                    { "optionId": "approve", "kind": "allow_once" },
                    { "optionId": "reject", "kind": "reject_once" }
                ]
            }),
        )
        .expect("permission request should be handled");

        assert_eq!(
            response,
            json!({
                "outcome": {
                    "outcome": "selected",
                    "optionId": "approve"
                }
            })
        );
    }

    #[test]
    fn apply_session_config_uses_baseline_option_snapshot() {
        let mut rpc = StubRpcCaller {
            responses: vec![
                Ok(json!({
                    "configOptions": [{
                        "id": "mode",
                        "category": "mode",
                        "currentValue": "code"
                    }]
                })),
                Ok(json!({
                    "configOptions": [{
                        "id": "model",
                        "category": "model",
                        "currentValue": "model-next"
                    }]
                })),
                Ok(json!({
                    "configOptions": [{
                        "id": "thought_level",
                        "category": "thought_level",
                        "currentValue": "high"
                    }]
                })),
            ],
            ..Default::default()
        };
        let session = sample_session();
        let mut config_options = vec![
            configurable_option("mode", "mode", "ask", &["ask", "code"]),
            configurable_option("model", "model", "model-old", &["model-old", "model-next"]),
            configurable_option(
                "thought_level",
                "thought_level",
                "medium",
                &["medium", "high"],
            ),
        ];

        apply_session_config(&mut rpc, "sess-test", &session, &mut config_options)
            .expect("session config applies");

        assert_eq!(rpc.calls.len(), 3);
        let config_ids = rpc
            .calls
            .iter()
            .map(|(_, params)| {
                params
                    .get("configId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        assert_eq!(config_ids, vec!["mode", "model", "thought_level"]);
    }
}
