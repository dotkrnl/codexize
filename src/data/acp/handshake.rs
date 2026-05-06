//! ACP handshake: spawn the agent, perform `initialize` / `session/new`,
//! apply config options, and start the first prompt turn.

use super::actor::RpcClient;
use super::{AcpError, AcpResolvedLaunch, AcpResult, AcpSessionSpec, PromptPayload};
use crate::selection::vendor::vendor_kind_to_str;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::{
    io::BufReader,
    process::{Child, Command},
    runtime::Runtime,
    sync::oneshot,
};

pub(super) fn build_session_runtime() -> AcpResult<Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .thread_name("codexize-acp")
        .build()
        .map_err(|err| AcpError::io(format!("failed to build ACP tokio runtime: {err}")))
}

pub(super) async fn spawn_actor(
    runtime: &Arc<Runtime>,
    launch: &AcpResolvedLaunch,
) -> AcpResult<(RpcClient, Child)> {
    let mut child = Command::new(&launch.spawn.program)
        .args(&launch.spawn.args)
        .envs(&launch.spawn.env)
        .current_dir(&launch.session.cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| {
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

pub(super) struct HandshakeOutput {
    pub(super) session_id: String,
    pub(super) supports_close: bool,
    pub(super) prompt_response: oneshot::Receiver<AcpResult<Value>>,
}

pub(super) async fn handshake(
    rpc: &mut RpcClient,
    launch: &AcpResolvedLaunch,
) -> AcpResult<HandshakeOutput> {
    let init = parse_initialize_result(
        rpc.call_async(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false
                },
                "clientInfo": {
                    "name": "codexize",
                    "title": "codexize",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )
        .await?,
    )?;
    if init.protocol_version != 1 {
        return Err(AcpError::human_block(format!(
            "ACP agent negotiated unsupported protocol version {}",
            init.protocol_version
        )));
    }
    let new_session = rpc
        .call_async(
            "session/new",
            json!({ "cwd": launch.session.cwd, "mcpServers": [] }),
        )
        .await?;
    let mut session: NewSessionResult = serde_json::from_value(new_session).map_err(|err| {
        AcpError::protocol(format!("failed to parse ACP session/new response: {err}"))
    })?;
    apply_session_config_async(
        rpc,
        &session.session_id,
        &launch.session,
        &mut session.config_options,
    )
    .await;
    let prompt_response = rpc.start_request(
        "session/prompt",
        prompt_request_params(&session.session_id, &launch.session.prompt)?,
    )?;
    Ok(HandshakeOutput {
        session_id: session.session_id,
        supports_close: init.supports_close,
        prompt_response,
    })
}

#[derive(Debug)]
pub(super) struct InitializeOutcome {
    pub(super) protocol_version: u64,
    pub(super) supports_close: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct NewSessionResult {
    #[serde(rename = "sessionId")]
    pub(super) session_id: String,
    #[serde(rename = "configOptions", default)]
    pub(super) config_options: Vec<ConfigOption>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ConfigOption {
    pub(super) id: String,
    #[serde(default)]
    pub(super) category: Option<String>,
    #[serde(rename = "currentValue", default)]
    pub(super) current_value: Option<String>,
    #[serde(default)]
    pub(super) options: Vec<ConfigChoice>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ConfigChoice {
    pub(super) value: String,
}

pub(super) fn parse_initialize_result(value: Value) -> AcpResult<InitializeOutcome> {
    Ok(InitializeOutcome {
        protocol_version: value
            .get("protocolVersion")
            .and_then(Value::as_u64)
            .ok_or_else(|| AcpError::protocol("ACP initialize response missing protocolVersion"))?,
        supports_close: value
            .pointer("/agentCapabilities/sessionCapabilities/close")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PromptTurnOutcome {
    Finished,
    Failed { message: String },
}

pub(super) fn parse_prompt_result(value: Value) -> AcpResult<PromptTurnOutcome> {
    let stop = value
        .get("stopReason")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::protocol("ACP prompt response missing stopReason"))?;
    Ok(
        if matches!(
            stop,
            "cancelled"
                | "canceled"
                | "interrupted"
                | "error"
                | "errored"
                | "failed"
                | "timeout"
                | "timed_out"
        ) {
            PromptTurnOutcome::Failed {
                message: format!("ACP prompt turn failed with stopReason={stop}"),
            }
        } else {
            PromptTurnOutcome::Finished
        },
    )
}

pub(super) fn prompt_request_params(session_id: &str, prompt: &PromptPayload) -> AcpResult<Value> {
    let text = match prompt {
        PromptPayload::Text(text) => text.clone(),
        PromptPayload::File(path) => std::fs::read_to_string(path).map_err(|err| {
            AcpError::io(format!(
                "failed to read ACP prompt payload {}: {err}",
                path.display()
            ))
        })?,
    };
    Ok(json!({
        "sessionId": session_id,
        "messageId": uuid::Uuid::new_v4().to_string(),
        "prompt": [{ "type": "text", "text": text }],
    }))
}

async fn apply_session_config_async(
    rpc: &mut RpcClient,
    session_id: &str,
    session: &AcpSessionSpec,
    config_options: &mut Vec<ConfigOption>,
) {
    for (category, option, value) in selected_config_updates(session, config_options) {
        let response = rpc
            .call_async(
                "session/set_config_option",
                json!({ "sessionId": session_id, "configId": option.id, "value": value }),
            )
            .await
            .and_then(parse_config_options_response);
        match response {
            Ok(updated) => *config_options = updated,
            Err(err) => tracing::debug!(
                target: "codexize::acp",
                "session/set_config_option failed for category={category} id={} value={value}: {err}",
                option.id
            ),
        }
    }
}

/// Pick (category, option, desired value) tuples to apply against
/// `config_options` for `session`.
fn selected_config_updates(
    session: &AcpSessionSpec,
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
            let option = baseline
                .iter()
                .find(|o| o.category.as_deref() == Some(category) || o.id == category)?;
            if option.current_value.as_deref() == Some(value.as_str()) {
                return None;
            }
            if !option.options.is_empty() && !option.options.iter().any(|c| c.value == value) {
                return None;
            }
            Some((category, option.clone(), value))
        })
        .collect()
}

fn parse_config_options_response(value: Value) -> AcpResult<Vec<ConfigOption>> {
    #[derive(Deserialize)]
    struct Wrap {
        #[serde(rename = "configOptions", default)]
        config_options: Vec<ConfigOption>,
    }
    serde_json::from_value::<Wrap>(value)
        .map(|w| w.config_options)
        .map_err(|err| {
            AcpError::protocol(format!(
                "failed to parse ACP session/set_config_option response: {err}"
            ))
        })
}
