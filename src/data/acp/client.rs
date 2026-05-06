//! ACP JSON-RPC stdio client.
//!
//! Wire transport is a tokio actor: one task owns the spawned child's stdio,
//! the framed line reader, the outstanding-request map, and notification
//! dispatch. The synchronous [`AcpSession`] facade ([`SubprocessSession`])
//! keeps the existing trait shape so callers that have not yet been ported to
//! async can drive prompt turns by polling — internally the polls hit
//! non-blocking `try_recv` on tokio channels and `block_on` on tokio
//! `oneshot::Receiver`s rather than the previous reader thread and sync
//! request-correlation map.

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
    collections::{HashMap, VecDeque},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, Command},
    runtime::Runtime,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

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

/// Blocking RPC seam used by the legacy `apply_session_config` test stub.
/// Production callers reach the same wire protocol through `RpcClient`'s
/// async `call_async`; the synchronous trait survives only so the existing
/// table-driven test for `apply_session_config` can replace the transport
/// without standing up a tokio runtime.
#[allow(dead_code)]
trait RpcCaller {
    fn call(&mut self, method: &str, params: Value) -> AcpResult<Value>;
}

#[derive(Debug, Clone, Default)]
pub struct SubprocessConnector;

impl AcpConnector for SubprocessConnector {
    fn connect(&self, launch: &AcpResolvedLaunch) -> AcpResult<Box<dyn AcpSession>> {
        let runtime = Arc::new(build_session_runtime()?);
        let (mut rpc, child) = runtime.block_on(spawn_actor(&runtime, launch))?;
        let session = match runtime.block_on(handshake(&mut rpc, launch)) {
            Ok(session) => session,
            Err(err) => {
                rpc.shutdown_blocking(child);
                return Err(err);
            }
        };
        Ok(Box::new(SubprocessSession::new(
            session, rpc, child, runtime, launch,
        )))
    }
}

fn build_session_runtime() -> AcpResult<Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .thread_name("codexize-acp")
        .build()
        .map_err(|err| AcpError::io(format!("failed to build ACP tokio runtime: {err}")))
}

async fn spawn_actor(
    runtime: &Arc<Runtime>,
    launch: &AcpResolvedLaunch,
) -> AcpResult<(RpcClient, Child)> {
    let mut command = Command::new(&launch.spawn.program);
    command
        .args(&launch.spawn.args)
        .envs(&launch.spawn.env)
        .current_dir(&launch.session.cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        // Keep stderr from backing up an unread pipe; protocol diagnostics
        // flow through ACP updates and request failures.
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);

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

    let rpc = RpcClient::start(runtime.clone(), BufReader::new(stdout), stdin);
    Ok((rpc, child))
}

async fn handshake(rpc: &mut RpcClient, launch: &AcpResolvedLaunch) -> AcpResult<HandshakeOutput> {
    let initialize = rpc
        .call_async(
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
        )
        .await?;
    let init = parse_initialize_result(initialize)?;
    if init.protocol_version != 1 {
        return Err(AcpError::human_block(format!(
            "ACP agent negotiated unsupported protocol version {}",
            init.protocol_version
        )));
    }

    let new_session = rpc
        .call_async(
            "session/new",
            json!({
                "cwd": launch.session.cwd,
                "mcpServers": []
            }),
        )
        .await?;
    let mut session = parse_new_session_result(new_session)?;
    apply_session_config_async(
        rpc,
        &session.session_id,
        &launch.session,
        &mut session.config_options,
    )
    .await?;
    let prompt_params = prompt_request_params(&session.session_id, &launch.session.prompt)?;
    let prompt_response = rpc.start_request("session/prompt", prompt_params)?;

    Ok(HandshakeOutput {
        session_id: session.session_id,
        supports_close: init.supports_close,
        prompt_response,
    })
}

struct HandshakeOutput {
    session_id: String,
    supports_close: bool,
    prompt_response: oneshot::Receiver<AcpResult<Value>>,
}

struct SubprocessSession {
    session_id: String,
    rpc: RpcClient,
    child: Option<Child>,
    runtime: Arc<Runtime>,
    supports_close: bool,
    prompt_response: Option<oneshot::Receiver<AcpResult<Value>>>,
    prompt_finished: bool,
    closed: bool,
    cwd: PathBuf,
    tool_calls: ToolCallMap,
    boundary_state: AcpBoundaryState,
    pending_updates: VecDeque<ClientUpdate>,
    acp_trace_path: Option<PathBuf>,
}

impl SubprocessSession {
    fn new(
        handshake: HandshakeOutput,
        rpc: RpcClient,
        child: Child,
        runtime: Arc<Runtime>,
        launch: &AcpResolvedLaunch,
    ) -> Self {
        Self {
            session_id: handshake.session_id,
            rpc,
            child: Some(child),
            runtime,
            supports_close: handshake.supports_close,
            prompt_response: Some(handshake.prompt_response),
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
        }
    }

    fn finish_prompt_turn(&mut self) {
        self.prompt_finished = true;
        self.prompt_response = None;
        self.boundary_state.reset_for_prompt_turn();
    }
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

        let Some(prompt_response) = self.prompt_response.as_mut() else {
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
            Err(oneshot::error::TryRecvError::Empty) => Ok(None),
            Err(oneshot::error::TryRecvError::Closed) => {
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
        self.prompt_response = None;

        if self.supports_close {
            // Best-effort graceful close — ignore the receiver so we never
            // block the sync caller waiting on a server that has already
            // disconnected.
            let _ = self
                .rpc
                .start_request("session/close", json!({ "sessionId": self.session_id }));
        }

        let mut child = match self.child.take() {
            Some(child) => child,
            None => {
                self.rpc.shutdown_blocking_no_child();
                return Ok(());
            }
        };

        // Close the writer half so the actor sees a clean shutdown via
        // command-channel drop, then wait for the actor to drain.
        self.rpc.shutdown_async(&self.runtime);

        // Reap the child after the actor has released stdin. `try_wait` is a
        // sync syscall on tokio's `Child`; only `kill`/`wait` need the
        // executor.
        match child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => {
                let _ = self.runtime.block_on(async {
                    let _ = child.kill().await;
                    child.wait().await
                });
            }
            Err(err) => {
                return Err(AcpError::io(format!(
                    "failed to inspect ACP child process: {err}"
                )));
            }
        }
        Ok(())
    }
}

impl Drop for SubprocessSession {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[derive(Debug)]
enum RpcCommand {
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
/// per-session runtime so that legacy sync callers (`AcpSession`) can keep
/// driving prompt turns by polling without yielding into an executor.
struct RpcClient {
    runtime: Arc<Runtime>,
    cancel: CancellationToken,
    next_request_id: AtomicU64,
    commands: mpsc::UnboundedSender<RpcCommand>,
    updates: mpsc::UnboundedReceiver<AcpResult<Value>>,
    actor: Option<JoinHandle<()>>,
}

impl RpcClient {
    fn start<R, W>(runtime: Arc<Runtime>, reader: R, writer: W) -> Self
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

    fn allocate_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }

    fn start_request(
        &self,
        method: &str,
        params: Value,
    ) -> AcpResult<oneshot::Receiver<AcpResult<Value>>> {
        let id = self.allocate_id();
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

    fn notify(&self, method: &str, params: Value) -> AcpResult<()> {
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

    async fn call_async(&mut self, method: &str, params: Value) -> AcpResult<Value> {
        let rx = self.start_request(method, params)?;
        rx.await
            .map_err(|_| AcpError::protocol(format!("ACP request {method} channel disconnected")))?
    }

    fn try_next_update(&mut self) -> AcpResult<Option<Value>> {
        match self.updates.try_recv() {
            Ok(Ok(value)) => Ok(Some(value)),
            Ok(Err(err)) => Err(err),
            Err(mpsc::error::TryRecvError::Empty) => Ok(None),
            Err(mpsc::error::TryRecvError::Disconnected) => Ok(None),
        }
    }

    /// Cooperative shutdown when a child process has already been reaped or
    /// was never spawned. Cancels the actor and waits for it to exit.
    fn shutdown_blocking_no_child(&mut self) {
        self.cancel.cancel();
        if let Some(actor) = self.actor.take() {
            let _ = self.runtime.block_on(actor);
        }
    }

    /// Cooperative shutdown when the caller already owns the `Child`. Queue a
    /// shutdown command after any best-effort close request so prior commands
    /// can flush before the actor exits; the runtime is needed to await the
    /// join handle on a sync stack.
    fn shutdown_async(&mut self, runtime: &Runtime) {
        // Intentional FIFO shutdown: cancellation is for aggressive teardown,
        // while graceful close should let queued `session/close` reach stdin.
        let _ = self.commands.send(RpcCommand::Shutdown);
        if let Some(actor) = self.actor.take() {
            let _ = runtime.block_on(actor);
        }
    }

    /// Aggressive shutdown used during connect-time failures: kill the actor
    /// and reap the child immediately. The caller is on the connector's
    /// hot path; protocol diagnostics already surfaced as the returned
    /// error, so we drop the child silently.
    fn shutdown_blocking(&mut self, mut child: Child) {
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

#[cfg(test)]
impl RpcCaller for RpcClient {
    fn call(&mut self, method: &str, params: Value) -> AcpResult<Value> {
        let runtime = self.runtime.clone();
        runtime.block_on(self.call_async(method, params))
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
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                break;
            }
            cmd = commands.recv() => {
                let Some(cmd) = cmd else {
                    // No further commands will arrive; keep draining inbound
                    // messages until the server side closes stdout so any
                    // late notifications still reach the consumer. The cancel
                    // token is the explicit termination signal.
                    drain_until_eof(&mut reader, &mut writer, &mut pending, &updates, &cancel).await;
                    break;
                };
                if matches!(cmd, RpcCommand::Shutdown) {
                    break;
                }
                if let Err(err) = handle_command(cmd, &mut writer, &mut pending, &mut writer_open).await {
                    // Writer error: surface as transport failure to all
                    // outstanding requests and exit. The reader is dropped on
                    // function return.
                    broadcast_transport_error(&mut pending, &updates, err);
                    break;
                }
            }
            result = read_line(&mut reader, &mut line_buf) => {
                match result {
                    ReadOutcome::Eof => {
                        broadcast_transport_error(
                            &mut pending,
                            &updates,
                            AcpError::protocol(
                                "ACP agent closed stdout before the prompt turn finished",
                            ),
                        );
                        break;
                    }
                    ReadOutcome::Empty => {
                        line_buf.clear();
                        continue;
                    }
                    ReadOutcome::Line => {
                        match decode_line(&line_buf) {
                            Ok(line) => {
                                if let Err(err) = handle_inbound_line(
                                    line,
                                    &mut writer,
                                    &mut pending,
                                    &updates,
                                    &mut writer_open,
                                )
                                .await
                                {
                                    broadcast_transport_error(&mut pending, &updates, err);
                                    break;
                                }
                            }
                            Err(err) => {
                                line_buf.clear();
                                broadcast_transport_error(&mut pending, &updates, err);
                                break;
                            }
                        }
                        line_buf.clear();
                    }
                    ReadOutcome::Error(err) => {
                        line_buf.clear();
                        broadcast_transport_error(&mut pending, &updates, err);
                        break;
                    }
                }
            }
        }
    }
    // Best-effort writer flush before exit — we want any in-flight responses
    // to client-side requests (e.g. session/request_permission) to make it
    // back to the agent before stdin closes.
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

async fn drain_until_eof<R, W>(
    reader: &mut R,
    writer: &mut W,
    pending: &mut HashMap<u64, oneshot::Sender<AcpResult<Value>>>,
    updates: &mpsc::UnboundedSender<AcpResult<Value>>,
    cancel: &CancellationToken,
) where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut writer_open = true;
    let mut buf = Vec::new();
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            outcome = read_line(reader, &mut buf) => match outcome {
                ReadOutcome::Eof => return,
                ReadOutcome::Empty => {
                    buf.clear();
                    continue;
                }
                ReadOutcome::Line => {
                    match decode_line(&buf) {
                        Ok(line) => {
                            if let Err(err) =
                                handle_inbound_line(line, writer, pending, updates, &mut writer_open).await
                            {
                                broadcast_transport_error(pending, updates, err);
                                return;
                            }
                        }
                        Err(err) => {
                            buf.clear();
                            broadcast_transport_error(pending, updates, err);
                            return;
                        }
                    }
                    buf.clear();
                }
                ReadOutcome::Error(err) => {
                    buf.clear();
                    broadcast_transport_error(pending, updates, err);
                    return;
                }
            }
        }
    }
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
            // Forward the inner `update` field unchanged; the consumer owns
            // the per-session state needed to translate it. Null signals
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

async fn apply_session_config_async(
    rpc: &mut RpcClient,
    session_id: &str,
    session: &super::AcpSessionSpec,
    config_options: &mut Vec<ConfigOption>,
) -> AcpResult<()> {
    for (category, option, value) in selected_config_updates(session, config_options) {
        let response = rpc
            .call_async(
                "session/set_config_option",
                json!({
                    "sessionId": session_id,
                    "configId": option.id,
                    "value": value,
                }),
            )
            .await;
        absorb_config_response(category, &option, &value, response, config_options);
    }
    Ok(())
}

/// Synchronous entry preserved for the `RpcCaller`-based unit tests; production
/// code drives `apply_session_config_async` directly inside the actor's runtime.
#[cfg(test)]
fn apply_session_config(
    rpc: &mut impl RpcCaller,
    session_id: &str,
    session: &super::AcpSessionSpec,
    config_options: &mut Vec<ConfigOption>,
) -> AcpResult<()> {
    for (category, option, value) in selected_config_updates(session, config_options) {
        let response = rpc.call(
            "session/set_config_option",
            json!({
                "sessionId": session_id,
                "configId": option.id,
                "value": value,
            }),
        );
        absorb_config_response(category, &option, &value, response, config_options);
    }
    Ok(())
}

/// Pure planner: pick the (category, option, desired value) tuples to apply
/// against `config_options` for `session`. ACP standardizes categories, not
/// concrete option values, so this mirrors the legacy ask/code convention and
/// falls back to the codexize env contract when an agent exposes different
/// labels.
fn selected_config_updates(
    session: &super::AcpSessionSpec,
    config_options: &[ConfigOption],
) -> Vec<(&'static str, ConfigOption, String)> {
    let desired = [
        ("mode", session.permission_mode.to_string()),
        ("model", session.model.clone()),
        ("thought_level", session.reasoning_effort.to_string()),
    ];
    let baseline = config_options.to_vec();
    desired
        .into_iter()
        .filter_map(|(category, value)| {
            let option = baseline.iter().find(|option| {
                option.category.as_deref() == Some(category) || option.id == category
            })?;
            if option.current_value.as_deref() == Some(value.as_str()) {
                return None;
            }
            if !option.options.is_empty()
                && !option.options.iter().any(|choice| choice.value == value)
            {
                return None;
            }
            Some((category, option.clone(), value))
        })
        .collect()
}

fn absorb_config_response(
    category: &'static str,
    option: &ConfigOption,
    value: &str,
    response: AcpResult<Value>,
    config_options: &mut Vec<ConfigOption>,
) {
    match response {
        Ok(response) => match parse_config_options_response(response) {
            Ok(updated) => *config_options = updated,
            Err(err) => debug_protocol(format!(
                "session/set_config_option response parse failed for category={category} id={}: {err}",
                option.id
            )),
        },
        Err(err) => debug_protocol(format!(
            "session/set_config_option failed for category={category} id={} value={value}: {err}",
            option.id
        )),
    }
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
mod tests;
