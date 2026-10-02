use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use homeboy_core::cooperative_control::CooperativeControl;
use homeboy_core::error::{Error, Result};
use homeboy_engine_primitives::command::{self, ExecutionOwner};

/// Invocation-owned control. No ambient cancellation state is consulted by
/// filesystem traversal; ordinary reusable workspace callers remain unbounded.
#[derive(Clone, Default)]
pub(crate) struct WorkspaceControl(Option<CooperativeControl>);

impl WorkspaceControl {
    pub(crate) fn new(control: CooperativeControl) -> Self {
        Self(Some(control))
    }

    pub(crate) fn before(deadline: Option<Instant>) -> Self {
        Self(deadline.map(|deadline| CooperativeControl::new(deadline, Arc::new(|| false))))
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.0.as_ref().map(CooperativeControl::deadline)
    }

    pub(crate) fn checkpoint(&self) -> Result<()> {
        if self
            .0
            .as_ref()
            .is_some_and(CooperativeControl::cancellation_requested)
        {
            let mut error = Error::internal_unexpected("workspace synchronization was cancelled");
            error.details["workspace_sync"] = serde_json::json!({"cancelled": true});
            return Err(error.with_retryable(false));
        }
        if self
            .deadline()
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(super::util::workspace_preparation_timeout(
                "workspace snapshot staging",
                Duration::ZERO,
            ));
        }
        Ok(())
    }

    /// Own and reap exactly this invocation's process scope. A stopped SSH
    /// transport does not prove the remote installation absent: retain that
    /// uncertainty on the returned error for the staging custody owner.
    pub(crate) fn output(
        &self,
        process: &mut Command,
        action: &str,
    ) -> Result<std::process::Output> {
        process
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.wait_output(process, action)
    }

    /// Git identities and object closures need complete stdout. Spool that
    /// evidence to owned scratch while keeping child supervision output bounded.
    pub(crate) fn output_exact(
        &self,
        process: &mut Command,
        action: &str,
    ) -> Result<std::process::Output> {
        self.checkpoint()?;
        let file = tempfile::NamedTempFile::new()
            .map_err(|error| Error::internal_io(error.to_string(), Some(action.to_string())))?;
        let mut output = self.output_to_file(
            process,
            file.reopen()
                .map_err(|error| Error::internal_io(error.to_string(), None))?,
            action,
        )?;
        let mut reader = file
            .reopen()
            .map_err(|error| Error::internal_io(error.to_string(), None))?;
        let mut buffer = [0; 64 * 1024];
        loop {
            self.checkpoint()?;
            let count = reader
                .read(&mut buffer)
                .map_err(|error| Error::internal_io(error.to_string(), None))?;
            if count == 0 {
                break;
            }
            output.stdout.extend_from_slice(&buffer[..count]);
        }
        Ok(output)
    }

    pub(crate) fn output_to_file(
        &self,
        process: &mut Command,
        file: std::fs::File,
        action: &str,
    ) -> Result<std::process::Output> {
        process
            .stdin(Stdio::null())
            .stdout(file)
            .stderr(Stdio::piped());
        self.wait_output(process, action)
    }

    pub(crate) fn git(&self, path: &Path, args: &[&str]) -> Result<String> {
        let output = self.output_exact(
            Command::new("git").args(args).current_dir(path),
            "read workspace Git evidence",
        )?;
        if !output.status.success() {
            return Err(Error::internal_unexpected(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn wait_output(&self, process: &mut Command, action: &str) -> Result<std::process::Output> {
        self.checkpoint()?;
        let mut owner = ExecutionOwner::spawn(process)
            .map_err(|error| Error::internal_io(error.to_string(), Some(action.to_string())))?;
        let mut stopped = None;
        let output = command::wait_with_bounded_output_until_cancelled_owned(
            &mut owner,
            command::DEFAULT_CAPTURE_LIMIT_BYTES,
            || {
                stopped = self.checkpoint().err();
                stopped.is_some()
            },
        )
        .map_err(|error| {
            let mut error = Error::internal_io(error.to_string(), Some(action.to_string()));
            error.details["workspace_sync"] =
                serde_json::json!({"command_started": true, "observation_failed": true});
            error
        })?;
        if let Some(mut error) = stopped {
            error.details["workspace_sync"]["command_started"] = serde_json::json!(true);
            error.details["workspace_sync"]["action"] = serde_json::json!(action);
            return Err(error);
        }
        Ok(output.into_output())
    }

    pub(crate) fn shell(&self, command: &str, action: &str) -> Result<()> {
        let mut process = Command::new("bash");
        process.args(["-o", "pipefail", "-c", command]);
        let output = self.output(&mut process, action).map_err(|mut error| {
            if error.details["workspace_sync"]["command_started"] == true {
                error.details["workspace_sync"]["remote_effect_uncertain"] =
                    serde_json::json!(true);
            }
            error
        })?;
        if output.status.success() {
            return Ok(());
        }
        super::util::shell_output_failure(output, action).map_err(|mut error| {
            if error.code == homeboy_core::error::ErrorCode::RunnerLabTransportFailure {
                error.details["workspace_sync"] = serde_json::json!({"command_started": true, "remote_effect_uncertain": true, "action": action});
            }
            error
        })
    }
}
