//! Tokio actor that owns the spawned ACP child's stdio.
use super::{AcpError, AcpResult};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::Child;
use tokio::runtime::Runtime;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
type Pending = HashMap<u64, oneshot::Sender<AcpResult<Value>>>;
type Updates = mpsc::UnboundedSender<AcpResult<Value>>;
type Respond = Option<(u64, oneshot::Sender<AcpResult<Value>>)>;
#[rustfmt::skip]
#[derive(Debug)]
pub(super) enum RpcCommand {
    Send { method: String, params: Value, respond: Respond },
    Shutdown,
}
pub(super) struct RpcClient {
    runtime: Arc<Runtime>,
    cancel: CancellationToken,
    next_request_id: AtomicU64,
    commands: mpsc::UnboundedSender<RpcCommand>,
    updates: mpsc::UnboundedReceiver<AcpResult<Value>>,
    actor: Option<JoinHandle<()>>,
}
#[rustfmt::skip]
impl RpcClient {
    pub(super) fn start<R, W>(runtime: Arc<Runtime>, reader: R, writer: W) -> Self
    where R: AsyncBufRead + Unpin + Send + 'static, W: AsyncWrite + Unpin + Send + 'static
    {
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        let (updates_tx, updates) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let actor = runtime.spawn(actor_loop(reader, writer, commands_rx, updates_tx, cancel.clone()));
        Self {
            runtime, cancel, next_request_id: AtomicU64::new(0),
            commands: commands_tx, updates, actor: Some(actor),
        }
    }
    fn enqueue(&self, method: &str, params: Value, respond: Respond) -> AcpResult<()> {
        let kind = if respond.is_some() { "request" } else { "notification" };
        self.commands.send(RpcCommand::Send { method: method.to_string(), params, respond })
            .map_err(|_| AcpError::io(format!("failed to enqueue ACP {kind} {method}: actor stopped")))
    }
    pub(super) fn start_request(&self, method: &str, params: Value)
        -> AcpResult<oneshot::Receiver<AcpResult<Value>>>
    {
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.enqueue(method, params, Some((id, tx)))?;
        Ok(rx)
    }
    pub(super) fn notify(&self, method: &str, params: Value) -> AcpResult<()> {
        self.enqueue(method, params, None)
    }
    pub(super) async fn call_async(&mut self, method: &str, params: Value) -> AcpResult<Value> {
        self.start_request(method, params)?.await
            .map_err(|_| AcpError::protocol(format!("ACP request {method} channel disconnected")))?
    }
    pub(super) fn try_next_update(&mut self) -> AcpResult<Option<Value>> {
        match self.updates.try_recv() {
            Ok(Ok(v)) => Ok(Some(v)),
            Ok(Err(err)) => Err(err),
            Err(_) => Ok(None),
        }
    }
    fn join_actor(&mut self, runtime: &Runtime) {
        if let Some(actor) = self.actor.take() { let _ = runtime.block_on(actor); }
    }
    pub(super) fn shutdown_blocking_no_child(&mut self) {
        self.cancel.cancel();
        self.join_actor(&self.runtime.clone());
    }
    /// Graceful: queue Shutdown after pending writes, then await actor.
    pub(super) fn shutdown_async(&mut self, runtime: &Runtime) {
        let _ = self.commands.send(RpcCommand::Shutdown);
        self.join_actor(runtime);
    }
    /// Aggressive: kill the actor and reap the child immediately.
    pub(super) fn shutdown_blocking(&mut self, mut child: Child) {
        self.cancel.cancel();
        let runtime = self.runtime.clone();
        self.join_actor(&runtime);
        let _ = runtime.block_on(async {
            let _ = child.kill().await;
            child.wait().await
        });
    }
}
#[rustfmt::skip]
async fn actor_loop<R, W>(
    mut reader: R, mut writer: W,
    mut commands: mpsc::UnboundedReceiver<RpcCommand>,
    updates: Updates, cancel: CancellationToken,
) where R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin
{
    let mut pending: Pending = HashMap::new();
    let mut buf = Vec::new();
    let mut writer_open = true;
    let mut commands_closed = false;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            cmd = commands.recv(), if !commands_closed => {
                let Some(cmd) = cmd else { commands_closed = true; continue; };
                if matches!(cmd, RpcCommand::Shutdown) { break; }
                if let Err(err) = handle_command(cmd, &mut writer, &mut pending, &mut writer_open).await {
                    broadcast_error(&mut pending, &updates, err);
                    break;
                }
            }
            res = reader.read_until(b'\n', &mut buf) => {
                let outcome = match res {
                    Ok(0) => {
                        if !commands_closed {
                            broadcast_error(&mut pending, &updates,
                                AcpError::protocol("ACP agent closed stdout before the prompt turn finished"));
                        }
                        break;
                    }
                    Ok(_) if buf.iter().all(u8::is_ascii_whitespace) => Ok(()),
                    Ok(_) => match std::str::from_utf8(&buf) {
                        Ok(line) => handle_inbound_line(line, &mut writer, &mut pending, &updates, &mut writer_open).await,
                        Err(err) => Err(AcpError::protocol(format!("invalid ACP UTF-8 message: {err}"))),
                    },
                    Err(err) => Err(AcpError::io(format!("failed to read ACP stdout: {err}"))),
                };
                buf.clear();
                if let Err(err) = outcome {
                    broadcast_error(&mut pending, &updates, err);
                    break;
                }
            }
        }
    }
    if writer_open {
        if let Err(e) = writer.flush().await {
            tracing::debug!("ACP writer flush failed: {e}");
        }
        if let Err(e) = writer.shutdown().await {
            tracing::debug!("ACP writer shutdown failed: {e}");
        }
    }
}
#[rustfmt::skip]
async fn handle_command<W>(cmd: RpcCommand, writer: &mut W, pending: &mut Pending, writer_open: &mut bool)
    -> AcpResult<()>
where W: AsyncWrite + Unpin
{
    let RpcCommand::Send { method, params, mut respond } = cmd else { return Ok(()) };
    let kind = if respond.is_some() { "request" } else { "notification" };
    let report = |respond: Respond, err: AcpError| match respond {
        Some((_, tx)) => { let _ = tx.send(Err(err)); Ok(()) }
        None => Err(err),
    };
    if !*writer_open {
        return report(respond, AcpError::io(format!("failed to write ACP {kind} {method}: writer closed")));
    }
    let mut message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
    if let Some((id, _)) = respond.as_ref() { message["id"] = json!(id); }
    if let Err(err) = write_line(writer, &message).await {
        *writer_open = false;
        return report(respond, AcpError::io(format!("failed to write ACP {kind} {method}: {err}")));
    }
    if let Some((id, tx)) = respond.take() { pending.insert(id, tx); }
    Ok(())
}
#[rustfmt::skip]
async fn handle_inbound_line<W>(line: &str, writer: &mut W, pending: &mut Pending,
    updates: &Updates, writer_open: &mut bool) -> AcpResult<()>
where W: AsyncWrite + Unpin
{
    let value: Value = serde_json::from_str(line.trim_end_matches(['\r', '\n']))
        .map_err(|err| AcpError::protocol(format!("invalid ACP JSON message: {err}")))?;
    if let Some(method) = value.get("method").and_then(Value::as_str) {
        if method == "session/update" {
            let _ = updates.send(Ok(value.pointer("/params/update").cloned().unwrap_or(Value::Null)));
            return Ok(());
        }
        if let Some(id) = value.get("id") {
            let response = match value.get("params").and_then(|p| client_request_response(method, p)) {
                Some(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                None => json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32601,
                        "message": format!("codexize client does not implement method {method}") }
                }),
            };
            if *writer_open && let Err(err) = write_line(writer, &response).await {
                *writer_open = false;
                return Err(AcpError::io(format!("failed to write ACP response for {method}: {err}")));
            }
        }
        return Ok(());
    }
    if let Some(id) = value.get("id").and_then(Value::as_u64)
        && let Some(sender) = pending.remove(&id)
    {
        let result = match (value.get("error"), value.get("result")) {
            (Some(err), _) => Err(AcpError::protocol(
                err.get("message").and_then(Value::as_str).unwrap_or("ACP request failed").to_string())),
            (None, Some(r)) => Ok(r.clone()),
            (None, None) => Err(AcpError::protocol("ACP response was missing both result and error".to_string())),
        };
        let _ = sender.send(result);
    }
    Ok(())
}
#[rustfmt::skip]
fn broadcast_error(pending: &mut Pending, updates: &Updates, err: AcpError) {
    for (_, tx) in pending.drain() { let _ = tx.send(Err(err.clone())); }
    let _ = updates.send(Err(err));
}
#[rustfmt::skip]
async fn write_line<W>(writer: &mut W, message: &Value) -> std::io::Result<()>
where W: AsyncWrite + Unpin
{
    let encoded = serde_json::to_string(message)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    writer.write_all(encoded.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}
#[rustfmt::skip]
pub(super) fn client_request_response(method: &str, params: &Value) -> Option<Value> {
    if method != "session/request_permission" { return None; }
    let options = params.get("options").and_then(Value::as_array)?;
    let kind_eq = |o: &&Value, k: &str| o.get("kind").and_then(Value::as_str) == Some(k);
    let approve = options.iter()
        .find(|o| kind_eq(o, "allow_once") || o.get("optionId").and_then(Value::as_str) == Some("approve"))
        .or_else(|| options.iter().find(|o| kind_eq(o, "allow_always")))?;
    let option_id = approve.get("optionId").and_then(Value::as_str)?;
    Some(json!({ "outcome": { "outcome": "selected", "optionId": option_id } }))
}
