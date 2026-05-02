use super::{
    AcpError, AcpResolvedLaunch, AcpResult, ClientUpdate, PromptPayload,
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
        let prompt = match prompt_blocks(&launch.session.prompt) {
            Ok(prompt) => prompt,
            Err(err) => return cleanup_failed_connect(child, rpc, err),
        };
        let prompt_response = match rpc.start_request(
            "session/prompt",
            json!({
                "sessionId": session.session_id,
                "prompt": prompt
            }),
        ) {
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
            pending_updates: VecDeque::new(),
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
    pending_updates: VecDeque<ClientUpdate>,
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
                    dispatch_update(
                        &value,
                        &self.cwd,
                        &mut self.tool_calls,
                        &mut self.pending_updates,
                    );
                    if let Some(queued) = self.pending_updates.pop_front() {
                        return Ok(Some(queued));
                    }
                }
                Ok(None) => break,
                Err(err) => {
                    self.prompt_finished = true;
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
                self.prompt_finished = true;
                self.prompt_response = None;
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
                self.prompt_finished = true;
                self.prompt_response = None;
                Ok(Some(ClientUpdate::PromptTurnFailed {
                    message: err.to_string(),
                }))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                self.prompt_finished = true;
                self.prompt_response = None;
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
        let prompt = prompt_blocks(&PromptPayload::Text(text.to_string()))?;
        let prompt_response = self.rpc.start_request(
            "session/prompt",
            json!({
                "sessionId": self.session_id,
                "prompt": prompt
            }),
        )?;
        self.prompt_response = Some(prompt_response);
        self.prompt_finished = false;
        Ok(())
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
/// `ClientUpdate`s, mutating the per-session tool-call state map in the
/// process. A single `tool_call` payload may yield two updates (invocation
/// followed by result) when its status is already terminal; non-terminal
/// `tool_call_update`s with prior state are absorbed silently and emit
/// nothing.
fn dispatch_update(
    value: &Value,
    cwd: &Path,
    map: &mut ToolCallMap,
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
            out.push_back(ClientUpdate::AgentMessageText(text));
        }
        "agent_thought_chunk" => {
            let text = value
                .pointer("/content/text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            out.push_back(ClientUpdate::AgentThoughtText(text));
        }
        "session_info_update" => {
            out.push_back(ClientUpdate::SessionInfoUpdate {
                title: value
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
        "tool_call" => handle_tool_call(ToolCallPayload::from_value(value), cwd, map, out),
        "tool_call_update" => {
            handle_tool_call_update(ToolCallPayload::from_value(value), map, out);
        }
        other => out.push_back(ClientUpdate::Unknown {
            kind: other.to_string(),
        }),
    }
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
        out.push_back(ClientUpdate::ToolCallText { text: invocation });
        if terminal {
            // Spec §Behavior rule 1: when the same payload carries terminal
            // status, emit the result block immediately afterward and evict.
            out.push_back(ClientUpdate::ToolCallText {
                text: format_result_line(&state),
            });
            map.evict(&id);
        }
    } else {
        // Missing toolCallId: best-effort output only, never stored.
        out.push_back(ClientUpdate::ToolCallText { text: invocation });
        if terminal {
            out.push_back(ClientUpdate::ToolCallText {
                text: format_result_line(&state),
            });
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

    let Some(id) = payload.tool_call_id.clone() else {
        // Missing toolCallId: best-effort result if terminal, otherwise drop.
        if terminal {
            let state = ToolCallDisplayState::from_payload(&payload);
            out.push_back(ClientUpdate::ToolCallText {
                text: format_result_line(&state),
            });
        }
        return;
    };

    if let Some(state) = map.merge(&id, &payload) {
        if terminal {
            let result = format_result_line(state);
            out.push_back(ClientUpdate::ToolCallText { text: result });
            map.evict(&id);
        }
        // Non-terminal merges into prior state and produces no transcript
        // output (spec §Behavior rule 5).
    } else if terminal {
        // No prior state (never created or already evicted): emit a
        // best-effort result block from the payload only; never insert.
        let state = ToolCallDisplayState::from_payload(&payload);
        out.push_back(ClientUpdate::ToolCallText {
            text: format_result_line(&state),
        });
    }
    // Non-terminal update with no prior state is silently dropped.
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

    fn drain(value: Value, cwd: &Path, map: &mut ToolCallMap) -> Vec<ClientUpdate> {
        let mut out = VecDeque::new();
        dispatch_update(&value, cwd, map, &mut out);
        out.into_iter().collect()
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
            vec![ClientUpdate::ToolCallText {
                text: "tool: read(Cargo.toml)".to_string()
            }]
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
            vec![ClientUpdate::ToolCallText {
                text: "tool: read(Cargo.toml)".to_string()
            }]
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
            vec![ClientUpdate::ToolCallText {
                text: "result: completed, exit 0, output: [package] name = \"codexize\""
                    .to_string()
            }]
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
                ClientUpdate::ToolCallText {
                    text: "tool: exec(echo ok)".to_string()
                },
                ClientUpdate::ToolCallText {
                    text: "result: completed, exit 0, output: ok".to_string()
                },
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
            vec![ClientUpdate::ToolCallText {
                text: "result: completed, exit 0, output: ok".to_string()
            }]
        );
        assert!(
            !map.contains("stale_id"),
            "best-effort updates must never insert state"
        );
    }

    #[test]
    fn dispatch_second_terminal_update_for_evicted_id_emits_at_most_best_effort_then_nothing() {
        // Spec §Behavior rule 3: after eviction, a *second* terminal update
        // for the same id is treated as a "no prior state" terminal update.
        // It still produces a best-effort result block (per rule 4) but does
        // not double-emit from the original invocation+result pair, and the
        // total visible blocks for that tool stay capped at two.
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
        assert_eq!(first_result.len(), 1);
        assert!(!map.contains("call_1"));

        // A second terminal update for the now-evicted id is best-effort.
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
        assert_eq!(second_result.len(), 1);
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
            vec![ClientUpdate::ToolCallText {
                text: "tool: exec(cargo test --workspace)".to_string()
            }]
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
        assert_eq!(
            updates,
            vec![ClientUpdate::ToolCallText {
                text: "tool: tool".to_string()
            }]
        );
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
        assert_eq!(
            messages,
            vec![ClientUpdate::AgentMessageText("hello".to_string())]
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
            vec![ClientUpdate::AgentThoughtText("thinking".to_string())]
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
