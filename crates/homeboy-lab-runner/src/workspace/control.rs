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
pub struct WorkspaceControl {
    control: Option<CooperativeControl>,
    on_checkpoint: Option<Arc<dyn Fn() -> Result<()> + Send + Sync>>,
    progress: Option<Arc<std::sync::Mutex<(String, Instant)>>>,
}

impl WorkspaceControl {
    pub(crate) fn new(control: CooperativeControl) -> Self {
        Self {
            control: Some(control),
            on_checkpoint: None,
            progress: None,
        }
    }

    pub fn before(deadline: Option<Instant>) -> Self {
        Self {
            control: deadline.map(|deadline| CooperativeControl::new(deadline, Arc::new(|| false))),
            on_checkpoint: None,
            progress: None,
        }
    }

    pub(crate) fn with_checkpoint_observer(
        mut self,
        observer: Arc<dyn Fn() -> Result<()> + Send + Sync>,
    ) -> Self {
        self.on_checkpoint = Some(observer);
        self
    }

    pub(crate) fn with_progress(mut self) -> Self {
        self.progress = Some(Arc::new(std::sync::Mutex::new((
            "scanning".into(),
            Instant::now(),
        ))));
        eprintln!("sync: scanning");
        self
    }

    pub(crate) fn stage(&self, name: &str) {
        if self
            .deadline()
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return;
        }
        if let Some(progress) = &self.progress {
            let mut stage = progress.lock().unwrap();
            if stage.0 != name {
                let same_transfer =
                    stage.0.starts_with("transferring") && name.starts_with("transferring");
                let since = if same_transfer {
                    stage.1
                } else {
                    Instant::now()
                };
                *stage = (name.to_string(), since);
                if !same_transfer {
                    eprintln!("sync: {name}");
                }
            }
        }
    }

    pub(crate) fn heartbeat(&self) {
        if let Some(progress) = &self.progress {
            let stage = progress.lock().unwrap();
            eprintln!(
                "sync: {} (active for {:.1}s)",
                stage.0,
                stage.1.elapsed().as_secs_f64()
            );
        }
    }

    pub(crate) fn timeout_error(&self) -> Error {
        let (stage, elapsed) = self
            .progress
            .as_ref()
            .map(|progress| {
                let stage = progress.lock().unwrap();
                (stage.0.clone(), stage.1.elapsed())
            })
            .unwrap_or_else(|| ("workspace snapshot staging".into(), Duration::ZERO));
        let mut error = super::util::workspace_preparation_timeout(&stage, elapsed);
        error.details["workspace_sync"]["stage"] = serde_json::json!(stage);
        error.details["workspace_sync"]["stage_elapsed_ms"] =
            serde_json::json!(elapsed.as_millis());
        if stage.starts_with("transferring") || stage.starts_with("applying") {
            error.details["workspace_sync"]["remote_effect_uncertain"] = serde_json::json!(true);
        }
        error
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.control.as_ref().map(CooperativeControl::deadline)
    }

    pub(crate) fn checkpoint(&self) -> Result<()> {
        if self
            .control
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
            return Err(self.timeout_error());
        }
        if let Some(observer) = &self.on_checkpoint {
            observer()?;
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

    pub(crate) fn output_with_stdin(
        &self,
        process: &mut Command,
        input: std::fs::File,
        action: &str,
    ) -> Result<std::process::Output> {
        process
            .stdin(input)
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
        let file = crate::staging_temp::file().map_err(|error| {
            let error = Error::internal_io(error.to_string(), Some(action.to_string()));
            if error.is_storage_exhausted() {
                error.with_retryable(true)
            } else {
                error
            }
        })?;
        let mut output = self.output_to_file(
            process,
            file.reopen()
                .map_err(|error| Error::internal_io(error.to_string(), None))?,
            action,
        )?;
        if !output.status.success() {
            let storage = Error::internal_io(
                String::from_utf8_lossy(&output.stderr).to_string(),
                Some(file.path().display().to_string()),
            );
            if storage.is_storage_exhausted() {
                return Err(storage.with_retryable(true));
            }
        }
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
            let stderr = String::from_utf8_lossy(&output.stderr);
            let storage = Error::internal_io(stderr.to_string(), Some(path.display().to_string()));
            if storage.is_storage_exhausted() {
                return Err(storage.with_retryable(true));
            }
            return Err(Error::internal_unexpected(
                super::util::git_failure_evidence(path, args, &output),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn wait_output(&self, process: &mut Command, action: &str) -> Result<std::process::Output> {
        self.checkpoint()?;
        let mut owner = ExecutionOwner::spawn(process).map_err(|error| {
            let mut result = Error::internal_io(error.to_string(), Some(action.to_string()));
            result.details["workspace_sync"] = serde_json::json!({
                "command_started": false,
                "program_missing": error.kind() == std::io::ErrorKind::NotFound,
            });
            result
        })?;
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
