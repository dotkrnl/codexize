use super::{AcpError, AcpResolvedLaunch, AcpResult, ClientUpdate, PromptPayload};
use crate::selection::vendor::vendor_kind_to_str;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
};

type PendingRequests = Arc<Mutex<BTreeMap<u64, Sender<AcpResult<Value>>>>>;

pub trait AcpSession: Send {
    fn session_id(&self) -> &str;
    fn try_next_update(&mut self) -> AcpResult<Option<ClientUpdate>>;
    fn close(&mut self) -> AcpResult<()>;
}

pub trait AcpConnector {
    fn connect(&self, launch: &AcpResolvedLaunch) -> AcpResult<Box<dyn AcpSession>>;
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
            .stderr(Stdio::piped());

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

        let initialize = rpc.call(
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
        )?;
        let init = parse_initialize_result(initialize)?;
        if init.protocol_version != 1 {
            return Err(AcpError::human_block(format!(
                "ACP agent negotiated unsupported protocol version {}",
                init.protocol_version
            )));
        }

        let new_session = rpc.call(
            "session/new",
            json!({
                "cwd": launch.session.cwd,
                "mcpServers": []
            }),
        )?;
        let mut session = parse_new_session_result(new_session)?;
        apply_session_config(
            &mut rpc,
            &session.session_id,
            &launch.session,
            &mut session.config_options,
        )?;
        let prompt_response = rpc.start_request(
            "session/prompt",
            json!({
                "sessionId": session.session_id,
                "prompt": prompt_blocks(&launch.session.prompt)?
            }),
        )?;

        Ok(Box::new(SubprocessSession {
            session_id: session.session_id,
            rpc,
            child: Some(child),
            supports_close: init.supports_close,
            prompt_response,
            prompt_finished: false,
            closed: false,
        }))
    }
}

struct SubprocessSession {
    session_id: String,
    rpc: RpcPeer,
    child: Option<Child>,
    supports_close: bool,
    prompt_response: Receiver<AcpResult<Value>>,
    prompt_finished: bool,
    closed: bool,
}

impl AcpSession for SubprocessSession {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn try_next_update(&mut self) -> AcpResult<Option<ClientUpdate>> {
        match self.rpc.try_next_update()? {
            Some(update) => return Ok(Some(update)),
            None if !self.prompt_finished => {}
            None => return Ok(None),
        }

        match self.prompt_response.try_recv() {
            Ok(Ok(result)) => {
                self.prompt_finished = true;
                parse_prompt_result(result)?;
                Ok(Some(ClientUpdate::PromptTurnFinished))
            }
            Ok(Err(err)) => {
                self.prompt_finished = true;
                Ok(Some(ClientUpdate::PromptTurnFailed {
                    message: err.to_string(),
                }))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                self.prompt_finished = true;
                Ok(Some(ClientUpdate::PromptTurnFailed {
                    message: "ACP prompt turn channel disconnected".to_string(),
                }))
            }
        }
    }

    fn close(&mut self) -> AcpResult<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;

        if self.supports_close {
            let _ = self
                .rpc
                .call("session/close", json!({ "sessionId": self.session_id }));
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
    writer: Option<ChildStdin>,
    pending: PendingRequests,
    updates_rx: Receiver<AcpResult<ClientUpdate>>,
    reader_handle: Option<JoinHandle<()>>,
    next_request_id: u64,
}

impl RpcPeer {
    fn new(stdin: ChildStdin, stdout: ChildStdout) -> Self {
        let pending = Arc::new(Mutex::new(BTreeMap::<u64, Sender<AcpResult<Value>>>::new()));
        let (updates_tx, updates_rx) = mpsc::channel();
        let reader_pending = Arc::clone(&pending);
        let reader_handle = thread::spawn(move || read_loop(stdout, reader_pending, updates_tx));
        Self {
            writer: Some(stdin),
            pending,
            updates_rx,
            reader_handle: Some(reader_handle),
            next_request_id: 0,
        }
    }

    fn call(&mut self, method: &str, params: Value) -> AcpResult<Value> {
        let receiver = self.start_request(method, params)?;
        receiver
            .recv()
            .map_err(|_| AcpError::protocol(format!("ACP request {method} channel disconnected")))?
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
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| AcpError::protocol("ACP transport writer already closed"))?;
        let encoded = serde_json::to_string(&message)
            .map_err(|err| AcpError::protocol(format!("failed to encode ACP request: {err}")))?;
        writer
            .write_all(encoded.as_bytes())
            .and_then(|_| writer.write_all(b"\n"))
            .and_then(|_| writer.flush())
            .map_err(|err| AcpError::io(format!("failed to write ACP request {method}: {err}")))?;
        Ok(rx)
    }

    fn try_next_update(&mut self) -> AcpResult<Option<ClientUpdate>> {
        match self.updates_rx.try_recv() {
            Ok(Ok(update)) => Ok(Some(update)),
            Ok(Err(err)) => Err(err),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Ok(None),
        }
    }

    fn shutdown(&mut self) {
        self.writer.take();
        if let Some(handle) = self.reader_handle.take() {
            let _ = handle.join();
        }
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }
}

fn read_loop(
    stdout: ChildStdout,
    pending: PendingRequests,
    updates_tx: Sender<AcpResult<ClientUpdate>>,
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

        if value.get("method").and_then(Value::as_str) == Some("session/update") {
            let update = value
                .get("params")
                .and_then(|params| params.get("update"))
                .map(parse_update)
                .unwrap_or_else(|| ClientUpdate::Unknown {
                    kind: "session/update".to_string(),
                });
            let _ = updates_tx.send(Ok(update));
            continue;
        }

        if let Some(method) = value.get("method").and_then(Value::as_str) {
            let _ = updates_tx.send(Err(AcpError::protocol(format!(
                "unsupported ACP agent request {method}; codexize client methods are not wired yet",
            ))));
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
    updates_tx: &Sender<AcpResult<ClientUpdate>>,
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

fn parse_prompt_result(value: Value) -> AcpResult<()> {
    let stop_reason = value
        .get("stopReason")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::protocol("ACP prompt response missing stopReason"))?;
    let _ = stop_reason;
    Ok(())
}

fn parse_update(value: &Value) -> ClientUpdate {
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
            ClientUpdate::AgentMessageText(text)
        }
        "agent_thought_chunk" => {
            let text = value
                .pointer("/content/text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            ClientUpdate::AgentThoughtText(text)
        }
        "session_info_update" => ClientUpdate::SessionInfoUpdate {
            title: value
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string),
        },
        other => ClientUpdate::Unknown {
            kind: other.to_string(),
        },
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

fn apply_session_config(
    rpc: &mut RpcPeer,
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

    for (category, value) in desired {
        let Some(option) = config_options
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
            Err(_) => continue,
        };
        *config_options = parse_config_options_response(response)?;
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
