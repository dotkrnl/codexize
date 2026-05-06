//! Tokio actor that owns the spawned ACP child's stdio.

use super::super::{AcpError, AcpResult};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt},
    process::Child,
    runtime::Runtime,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(super) enum RpcCommand {
    Request {
        id: u64,
        method: String,
        params: Value,
        respond: oneshot::Sender<AcpResult<Value>>,
    },
    Notify {
        method: String,
        params: Value,
    },
    Shutdown,
}

/// Synchronous handle wrapping the tokio actor task. Threads through the
/// per-session runtime so legacy sync callers can drive prompt turns by
/// polling without yielding into an executor.
pub(super) struct RpcClient {
    runtime: Arc<Runtime>,
    cancel: CancellationToken,
    next_request_id: AtomicU64,
    commands: mpsc::UnboundedSender<RpcCommand>,
    updates: mpsc::UnboundedReceiver<AcpResult<Value>>,
    actor: Option<JoinHandle<()>>,
}

impl RpcClient {
    pub(super) fn start<R, W>(runtime: Arc<Runtime>, reader: R, writer: W) -> Self
    where
        R: AsyncBufRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (commands_tx, commands_rx) = mpsc::unbounded_channel::<RpcCommand>();
        let (updates_tx, updates_rx) = mpsc::unbounded_channel::<AcpResult<Value>>();
        let cancel = CancellationToken::new();
        let actor = runtime.spawn(actor_loop(
            reader,
            writer,
            commands_rx,
            updates_tx,
            cancel.clone(),
        ));
        Self {
            runtime,
            cancel,
            next_request_id: AtomicU64::new(0),
            commands: commands_tx,
            updates: updates_rx,
            actor: Some(actor),
        }
    }

    pub(super) fn start_request(
        &self,
        method: &str,
        params: Value,
    ) -> AcpResult<oneshot::Receiver<AcpResult<Value>>> {
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let (respond, rx) = oneshot::channel();
        self.commands
            .send(RpcCommand::Request {
                id,
                method: method.to_string(),
                params,
                respond,
            })
            .map_err(|_| {
                AcpError::io(format!(
                    "failed to enqueue ACP request {method}: actor stopped"
                ))
            })?;
        Ok(rx)
    }

    pub(super) fn notify(&self, method: &str, params: Value) -> AcpResult<()> {
        self.commands
            .send(RpcCommand::Notify {
                method: method.to_string(),
                params,
            })
            .map_err(|_| {
                AcpError::io(format!(
                    "failed to enqueue ACP notification {method}: actor stopped"
                ))
            })
    }

    pub(super) async fn call_async(&mut self, method: &str, params: Value) -> AcpResult<Value> {
        let rx = self.start_request(method, params)?;
        rx.await
            .map_err(|_| AcpError::protocol(format!("ACP request {method} channel disconnected")))?
    }

    pub(super) fn try_next_update(&mut self) -> AcpResult<Option<Value>> {
        match self.updates.try_recv() {
            Ok(Ok(value)) => Ok(Some(value)),
            Ok(Err(err)) => Err(err),
            Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                Ok(None)
            }
        }
    }

    /// Cooperative shutdown when no `Child` is owned. Cancels the actor and
    /// waits for it to exit.
    pub(super) fn shutdown_blocking_no_child(&mut self) {
        self.cancel.cancel();
        if let Some(actor) = self.actor.take() {
            let _ = self.runtime.block_on(actor);
        }
    }

    /// Graceful shutdown: queue a `Shutdown` after any pending close request,
    /// then await the actor on `runtime`.
    pub(super) fn shutdown_async(&mut self, runtime: &Runtime) {
        let _ = self.commands.send(RpcCommand::Shutdown);
        if let Some(actor) = self.actor.take() {
            let _ = runtime.block_on(actor);
        }
    }

    /// Aggressive shutdown for connect-time failures: kill the actor and
    /// reap the child immediately.
    pub(super) fn shutdown_blocking(&mut self, mut child: Child) {
        self.cancel.cancel();
        if let Some(actor) = self.actor.take() {
            let _ = self.runtime.block_on(actor);
        }
        let _ = self.runtime.block_on(async {
            let _ = child.kill().await;
            child.wait().await
        });
    }
}

async fn actor_loop<R, W>(
    mut reader: R,
    mut writer: W,
    mut commands: mpsc::UnboundedReceiver<RpcCommand>,
    updates: mpsc::UnboundedSender<AcpResult<Value>>,
    cancel: CancellationToken,
) where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut pending: HashMap<u64, oneshot::Sender<AcpResult<Value>>> = HashMap::new();
    let mut line_buf = Vec::new();
    let mut writer_open = true;
    // After commands is closed we keep draining inbound messages so late
    // notifications still reach the consumer; cancel is the explicit kill.
    let mut commands_closed = false;
    'outer: loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            cmd = commands.recv(), if !commands_closed => {
                let Some(cmd) = cmd else {
                    commands_closed = true;
                    continue;
                };
                if matches!(cmd, RpcCommand::Shutdown) {
                    break;
                }
                if let Err(err) = handle_command(cmd, &mut writer, &mut pending, &mut writer_open).await {
                    broadcast_transport_error(&mut pending, &updates, err);
                    break;
                }
            }
            outcome = read_line(&mut reader, &mut line_buf) => {
                match outcome {
                    ReadOutcome::Eof => {
                        if !commands_closed {
                            broadcast_transport_error(
                                &mut pending,
                                &updates,
                                AcpError::protocol(
                                    "ACP agent closed stdout before the prompt turn finished",
                                ),
                            );
                        }
                        break 'outer;
                    }
                    ReadOutcome::Empty => line_buf.clear(),
                    ReadOutcome::Line => {
                        let res = match decode_line(&line_buf) {
                            Ok(line) => {
                                handle_inbound_line(
                                    line,
                                    &mut writer,
                                    &mut pending,
                                    &updates,
                                    &mut writer_open,
                                )
                                .await
                            }
                            Err(err) => Err(err),
                        };
                        line_buf.clear();
                        if let Err(err) = res {
                            broadcast_transport_error(&mut pending, &updates, err);
                            break 'outer;
                        }
                    }
                    ReadOutcome::Error(err) => {
                        line_buf.clear();
                        broadcast_transport_error(&mut pending, &updates, err);
                        break 'outer;
                    }
                }
            }
        }
    }
    if writer_open {
        let _ = writer.flush().await;
        let _ = writer.shutdown().await;
    }
}

enum ReadOutcome {
    Eof,
    Empty,
    Line,
    Error(AcpError),
}

async fn read_line<R>(reader: &mut R, buf: &mut Vec<u8>) -> ReadOutcome
where
    R: AsyncBufRead + Unpin,
{
    // `read_until` is cancellation-safe under `tokio::select!`; `read_line`
    // can drop partial JSON if an outbound command wins the select branch.
    match reader.read_until(b'\n', buf).await {
        Ok(0) => ReadOutcome::Eof,
        Ok(_) if buf.iter().all(|byte| byte.is_ascii_whitespace()) => ReadOutcome::Empty,
        Ok(_) => ReadOutcome::Line,
        Err(err) => ReadOutcome::Error(AcpError::io(format!("failed to read ACP stdout: {err}"))),
    }
}

fn decode_line(buf: &[u8]) -> AcpResult<&str> {
    std::str::from_utf8(buf)
        .map_err(|err| AcpError::protocol(format!("invalid ACP UTF-8 message: {err}")))
}

async fn handle_command<W>(
    cmd: RpcCommand,
    writer: &mut W,
    pending: &mut HashMap<u64, oneshot::Sender<AcpResult<Value>>>,
    writer_open: &mut bool,
) -> AcpResult<()>
where
    W: AsyncWrite + Unpin,
{
    match cmd {
        RpcCommand::Request {
            id,
            method,
            params,
            respond,
        } => {
            let message = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params
            });
            if !*writer_open {
                let _ = respond.send(Err(AcpError::io(format!(
                    "failed to write ACP request {method}: writer closed"
                ))));
                return Ok(());
            }
            if let Err(err) = write_json_rpc_line(writer, &message).await {
                *writer_open = false;
                let _ = respond.send(Err(AcpError::io(format!(
                    "failed to write ACP request {method}: {err}"
                ))));
                return Ok(());
            }
            pending.insert(id, respond);
            Ok(())
        }
        RpcCommand::Notify { method, params } => {
            if !*writer_open {
                return Err(AcpError::io(format!(
                    "failed to write ACP notification {method}: writer closed"
                )));
            }
            let message = json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params
            });
            if let Err(err) = write_json_rpc_line(writer, &message).await {
                *writer_open = false;
                return Err(AcpError::io(format!(
                    "failed to write ACP notification {method}: {err}"
                )));
            }
            Ok(())
        }
        RpcCommand::Shutdown => Ok(()),
    }
}

async fn handle_inbound_line<W>(
    line: &str,
    writer: &mut W,
    pending: &mut HashMap<u64, oneshot::Sender<AcpResult<Value>>>,
    updates: &mpsc::UnboundedSender<AcpResult<Value>>,
    writer_open: &mut bool,
) -> AcpResult<()>
where
    W: AsyncWrite + Unpin,
{
    let value: Value = serde_json::from_str(line.trim_end_matches(['\r', '\n']))
        .map_err(|err| AcpError::protocol(format!("invalid ACP JSON message: {err}")))?;

    if let Some(method) = value.get("method").and_then(Value::as_str) {
        if method == "session/update" {
            // Forward the inner `update` field unchanged. Null signals
            // "session/update without an update field" so the dispatcher can
            // emit Unknown { kind: "session/update" }.
            let update_value = value
                .get("params")
                .and_then(|params| params.get("update"))
                .cloned()
                .unwrap_or(Value::Null);
            let _ = updates.send(Ok(update_value));
            return Ok(());
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
            if *writer_open && let Err(err) = write_json_rpc_line(writer, &response).await {
                *writer_open = false;
                return Err(AcpError::io(format!(
                    "failed to write ACP response for {method}: {err}"
                )));
            }
        }
        return Ok(());
    }

    if let Some(id) = value.get("id").and_then(Value::as_u64)
        && let Some(sender) = pending.remove(&id)
    {
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
    Ok(())
}

fn broadcast_transport_error(
    pending: &mut HashMap<u64, oneshot::Sender<AcpResult<Value>>>,
    updates: &mpsc::UnboundedSender<AcpResult<Value>>,
    err: AcpError,
) {
    for (_, sender) in pending.drain() {
        let _ = sender.send(Err(err.clone()));
    }
    let _ = updates.send(Err(err));
}

async fn write_json_rpc_line<W>(writer: &mut W, message: &Value) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let encoded = serde_json::to_string(message)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    writer.write_all(encoded.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

pub(super) fn client_request_response(method: &str, params: &Value) -> Option<Value> {
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
