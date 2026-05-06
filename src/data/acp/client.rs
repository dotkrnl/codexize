//! ACP JSON-RPC stdio client.
//!
//! Wire transport is a tokio actor (`actor`) that owns the spawned child's
//! stdio, the framed line reader, the outstanding-request map, and notification
//! dispatch. The synchronous [`AcpSession`] facade ([`SubprocessSession`])
//! keeps the existing trait shape so callers that have not yet been ported to
//! async can drive prompt turns by polling — internally the polls hit
//! non-blocking `try_recv` on tokio channels and `block_on` on tokio
//! `oneshot::Receiver`s rather than the previous reader thread and sync
//! request-correlation map.
//!
//! The bulk of the implementation lives in three submodules:
//!
//! * [`actor`] — `RpcClient` + `actor_loop` (transport, framing, server
//!   request handling, shutdown).
//! * [`dispatch`] — translation of `session/update` payloads into
//!   `ClientUpdate`s, including text-boundary classification.
//! * [`handshake`] — `initialize` / `session/new` / config-option negotiation
//!   and prompt-request shape.

mod actor;
mod dispatch;
mod handshake;

use super::{AcpError, AcpResolvedLaunch, AcpResult, ClientUpdate, tool_call::ToolCallMap};
use actor::RpcClient;
use dispatch::{AcpBoundaryState, dispatch_update};
use handshake::{
    HandshakeOutput, PromptTurnOutcome, build_session_runtime, handshake, parse_prompt_result,
    prompt_request_params, spawn_actor,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{process::Child, runtime::Runtime, sync::oneshot};

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
#[cfg(test)]
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
        let prompt_params = prompt_request_params(
            &self.session_id,
            &super::PromptPayload::Text(text.to_string()),
        )?;
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

#[cfg(test)]
mod tests;
