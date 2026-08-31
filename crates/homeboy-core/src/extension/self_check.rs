use homeboy_core::component::Component;
use homeboy_core::engine::run_dir::RunDir;
use homeboy_core::error::{Error, Result};
use homeboy_core::observation::ActiveObservation;
use homeboy_core::stream_capture::StreamCaptureMetadata;
use homeboy_core::validation_progress::{write_command_artifact, ValidationProgressRecorder};
use homeboy_extension_contract::{ExtensionCapability, SelfCheckCaptureMetadata};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

const SELF_CHECK_CAPTURE_LIMIT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct SelfCheckOutput {
    pub exit_code: i32,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub capture: SelfCheckCaptureMetadata,
    pub cargo_target: Option<homeboy_core::CargoTargetEvidence>,
}

/// Optional policy for a product boundary that owns a bounded self-check.
/// Existing capability callers pass no policy and retain their historic
/// unbounded behavior.
pub(crate) struct SelfCheckExecutionPolicy {
    timeout: Duration,
    env: Vec<(String, String)>,
}

impl SelfCheckExecutionPolicy {
    pub(crate) fn bounded(timeout: Duration, env: Vec<(String, String)>) -> Self {
        Self { timeout, env }
    }
}

#[cfg(test)]
pub(crate) fn run_self_checks_with_passthrough(
    component: &Component,
    capability: ExtensionCapability,
    source_path: &Path,
    passthrough: bool,
) -> Result<SelfCheckOutput> {
    run_self_checks_with_passthrough_and_progress(
        component,
        capability,
        source_path,
        passthrough,
        None,
        None,
        None,
    )
}

pub(crate) fn run_self_checks_with_passthrough_and_progress(
    component: &Component,
    capability: ExtensionCapability,
    source_path: &Path,
    passthrough: bool,
    execution_policy: Option<&SelfCheckExecutionPolicy>,
    run_dir: Option<&RunDir>,
    observation: Option<&ActiveObservation>,
) -> Result<SelfCheckOutput> {
    let commands = component.script_commands(capability);
    if commands.is_empty() {
        return Err(Error::validation_invalid_argument(
            "scripts",
            format!(
                "Component '{}' has no {} self-check commands configured",
                component.id,
                capability.label()
            ),
            None,
            None,
        ));
    }

    let working_dir = source_path.to_string_lossy();
    let explicit_cargo_target = component
        .env
        .get("CARGO_TARGET_DIR")
        .cloned()
        .or_else(|| std::env::var("CARGO_TARGET_DIR").ok());
    let cargo_target = component
        .managed_execution
        .shared_cargo_target
        .then(|| {
            homeboy_core::cleanup::acquire_managed_cargo_target(
                &format!("component:{}", component.id),
                source_path,
                explicit_cargo_target.as_deref(),
            )
        })
        .transpose()?;
    let mut stdout = BoundedCapture::new(SELF_CHECK_CAPTURE_LIMIT_BYTES);
    let mut stderr = BoundedCapture::new(SELF_CHECK_CAPTURE_LIMIT_BYTES);
    let mut progress = if let Some(run_dir) = run_dir {
        Some(ValidationProgressRecorder::new(
            run_dir,
            observation,
            commands
                .iter()
                .enumerate()
                .map(|(index, command)| {
                    (
                        format!("{} command {}", capability.label(), index + 1),
                        command.clone(),
                    )
                })
                .collect(),
        )?)
    } else {
        None
    };

    for (index, command) in commands.iter().enumerate() {
        if passthrough {
            homeboy_core::log_status!(
                "self-check",
                "running {} self-check for {}: {}",
                capability.label(),
                component.id,
                command
            );
        }
        if let Some(progress) = progress.as_mut() {
            progress.start(index)?;
        }
        let output = execute_self_check_command(
            command,
            &working_dir,
            passthrough,
            cargo_target.as_ref(),
            execution_policy,
        );
        let stdout_artifact = if let Some(run_dir) = run_dir {
            write_command_artifact(run_dir, index, "stdout", &output.stdout.to_string_lossy())?
        } else {
            None
        };
        let stderr_artifact = if let Some(run_dir) = run_dir {
            write_command_artifact(run_dir, index, "stderr", &output.stderr.to_string_lossy())?
        } else {
            None
        };
        if let Some(progress) = progress.as_mut() {
            progress.finish(index, output.exit_code, stdout_artifact, stderr_artifact)?;
        }
        stdout.push_capture(output.stdout);
        stderr.push_capture(output.stderr);

        if !output.success {
            return Ok(self_check_output(
                output.exit_code,
                false,
                &stdout,
                &stderr,
                cargo_target.as_ref().map(|target| target.evidence()),
            ));
        }
    }

    Ok(self_check_output(
        0,
        true,
        &stdout,
        &stderr,
        cargo_target.as_ref().map(|target| target.evidence()),
    ))
}

fn self_check_output(
    exit_code: i32,
    success: bool,
    stdout: &BoundedCapture,
    stderr: &BoundedCapture,
    cargo_target: Option<homeboy_core::CargoTargetEvidence>,
) -> SelfCheckOutput {
    SelfCheckOutput {
        exit_code,
        success,
        stdout: stdout.to_string_lossy(),
        stderr: stderr.to_string_lossy(),
        capture: SelfCheckCaptureMetadata {
            stdout: stdout.metadata(),
            stderr: stderr.metadata(),
        },
        cargo_target,
    }
}

fn execute_self_check_command(
    command: &str,
    working_dir: &str,
    passthrough: bool,
    cargo_target: Option<&homeboy_core::ManagedCargoTarget>,
    execution_policy: Option<&SelfCheckExecutionPolicy>,
) -> SelfCheckCommandOutput {
    execute_local_self_check_command(
        command,
        working_dir,
        passthrough,
        cargo_target,
        execution_policy,
    )
}

#[derive(Debug, Clone)]
struct SelfCheckCommandOutput {
    stdout: BoundedCapture,
    stderr: BoundedCapture,
    success: bool,
    exit_code: i32,
}

#[derive(Debug, Clone)]
struct BoundedCapture {
    limit: usize,
    seen: usize,
    retained: Vec<u8>,
}

impl BoundedCapture {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            seen: 0,
            retained: Vec::new(),
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        self.seen = self.seen.saturating_add(bytes.len());
        self.retained.extend_from_slice(bytes);
        if self.retained.len() > self.limit {
            let drop_len = self.retained.len() - self.limit;
            self.retained.drain(..drop_len);
        }
    }

    fn push_capture(&mut self, capture: BoundedCapture) {
        self.seen = self.seen.saturating_add(capture.seen);
        self.retained.extend_from_slice(&capture.retained);
        if self.retained.len() > self.limit {
            let drop_len = self.retained.len() - self.limit;
            self.retained.drain(..drop_len);
        }
    }

    fn to_string_lossy(&self) -> String {
        String::from_utf8_lossy(&self.retained).to_string()
    }

    fn metadata(&self) -> StreamCaptureMetadata {
        StreamCaptureMetadata {
            limit_bytes: self.limit,
            seen_bytes: self.seen,
            retained_bytes: self.retained.len(),
            truncated: self.seen > self.retained.len(),
        }
    }
}

fn execute_local_self_check_command(
    command: &str,
    working_dir: &str,
    passthrough: bool,
    cargo_target: Option<&homeboy_core::ManagedCargoTarget>,
    execution_policy: Option<&SelfCheckExecutionPolicy>,
) -> SelfCheckCommandOutput {
    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    };

    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command]);
        cmd
    };

    cmd.current_dir(working_dir);
    if let Some(cargo_target) = cargo_target {
        cmd.env("CARGO_TARGET_DIR", cargo_target.target_dir());
        cmd.env("HOMEBOY_CARGO_TARGET_RESOLUTION", cargo_target.resolution());
    }
    if let Some(policy) = execution_policy {
        cmd.envs(policy.env.iter().map(|(key, value)| (key, value)));
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    if let Some(policy) = execution_policy {
        return execute_bounded_self_check_command(cmd, passthrough, policy);
    }

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            return SelfCheckCommandOutput {
                stdout: BoundedCapture::new(SELF_CHECK_CAPTURE_LIMIT_BYTES),
                stderr: {
                    let mut capture = BoundedCapture::new(SELF_CHECK_CAPTURE_LIMIT_BYTES);
                    capture.push_bytes(format!("Command error: {}", err).as_bytes());
                    capture
                },
                success: false,
                exit_code: -1,
            };
        }
    };

    let stdout_handle = child
        .stdout
        .take()
        .map(|pipe| thread::spawn(move || capture_stream(pipe, passthrough, false)));
    let stderr_handle = child
        .stderr
        .take()
        .map(|pipe| thread::spawn(move || capture_stream(pipe, passthrough, true)));

    let status = child.wait();
    let stdout = stdout_handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_else(|| BoundedCapture::new(SELF_CHECK_CAPTURE_LIMIT_BYTES));
    let stderr = stderr_handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_else(|| BoundedCapture::new(SELF_CHECK_CAPTURE_LIMIT_BYTES));

    match status {
        Ok(status) => SelfCheckCommandOutput {
            stdout,
            stderr,
            success: status.success(),
            exit_code: status.code().unwrap_or(-1),
        },
        Err(err) => {
            let mut stderr = stderr;
            stderr.push_bytes(format!("\nCommand error: {}", err).as_bytes());
            SelfCheckCommandOutput {
                stdout,
                stderr,
                success: false,
                exit_code: -1,
            }
        }
    }
}

fn execute_bounded_self_check_command(
    mut command: Command,
    passthrough: bool,
    policy: &SelfCheckExecutionPolicy,
) -> SelfCheckCommandOutput {
    use homeboy_engine_primitives::command::{ControllerChildGuard, SupervisedCommandTermination};

    let guard = match ControllerChildGuard::prepare(&mut command) {
        Ok(guard) => guard,
        Err(error) => return self_check_spawn_error(error),
    };
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return self_check_spawn_error(error),
    };
    if let Err(error) = guard.attach(&child) {
        let _ = homeboy_engine_primitives::command::terminate_process_tree_and_reap(&mut child);
        return self_check_spawn_error(error);
    }
    let supervised = match homeboy_engine_primitives::command::wait_with_bounded_output_supervised(
        &mut child,
        SELF_CHECK_CAPTURE_LIMIT_BYTES,
        policy.timeout,
        Duration::from_secs(1),
        || false,
        |_, _| Ok(()),
    ) {
        Ok(output) => output,
        Err(error) => return self_check_spawn_error(error),
    };
    let output = supervised.output;
    if passthrough {
        let _ = std::io::stdout().write_all(&output.stdout);
        let _ = std::io::stderr().write_all(&output.stderr);
    }
    let timed_out = supervised.termination == SupervisedCommandTermination::TimedOut;
    SelfCheckCommandOutput {
        stdout: BoundedCapture {
            limit: output.capture.stdout.byte_limit,
            seen: output.capture.stdout.bytes_seen as usize,
            retained: output.stdout,
        },
        stderr: BoundedCapture {
            limit: output.capture.stderr.byte_limit,
            seen: output.capture.stderr.bytes_seen as usize,
            retained: output.stderr,
        },
        success: !timed_out && output.status.success(),
        exit_code: if timed_out {
            124
        } else {
            output.status.code().unwrap_or(-1)
        },
    }
}

fn self_check_spawn_error(error: std::io::Error) -> SelfCheckCommandOutput {
    SelfCheckCommandOutput {
        stdout: BoundedCapture::new(SELF_CHECK_CAPTURE_LIMIT_BYTES),
        stderr: {
            let mut capture = BoundedCapture::new(SELF_CHECK_CAPTURE_LIMIT_BYTES);
            capture.push_bytes(format!("Command error: {error}").as_bytes());
            capture
        },
        success: false,
        exit_code: -1,
    }
}

fn capture_stream<R: Read>(mut src: R, passthrough: bool, stderr: bool) -> BoundedCapture {
    let mut captured = BoundedCapture::new(SELF_CHECK_CAPTURE_LIMIT_BYTES);
    let mut buf = [0u8; 4096];

    loop {
        match src.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if passthrough {
                    if stderr {
                        let mut sink = std::io::stderr();
                        let _ = sink.write_all(&buf[..n]);
                        let _ = sink.flush();
                    } else {
                        let mut sink = std::io::stdout();
                        let _ = sink.write_all(&buf[..n]);
                        let _ = sink.flush();
                    }
                }
                captured.push_bytes(&buf[..n]);
            }
            Err(_) => break,
        }
    }

    captured
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::component::{Component, ComponentScriptsConfig};
    use std::ffi::OsString;

    struct EnvRestore {
        name: &'static str,
        value: Option<OsString>,
    }

    impl EnvRestore {
        fn set(name: &'static str, value: &str) -> Self {
            let restore = Self {
                name,
                value: std::env::var_os(name),
            };
            std::env::set_var(name, value);
            restore
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            match &self.value {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    #[test]
    fn test_run_self_checks_requires_configured_commands() {
        let component = Component::new(
            "fixture".to_string(),
            "/tmp/fixture".to_string(),
            "".to_string(),
            None,
        );

        let err = run_self_checks_with_passthrough(
            &component,
            ExtensionCapability::Test,
            Path::new("/tmp"),
            false,
        )
        .expect_err("missing self-checks should fail");

        assert!(err.to_string().contains("no test self-check commands"));
    }

    #[test]
    fn test_run_self_checks_runs_commands_in_order() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("one.sh"), "printf one >> order.txt\n")
            .expect("first script should be written");
        std::fs::write(dir.path().join("two.sh"), "printf two >> order.txt\n")
            .expect("second script should be written");

        let mut component = Component::new(
            "fixture".to_string(),
            dir.path().to_string_lossy().to_string(),
            "".to_string(),
            None,
        );
        component.scripts = Some(ComponentScriptsConfig {
            lint: vec!["sh one.sh".to_string(), "sh two.sh".to_string()],
            test: Vec::new(),
            build: Vec::new(),
            bench: Vec::new(),
            fuzz: Vec::new(),
            trace: Vec::new(),
            deps: Vec::new(),
        });

        let output = run_self_checks_with_passthrough(
            &component,
            ExtensionCapability::Lint,
            dir.path(),
            false,
        )
        .expect("self-checks should run");

        assert!(output.success);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("order.txt")).unwrap(),
            "onetwo"
        );
    }

    #[test]
    fn test_run_self_checks_with_passthrough() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("lint.sh"),
            "printf self-check-stdout\nprintf self-check-stderr >&2\n",
        )
        .expect("script should be written");

        let mut component = Component::new(
            "fixture".to_string(),
            dir.path().to_string_lossy().to_string(),
            "".to_string(),
            None,
        );
        component.scripts = Some(ComponentScriptsConfig {
            lint: vec!["sh lint.sh".to_string()],
            test: Vec::new(),
            build: Vec::new(),
            bench: Vec::new(),
            fuzz: Vec::new(),
            trace: Vec::new(),
            deps: Vec::new(),
        });

        let output = run_self_checks_with_passthrough(
            &component,
            ExtensionCapability::Lint,
            dir.path(),
            false,
        )
        .expect("self-checks should run without terminal passthrough");

        assert!(output.success);
        assert_eq!(output.stdout, "self-check-stdout");
        assert_eq!(output.stderr, "self-check-stderr");
    }

    #[test]
    fn self_check_child_reports_component_cargo_target_evidence() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("component-target");
        let mut component = Component::new(
            "fixture".to_string(),
            dir.path().to_string_lossy().to_string(),
            "".to_string(),
            None,
        );
        component.managed_execution.shared_cargo_target = true;
        component.env.insert(
            "CARGO_TARGET_DIR".to_string(),
            target.to_string_lossy().to_string(),
        );
        component.scripts = Some(ComponentScriptsConfig {
            lint: vec![
                "printf '%s:%s' \"$HOMEBOY_CARGO_TARGET_RESOLUTION\" \"$CARGO_TARGET_DIR\""
                    .to_string(),
            ],
            ..Default::default()
        });

        let output = run_self_checks_with_passthrough(
            &component,
            ExtensionCapability::Lint,
            dir.path(),
            false,
        )
        .expect("self-check should run");

        assert_eq!(output.stdout, format!("local:{}", target.display()));
        assert_eq!(
            output.cargo_target,
            Some(homeboy_core::CargoTargetEvidence {
                path: target.to_string_lossy().to_string(),
                resolution: "local".to_string(),
                owner: "component:fixture".to_string(),
            })
        );
    }

    #[test]
    fn self_check_child_reports_ambient_cargo_target_evidence() {
        let _env_lock = homeboy_core::test_support::env_lock();
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("ambient-target");
        let _restore = EnvRestore::set("CARGO_TARGET_DIR", &target.to_string_lossy());
        let mut component = Component::new(
            "fixture".to_string(),
            dir.path().to_string_lossy().to_string(),
            "".to_string(),
            None,
        );
        component.managed_execution.shared_cargo_target = true;
        component.scripts = Some(ComponentScriptsConfig {
            lint: vec![
                "printf '%s:%s' \"$HOMEBOY_CARGO_TARGET_RESOLUTION\" \"$CARGO_TARGET_DIR\""
                    .to_string(),
            ],
            ..Default::default()
        });

        let output = run_self_checks_with_passthrough(
            &component,
            ExtensionCapability::Lint,
            dir.path(),
            false,
        )
        .expect("self-check should run");

        assert_eq!(output.stdout, format!("local:{}", target.display()));
        assert_eq!(
            output.cargo_target,
            Some(homeboy_core::CargoTargetEvidence {
                path: target.to_string_lossy().to_string(),
                resolution: "local".to_string(),
                owner: "component:fixture".to_string(),
            })
        );
    }

    #[test]
    fn self_check_holds_shared_cargo_target_lease_for_child_lifetime() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let dir = tempfile::tempdir().expect("temp dir");
            let mut component = Component::new(
                "fixture".to_string(),
                dir.path().to_string_lossy().to_string(),
                "".to_string(),
                None,
            );
            component.managed_execution.shared_cargo_target = true;
            // Override an ambient harness target so this exercises the managed lease.
            component
                .env
                .insert("CARGO_TARGET_DIR".to_string(), String::new());
            component.scripts = Some(ComponentScriptsConfig {
                lint: vec!["test -f \"$CARGO_TARGET_DIR/.homeboy-lease\"".to_string()],
                ..Default::default()
            });

            let output = run_self_checks_with_passthrough(
                &component,
                ExtensionCapability::Lint,
                dir.path(),
                false,
            )
            .expect("self-check should run");
            let evidence = output.cargo_target.expect("managed target evidence");

            assert!(output.success);
            assert_eq!(evidence.resolution, "shared");
            assert!(!Path::new(&evidence.path).join(".homeboy-lease").exists());
        });
    }

    #[test]
    fn test_run_self_checks_bounds_large_output_capture() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("large.sh"),
            "perl -e 'print \"stdout-line\\n\" x 20000'\nperl -e 'print STDERR \"stderr-line\\n\" x 20000'\nexit 7\n",
        )
        .expect("script should be written");

        let mut component = Component::new(
            "fixture".to_string(),
            dir.path().to_string_lossy().to_string(),
            "".to_string(),
            None,
        );
        component.scripts = Some(ComponentScriptsConfig {
            lint: vec!["sh large.sh".to_string()],
            test: Vec::new(),
            build: Vec::new(),
            bench: Vec::new(),
            fuzz: Vec::new(),
            trace: Vec::new(),
            deps: Vec::new(),
        });

        let output = run_self_checks_with_passthrough(
            &component,
            ExtensionCapability::Lint,
            dir.path(),
            false,
        )
        .expect("self-check should return bounded failure output");

        assert!(!output.success);
        assert_eq!(output.exit_code, 7);
        assert!(output.capture.stdout.truncated);
        assert!(output.capture.stderr.truncated);
        assert!(output.capture.stdout.seen_bytes > output.capture.stdout.limit_bytes);
        assert!(output.capture.stderr.seen_bytes > output.capture.stderr.limit_bytes);
        assert!(output.stdout.len() <= output.capture.stdout.limit_bytes);
        assert!(output.stderr.len() <= output.capture.stderr.limit_bytes);
        assert!(output.stdout.contains("stdout-line"));
        assert!(output.stderr.contains("stderr-line"));
    }
}
