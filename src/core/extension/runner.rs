use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::component::Component;
use crate::core::engine::invocation::{InvocationGuard, InvocationRequirements};
use crate::core::engine::resource::{self, ExtensionChildResourceSummary};
use crate::core::engine::run_dir::{self, RunDir};
use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::extension::{ExtensionCapability, ExtensionPhaseTiming};
use crate::core::server::CommandOutput;
use serde_json::json;

const STRICT_VALIDATION_DEPENDENCIES_ENV: &str = "HOMEBOY_STRICT_VALIDATION_DEPENDENCIES";
const STALE_VALIDATION_DEPENDENCY_PREFIX: &str = "Resolved validation dependency";
const FAILURE_TAIL_LINES: usize = 80;

/// Output from a extension runner script execution.
pub struct RunnerOutput {
    pub exit_code: i32,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub child_resource: Option<ExtensionChildResourceSummary>,
    pub extension_phase_timings: Vec<ExtensionPhaseTiming>,
}

use super::ExtensionExecutionContext;

/// Orchestrates extension script execution for lint/test/build runners.
///
/// Encapsulates the shared logic for finding components, resolving extensions,
/// loading manifests, merging settings, and executing runner scripts.
pub struct ExtensionRunner {
    execution_context: ExtensionExecutionContext,
    settings_overrides: Vec<(String, String)>,
    /// Typed-JSON setting overrides from `--setting-json key=<json>`.
    /// Applied AFTER `settings_overrides` (so `--setting-json` wins on
    /// conflict — strictly more expressive). See SettingArgs docstring.
    settings_json_overrides: Vec<(String, serde_json::Value)>,
    env_vars: Vec<(String, String)>,
    env_provider_extensions: Vec<String>,
    script_args: Vec<String>,
    path_override: Option<String>,
    pre_loaded_component: Option<Component>,
    /// Override the working directory for script execution.
    /// When set, the script runs in this directory instead of deriving it from the extension path.
    /// Used by Build to run in the component's `local_path`.
    working_dir: Option<String>,
    /// Override the command string instead of constructing from extension_path + script_path.
    /// Used by Build when `command_template` produces a pre-resolved command.
    command_override: Option<String>,
    /// Tee runner stdout/stderr to the terminal while capturing it.
    passthrough: bool,
    /// Tee only runner stderr to the terminal while capturing stdout/stderr.
    stderr_passthrough: bool,
    /// Optional wall-clock budget enforced by the parent process.
    timeout: Option<Duration>,
    /// Run directory path for recording machine-local child process evidence.
    run_dir_path: Option<PathBuf>,
    invocation_requirements: InvocationRequirements,
}

impl ExtensionRunner {
    /// Use a pre-loaded component instead of loading by ID.
    ///
    /// This avoids re-loading from config when the caller already has a
    /// resolved component (e.g., from portable config discovery in CI).
    pub fn component(mut self, comp: Component) -> Self {
        self.pre_loaded_component = Some(comp);
        self
    }

    /// Create a runner from a pre-resolved execution context.
    pub(crate) fn for_context(execution_context: ExtensionExecutionContext) -> Self {
        Self {
            execution_context,
            settings_overrides: Vec::new(),
            settings_json_overrides: Vec::new(),
            env_vars: Vec::new(),
            env_provider_extensions: Vec::new(),
            script_args: Vec::new(),
            path_override: None,
            pre_loaded_component: None,
            working_dir: None,
            command_override: None,
            passthrough: true,
            stderr_passthrough: false,
            timeout: None,
            run_dir_path: None,
            invocation_requirements: InvocationRequirements::default(),
        }
    }

    /// Override the component's `local_path` for this execution.
    ///
    /// Use this when running against a workspace clone or temporary checkout
    /// instead of the configured component path.
    pub fn path_override(mut self, path: Option<String>) -> Self {
        self.path_override = path;
        self
    }

    /// Add settings overrides from key=value pairs.
    pub fn settings(mut self, overrides: &[(String, String)]) -> Self {
        self.settings_overrides.extend(overrides.iter().cloned());
        self
    }

    /// Add typed-JSON settings overrides from `--setting-json key=<json>`.
    /// Preserves object/array/typed-scalar values; applied after string
    /// overrides so JSON wins on conflict.
    pub fn settings_json(mut self, overrides: &[(String, serde_json::Value)]) -> Self {
        self.settings_json_overrides
            .extend(overrides.iter().cloned());
        self
    }

    /// Add an environment variable.
    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env_vars.push((key.to_string(), value.to_string()));
        self
    }

    pub fn env_provider_extensions(mut self, extension_ids: &[String]) -> Self {
        self.env_provider_extensions
            .extend(extension_ids.iter().filter(|id| !id.is_empty()).cloned());
        self.env_provider_extensions.sort();
        self.env_provider_extensions.dedup();
        self
    }

    /// Add an environment variable if condition is true.
    pub fn env_if(mut self, condition: bool, key: &str, value: &str) -> Self {
        if condition {
            self.env_vars.push((key.to_string(), value.to_string()));
        }
        self
    }

    /// Add an environment variable if the Option is Some.
    pub(crate) fn env_opt(mut self, key: &str, value: &Option<String>) -> Self {
        if let Some(v) = value {
            self.env_vars.push((key.to_string(), v.clone()));
        }
        self
    }

    /// Set the run directory, injecting HOMEBOY_RUN_DIR and all legacy
    /// per-file env vars so extension scripts work with either pattern.
    pub fn with_run_dir(mut self, run_dir: &crate::core::engine::run_dir::RunDir) -> Self {
        self.env_vars.extend(run_dir.legacy_env_vars());
        self.env_vars.push((
            crate::core::server::DELEGATED_RUN_STATUS_FILE_ENV.to_string(),
            run_dir
                .step_file("delegated-run-status.json")
                .to_string_lossy()
                .to_string(),
        ));
        self.run_dir_path = Some(run_dir.path().to_path_buf());
        self
    }

    /// Require invocation-scoped resources for the child workload.
    pub fn invocation_requirements(mut self, requirements: InvocationRequirements) -> Self {
        self.invocation_requirements = requirements;
        self
    }

    /// Add arguments to pass to the script.
    pub fn script_args(mut self, args: &[String]) -> Self {
        self.script_args.extend(args.iter().cloned());
        self
    }

    /// Set the working directory for script execution.
    ///
    /// By default, scripts run relative to the extension path. Use this to
    /// run in a different directory (e.g., the component's `local_path` for builds).
    pub(crate) fn working_dir(mut self, dir: &str) -> Self {
        self.working_dir = Some(dir.to_string());
        self
    }

    /// Override the command string instead of constructing from extension_path + script_path.
    ///
    /// Use this when the command is pre-resolved (e.g., Build's `command_template`
    /// has already been interpolated with the script path).
    pub(crate) fn command_override(mut self, command: String) -> Self {
        self.command_override = Some(command);
        self
    }

    /// Control whether runner output is streamed to the terminal while captured.
    pub(crate) fn passthrough(mut self, passthrough: bool) -> Self {
        self.passthrough = passthrough;
        self
    }

    /// Stream stderr without streaming stdout. Useful for commands that emit
    /// live human progress while the parent process owns stdout JSON.
    pub(crate) fn stderr_passthrough(mut self, stderr_passthrough: bool) -> Self {
        self.stderr_passthrough = stderr_passthrough;
        self
    }

    pub(crate) fn timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Execute the extension runner script.
    ///
    /// Performs the full orchestration:
    /// 1. Load component configuration
    /// 2. Determine extension from component config
    /// 3. Find extension path
    /// 4. Validate script exists (unless command_override is set)
    /// 5. Load manifest
    /// 6. Merge settings (manifest defaults → component → overrides)
    /// 7. Prepare environment variables
    /// 8. Execute via shell
    pub fn run(&self) -> Result<RunnerOutput> {
        let prepared = super::execution::prepare_capability_run(
            &self.execution_context,
            self.pre_loaded_component.as_ref(),
            self.path_override.as_deref(),
            &self.settings_overrides,
            &self.settings_json_overrides,
            self.command_override.is_some(),
        )?;

        let project_path = PathBuf::from(&prepared.execution.component.local_path);
        let invocation = self.acquire_invocation_guard()?;
        let mut extra_env_vars = self.env_vars.clone();
        if let Some(invocation) = invocation.as_ref() {
            extra_env_vars.extend(invocation.env_vars());
        }
        let env_vars = self.prepare_env_vars(
            &prepared.execution.extension_path,
            &project_path,
            &prepared.settings_json,
            &prepared.execution.extension_id,
            &extra_env_vars,
        )?;

        let output = self.execute_script(&prepared.execution.extension_path, &env_vars)?;
        if !output.success {
            if let Some(run_dir_path) = &self.run_dir_path {
                let command = self.command_string(&prepared.execution.extension_path);
                write_structured_failure_sidecar(
                    run_dir_path,
                    self.execution_context.capability,
                    &command,
                    &output,
                )?;
            }
        }
        if self.strict_validation_dependencies() {
            if let Some(message) =
                stale_validation_dependency_message(&output.stdout, &output.stderr)
            {
                return Err(Error::validation_invalid_argument(
                    "validation_dependencies",
                    format!("stale validation dependency blocks CI parity: {}", message),
                    None,
                    None,
                ));
            }
        }

        if let (Some(run_dir_path), Some(child_resource)) =
            (&self.run_dir_path, output.child_resource.as_ref())
        {
            let _ = resource::record_extension_child_resource(run_dir_path, child_resource);
        }

        if let (Some(run_dir_path), Some(invocation)) = (&self.run_dir_path, invocation.as_ref()) {
            let run_dir =
                crate::core::engine::run_dir::RunDir::from_existing(run_dir_path.clone())?;
            invocation.preserve_artifacts(&run_dir)?;
        }

        Ok(RunnerOutput {
            exit_code: output.exit_code,
            success: output.success,
            stdout: output.stdout,
            stderr: output.stderr,
            timed_out: output.timed_out,
            child_resource: output.child_resource,
            extension_phase_timings: self
                .run_dir_path
                .as_deref()
                .map(read_extension_phase_timings)
                .transpose()?
                .unwrap_or_default(),
        })
    }

    fn acquire_invocation_guard(&self) -> Result<Option<InvocationGuard>> {
        let Some(path) = &self.run_dir_path else {
            return Ok(None);
        };
        let run_dir = crate::core::engine::run_dir::RunDir::from_existing(path.clone())?;
        InvocationGuard::acquire(&run_dir, &self.invocation_requirements).map(Some)
    }

    fn strict_validation_dependencies(&self) -> bool {
        self.env_vars.iter().any(|(key, value)| {
            key == STRICT_VALIDATION_DEPENDENCIES_ENV && matches!(value.as_str(), "1" | "true")
        })
    }

    fn prepare_env_vars(
        &self,
        extension_path: &Path,
        project_path: &Path,
        settings_json: &str,
        extension_name: &str,
        extra_env_vars: &[(String, String)],
    ) -> Result<Vec<(String, String)>> {
        let additional_env_provider_paths = self.additional_env_provider_paths()?;
        super::execution::build_capability_env_with_additional_providers(
            extension_name,
            &self.execution_context.component.id,
            extension_path,
            project_path,
            settings_json,
            &additional_env_provider_paths,
            extra_env_vars,
        )
    }

    fn additional_env_provider_paths(&self) -> Result<Vec<(String, PathBuf)>> {
        self.env_provider_extensions
            .iter()
            .filter(|extension_id| extension_id.as_str() != self.execution_context.extension_id)
            .map(|extension_id| {
                Ok((
                    extension_id.clone(),
                    super::registry::extension_path(extension_id),
                ))
            })
            .collect()
    }

    fn execute_script(
        &self,
        extension_path: &Path,
        env_vars: &[(String, String)],
    ) -> Result<CommandOutput> {
        super::execution::execute_capability_script(
            extension_path,
            &self.execution_context.script_path,
            &self.script_args,
            env_vars,
            self.working_dir.as_deref(),
            self.command_override.as_deref(),
            super::execution::CapabilityScriptOptions {
                passthrough: self.passthrough,
                stderr_passthrough: self.stderr_passthrough,
                timeout: self.timeout,
            },
        )
    }

    fn command_string(&self, extension_path: &Path) -> String {
        if let Some(command) = &self.command_override {
            return command.clone();
        }

        let resolved = extension_path.join(&self.execution_context.script_path);
        let mut command = shell::quote_path(&resolved.to_string_lossy());
        if !self.script_args.is_empty() {
            command.push(' ');
            command.push_str(&shell::quote_args(&self.script_args));
        }
        command
    }
}

fn write_structured_failure_sidecar(
    run_dir_path: &Path,
    capability: ExtensionCapability,
    command: &str,
    output: &CommandOutput,
) -> Result<()> {
    match capability {
        ExtensionCapability::Lint => write_lint_failure_sidecar(run_dir_path, command, output),
        ExtensionCapability::Test => write_test_failure_sidecar(run_dir_path, command, output),
        _ => Ok(()),
    }
}

fn write_lint_failure_sidecar(
    run_dir_path: &Path,
    command: &str,
    output: &CommandOutput,
) -> Result<()> {
    let path = run_dir_path.join(run_dir::files::LINT_FINDINGS);
    if sidecar_has_payload(&path) {
        return Ok(());
    }

    let failure = failure_payload("lint", command, output);
    write_json_sidecar(
        &path,
        &json!([{
            "tool": "homeboy-extension-runner",
            "category": "infrastructure",
            "severity": "error",
            "message": format!("lint runner failed before producing lint findings (exit {})", output.exit_code),
            "fingerprint": format!("homeboy-extension-runner:lint:{}", output.exit_code),
            "metadata": {
                "phase": "lint",
                "failure": failure,
            }
        }]),
    )
}

fn write_test_failure_sidecar(
    run_dir_path: &Path,
    command: &str,
    output: &CommandOutput,
) -> Result<()> {
    let path = run_dir_path.join(run_dir::files::TEST_RESULTS);
    if sidecar_has_payload(&path) {
        return Ok(());
    }

    write_json_sidecar(
        &path,
        &json!({
            "status": "failed",
            "phase": "test",
            "command": command,
            "exit_code": output.exit_code,
            "stdout_tail": tail_lines(&output.stdout, FAILURE_TAIL_LINES).0,
            "stderr_tail": tail_lines(&output.stderr, FAILURE_TAIL_LINES).0,
            "failure": failure_payload("test", command, output),
        }),
    )
}

fn sidecar_has_payload(path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(serde_json::Value::Array(items)) => !items.is_empty(),
        Ok(serde_json::Value::Object(fields)) => !fields.is_empty(),
        Ok(_) => true,
        Err(_) => true,
    }
}

fn write_json_sidecar(path: &Path, value: &serde_json::Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!(
                    "create structured failure sidecar directory {}",
                    parent.display()
                )),
            )
        })?;
    }

    let payload = serde_json::to_string_pretty(value).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("serialize structured failure sidecar".to_string()),
        )
    })?;
    std::fs::write(path, format!("{}\n", payload)).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!(
                "write structured failure sidecar {}",
                path.display()
            )),
        )
    })
}

fn failure_payload(phase: &str, command: &str, output: &CommandOutput) -> serde_json::Value {
    let (stdout_tail, stdout_truncated) = tail_lines(&output.stdout, FAILURE_TAIL_LINES);
    let (stderr_tail, stderr_truncated) = tail_lines(&output.stderr, FAILURE_TAIL_LINES);
    let mut payload = json!({
        "phase": phase,
        "command": command,
        "exit_code": output.exit_code,
        "stdout_tail": stdout_tail,
        "stderr_tail": stderr_tail,
        "stdout_truncated": stdout_truncated,
        "stderr_truncated": stderr_truncated,
    });

    if let Some(detail) = parsed_detail(&output.stdout).or_else(|| parsed_detail(&output.stderr)) {
        payload["parsed_detail"] = detail;
    }

    payload
}

fn parsed_detail(output: &str) -> Option<serde_json::Value> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok().or_else(|| {
        trimmed
            .lines()
            .rev()
            .map(str::trim)
            .find_map(|line| serde_json::from_str(line).ok())
    })
}

pub(in crate::core::extension) fn tail_lines(s: &str, max_lines: usize) -> (String, bool) {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= max_lines {
        (s.to_string(), false)
    } else {
        let start = lines.len() - max_lines;
        (lines[start..].join("\n"), true)
    }
}

pub(crate) fn read_extension_phase_timings(
    run_dir_path: &Path,
) -> Result<Vec<ExtensionPhaseTiming>> {
    let run_dir = RunDir::from_existing(run_dir_path.to_path_buf())?;
    let Some(value) = run_dir.read_step_output(run_dir::files::PHASE_TIMINGS) else {
        return Ok(Vec::new());
    };

    if let Some(timings) = value.get("phase_timings") {
        return serde_json::from_value(timings.clone()).map_err(|e| {
            Error::internal_json(
                e.to_string(),
                Some("parse extension phase timings".to_string()),
            )
        });
    }

    serde_json::from_value(value).map_err(|e| {
        Error::internal_json(
            e.to_string(),
            Some("parse extension phase timings".to_string()),
        )
    })
}

fn stale_validation_dependency_message(stdout: &str, stderr: &str) -> Option<String> {
    stderr
        .lines()
        .chain(stdout.lines())
        .map(str::trim)
        .find(|line| {
            line.contains(STALE_VALIDATION_DEPENDENCY_PREFIX)
                && line.contains(" is behind ")
                && line.contains("commit(s)")
        })
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::Component;
    use crate::core::engine::run_dir::RunDir;
    use crate::core::extension::ExtensionCapability;
    use crate::test_support::with_isolated_home;

    fn context() -> ExtensionExecutionContext {
        ExtensionExecutionContext {
            component: Component::new(
                "fixture".to_string(),
                "/tmp/fixture".to_string(),
                "fixture-extension".to_string(),
                None,
            ),
            capability: ExtensionCapability::Lint,
            extension_id: "fixture-extension".to_string(),
            extension_path: PathBuf::from("/tmp/fixture-extension"),
            script_path: "lint.sh".to_string(),
            settings: Vec::new(),
            accepted_setting_keys: Vec::new(),
        }
    }

    #[test]
    fn with_run_dir_tracks_resource_artifact_path() {
        let run_dir = RunDir::create().expect("run dir");
        let runner = ExtensionRunner::for_context(context()).with_run_dir(&run_dir);

        assert_eq!(runner.run_dir_path.as_deref(), Some(run_dir.path()));
        assert!(runner
            .env_vars
            .iter()
            .any(|(key, value)| key == "HOMEBOY_RUN_DIR"
                && value == &run_dir.path().to_string_lossy()));
        assert!(runner.env_vars.iter().any(|(key, value)| key
            == crate::core::server::DELEGATED_RUN_STATUS_FILE_ENV
            && value
                == &run_dir
                    .step_file("delegated-run-status.json")
                    .to_string_lossy()));

        run_dir.cleanup();
    }

    #[test]
    fn runner_without_run_dir_does_not_create_invocation_context() {
        with_isolated_home(|_| {
            let runner = ExtensionRunner::for_context(context());

            assert!(runner
                .acquire_invocation_guard()
                .expect("invocation guard")
                .is_none());
            assert!(!runner
                .env_vars
                .iter()
                .any(|(key, _)| key.starts_with("HOMEBOY_INVOCATION_")));
        });
    }

    #[test]
    fn reads_extension_phase_timings_from_run_dir() {
        with_isolated_home(|_| {
            let run_dir = RunDir::create().expect("run dir");
            std::fs::write(
                run_dir.step_file(run_dir::files::PHASE_TIMINGS),
                serde_json::json!({
                    "phase_timings": [
                        {
                            "name": "opaque-provider-phase",
                            "duration_ms": 1234,
                            "status": "waiting",
                            "message": "provider is waiting for a shared resource",
                            "artifacts": [{ "kind": "opaque", "path": "artifacts/timing.json" }],
                            "metadata": { "extension": "fixture" }
                        }
                    ]
                })
                .to_string(),
            )
            .expect("write phase timings");

            let timings =
                read_extension_phase_timings(&run_dir.path().to_path_buf()).expect("phase timings");

            assert_eq!(timings.len(), 1);
            assert_eq!(timings[0].name, "opaque-provider-phase");
            assert_eq!(timings[0].duration_ms, 1234);
            assert_eq!(timings[0].status.as_deref(), Some("waiting"));
            assert_eq!(
                timings[0].message.as_deref(),
                Some("provider is waiting for a shared resource")
            );
            assert_eq!(timings[0].artifacts[0]["path"], "artifacts/timing.json");
            assert_eq!(timings[0].metadata["extension"], "fixture");

            run_dir.cleanup();
        });
    }

    #[test]
    fn test_stderr_passthrough() {
        let runner = ExtensionRunner::for_context(context()).stderr_passthrough(true);

        assert!(runner.stderr_passthrough);
    }

    #[test]
    fn writes_test_failure_sidecar_when_runner_fails_before_counts() {
        let run_dir = RunDir::create().expect("run dir");
        let output = CommandOutput {
            stdout: "booting\n{\"detail\":\"missing db\"}".to_string(),
            stderr: "fatal setup error".to_string(),
            success: false,
            exit_code: 2,
            timed_out: false,
            child_resource: None,
        };

        write_structured_failure_sidecar(
            run_dir.path(),
            ExtensionCapability::Test,
            "./test.sh",
            &output,
        )
        .expect("write fallback");

        let payload: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(run_dir.step_file(run_dir::files::TEST_RESULTS))
                .expect("results file"),
        )
        .expect("json");
        assert_eq!(payload["phase"], "test");
        assert_eq!(payload["command"], "./test.sh");
        assert_eq!(payload["exit_code"], 2);
        assert_eq!(payload["stderr_tail"], "fatal setup error");
        assert_eq!(payload["failure"]["parsed_detail"]["detail"], "missing db");

        run_dir.cleanup();
    }

    #[test]
    fn writes_lint_failure_sidecar_as_finding() {
        let run_dir = RunDir::create().expect("run dir");
        let output = CommandOutput {
            stdout: String::new(),
            stderr: "formatter missing".to_string(),
            success: false,
            exit_code: 127,
            timed_out: false,
            child_resource: None,
        };

        write_structured_failure_sidecar(
            run_dir.path(),
            ExtensionCapability::Lint,
            "./lint.sh",
            &output,
        )
        .expect("write fallback");

        let payload: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(run_dir.step_file(run_dir::files::LINT_FINDINGS))
                .expect("findings file"),
        )
        .expect("json");
        assert_eq!(payload[0]["tool"], "homeboy-extension-runner");
        assert_eq!(payload[0]["metadata"]["phase"], "lint");
        assert_eq!(payload[0]["metadata"]["failure"]["exit_code"], 127);

        run_dir.cleanup();
    }

    #[test]
    fn detects_stale_validation_dependency_warning() {
        let stderr = "Resolved validation dependency 'sample-plugin' to local checkout '/tmp/sample-plugin', but it is behind origin/main by 3 commit(s). Update the checkout or pass an explicit dependency path.";

        let message = stale_validation_dependency_message("", stderr).expect("stale dependency");

        assert!(message.contains("sample-plugin"));
        assert!(message.contains("behind origin/main by 3 commit(s)"));
    }

    #[test]
    fn ignores_non_stale_validation_dependency_output() {
        let stderr =
            "Resolved validation dependency 'sample-plugin' to local checkout '/tmp/sample-plugin'.";

        assert!(stale_validation_dependency_message("", stderr).is_none());
    }
}
