//! ACP JSON-RPC stdio client.
//!
//! Wire transport is a tokio actor (`actor`) owning the spawned child's
//! stdio, framed line reader, outstanding-request map, and notification
//! dispatch. The synchronous [`AcpSession`] facade ([`SubprocessSession`])
//! lets sync callers drive prompt turns by polling — `try_recv` on tokio
//! channels and `block_on` on `oneshot::Receiver`s.

mod actor;
#[path = "../acp_support/client_dispatch.rs"]
mod dispatch;
#[path = "../acp_support/client_handshake.rs"]
mod handshake;

use super::{AcpError, AcpResolvedLaunch, AcpResult, ClientUpdate};
use crate::data::acp_support::tool_call::ToolCallMap;
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
        if let Some(queued) = self.pending_updates.pop_front() {
            return Ok(Some(queued));
        }

        // Drain wire messages until either a visible update is queued or the
        // channel runs dry. Non-terminal `tool_call_update`s are merged in
        // silently.
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
        self.boundary_state.reset_for_prompt_turn();
        let prompt_params = prompt_request_params(
            &self.session_id,
            &super::PromptPayload::Text(text.to_string()),
        )?;
        self.prompt_response = Some(self.rpc.start_request("session/prompt", prompt_params)?);
        self.prompt_finished = false;
        Ok(())
    }

    fn cancel_prompt(&mut self) -> AcpResult<()> {
        self.rpc
            .notify("session/cancel", json!({ "sessionId": self.session_id }))
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
            // Best-effort graceful close — drop the receiver so we never block
            // a sync caller waiting on a server that has already disconnected.
            let _ = self
                .rpc
                .start_request("session/close", json!({ "sessionId": self.session_id }));
        }

        let Some(mut child) = self.child.take() else {
            self.rpc.shutdown_blocking_no_child();
            return Ok(());
        };

        // Close the writer half so the actor sees a clean shutdown via
        // command-channel drop, then wait for the actor to drain.
        self.rpc.shutdown_async(&self.runtime);

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
