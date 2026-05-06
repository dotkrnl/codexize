//! Agent process supervision for codexize.
//!
//! The runner owns every agent child-process launch and the finish-stamp
//! contract that follows. Internals are split across three submodules:
//! [`transport`] (ACP channels, text accumulation, trace IO), [`exit`]
//! (finish stamps, git-state probes, exit-policy validators), and
//! [`supervise`] (the per-run loop, the active-run registry, and the
//! public control surface).

use crate::acp::AcpLaunchPolicy;
use crate::adapters::AgentRun;
use crate::selection::VendorKind;
use anyhow::{Context, Result};
use std::{
    path::Path,
    process::{ExitStatus, Stdio},
    time::Duration,
};
use tokio::process::Command;

mod exit;
mod supervise;
mod transport;

pub use exit::{FinishStamp, read_finish_stamp, validate_toml_artifacts, write_finish_stamp};
pub use supervise::{RunId, Supervisor};

#[cfg(test)]
pub use supervise::{
    cancel_run_labels_matching, drain_test_cancel_receiver_for, drain_test_input_receiver_for,
    force_interrupt_run_label, interrupt_run_label_input, register_test_run_id,
    request_run_label_active_for_test, request_run_label_exit,
    request_run_label_interactive_input_for_test, run_label_is_active,
    run_label_is_waiting_for_input, send_run_label_input, shutdown_all_runs, terminate_run_label,
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

/// Launch an agent interactively inside the managed ACP runtime boundary.
/// All agent child-process launches must route through the runner so that
/// finish-stamp logic is guaranteed to run.
impl Supervisor {
    #[allow(clippy::too_many_arguments)]
    pub fn launch_interactive(
        &self,
        run_id: RunId,
        window_name: &str,
        run: &AgentRun,
        vendor: VendorKind,
        run_key: &str,
        artifacts_dir: &Path,
        required_artifact: Option<&Path>,
    ) -> Result<()> {
        self.launch_managed(
            run_id,
            window_name,
            run,
            vendor,
            run_key,
            artifacts_dir,
            required_artifact,
            true,
            AcpLaunchPolicy::default(),
        )
    }

    /// Launch an agent non-interactively inside the managed ACP runtime boundary.
    /// All agent child-process launches must route through the runner so that
    /// finish-stamp logic is guaranteed to run.
    #[allow(clippy::too_many_arguments)]
    pub fn launch_noninteractive(
        &self,
        run_id: RunId,
        window_name: &str,
        run: &AgentRun,
        vendor: VendorKind,
        run_key: &str,
        artifacts_dir: &Path,
        required_artifact: Option<&Path>,
    ) -> Result<()> {
        self.launch_managed(
            run_id,
            window_name,
            run,
            vendor,
            run_key,
            artifacts_dir,
            required_artifact,
            false,
            AcpLaunchPolicy::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn launch_noninteractive_with_policy(
        &self,
        run_id: RunId,
        window_name: &str,
        run: &AgentRun,
        vendor: VendorKind,
        run_key: &str,
        artifacts_dir: &Path,
        required_artifact: Option<&Path>,
        policy: AcpLaunchPolicy,
    ) -> Result<()> {
        self.launch_managed(
            run_id,
            window_name,
            run,
            vendor,
            run_key,
            artifacts_dir,
            required_artifact,
            false,
            policy,
        )
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn launch_interactive(
    window_name: &str,
    run: &AgentRun,
    vendor: VendorKind,
    run_key: &str,
    artifacts_dir: &Path,
    required_artifact: Option<&Path>,
) -> Result<()> {
    let run_id = supervise::assign_test_run_id(window_name);
    supervise::test_supervisor().launch_interactive(
        run_id,
        window_name,
        run,
        vendor,
        run_key,
        artifacts_dir,
        required_artifact,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn launch_noninteractive(
    window_name: &str,
    run: &AgentRun,
    vendor: VendorKind,
    run_key: &str,
    artifacts_dir: &Path,
    required_artifact: Option<&Path>,
) -> Result<()> {
    let run_id = supervise::assign_test_run_id(window_name);
    supervise::test_supervisor().launch_noninteractive(
        run_id,
        window_name,
        run,
        vendor,
        run_key,
        artifacts_dir,
        required_artifact,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn launch_noninteractive_with_policy(
    window_name: &str,
    run: &AgentRun,
    vendor: VendorKind,
    run_key: &str,
    artifacts_dir: &Path,
    required_artifact: Option<&Path>,
    policy: AcpLaunchPolicy,
) -> Result<()> {
    let run_id = supervise::assign_test_run_id(window_name);
    supervise::test_supervisor().launch_noninteractive_with_policy(
        run_id,
        window_name,
        run,
        vendor,
        run_key,
        artifacts_dir,
        required_artifact,
        policy,
    )
}

/// Spawn a child process and await its exit, returning `None` if the timeout
/// elapsed (in which case the child is killed and reaped) or its exit status
/// otherwise.
///
/// Sync wrapper preserved for [`crate::data::providers::kimi::resolve_api_key`]
/// — the only remaining sync caller in product code. The supervisor's own
/// run-tail moved to [`run_child_with_timeout_async`] in this round; the
/// kimi/quota chain (`selection_quota::load_quota_maps_for`) is the next
/// async-migration boundary, after which this wrapper goes away.
pub fn run_child_with_timeout(
    launch: &ChildLaunch,
    timeout: Duration,
) -> Result<Option<ExitStatus>> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| {
            handle.block_on(run_child_with_timeout_async(launch, timeout))
        })
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build runner helper runtime")
            .block_on(run_child_with_timeout_async(launch, timeout))
    }
}

pub async fn run_child_with_timeout_async(
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
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => Ok(Some(status)),
        // A `wait()` failure here (e.g. the child was reaped elsewhere) is
        // observationally indistinguishable from "the supervised work did not
        // complete cleanly within the budget" for every existing caller, which
        // only inspects the `Some(status)` shape. Return `Ok(None)` to keep the
        // pre-async semantics rather than surfacing a transient OS error.
        Ok(Err(_)) => Ok(None),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests_mod;
