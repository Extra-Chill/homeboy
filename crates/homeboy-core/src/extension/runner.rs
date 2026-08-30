use std::io;
use std::path::{Component as PathComponent, Path, PathBuf};
use std::time::Duration;

use crate::extension::{ExtensionCapability, ExtensionPhaseTiming, TestSecretEnvProjection};
use homeboy_core::component::Component;
use homeboy_core::engine::invocation::{InvocationGuard, InvocationRequirements};
use homeboy_core::engine::resource::{self, ExtensionChildResourceSummary};
use homeboy_core::engine::run_dir::{self, RunDir};
use homeboy_core::error::{Error, Result};
use homeboy_core::server::CommandOutput;
use homeboy_engine_primitives::shell;
use serde_json::json;

/// Env var that makes a validation runner fail closed when a resolved
/// validation dependency's local checkout is behind its upstream, instead of
/// warning and proceeding. Set for blocking gates (differential CI lint, and
/// the release preflight lint) so a stale dependency cannot silently determine
/// the outcome (#9643).
pub const STRICT_VALIDATION_DEPENDENCIES_ENV: &str = "HOMEBOY_STRICT_VALIDATION_DEPENDENCIES";
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
    pub cargo_target: Option<homeboy_core::CargoTargetEvidence>,
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
    env_removals: Vec<String>,
    secret_env_names: Vec<String>,
    test_secret_env_projections: Vec<TestSecretEnvProjection>,
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
    pub fn for_context(execution_context: ExtensionExecutionContext) -> Self {
        Self {
            execution_context,
            settings_overrides: Vec::new(),
            settings_json_overrides: Vec::new(),
            env_vars: Vec::new(),
            env_removals: Vec::new(),
            secret_env_names: Vec::new(),
            test_secret_env_projections: Vec::new(),
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

    /// Remove an inherited environment variable from the extension child.
    pub(crate) fn env_remove(mut self, key: &str) -> Self {
        debug_assert!(
            key.bytes()
                .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric()),
            "environment variable names must be shell-safe"
        );
        self.env_removals.push(key.to_string());
        self
    }

    /// Remove an inherited environment variable if condition is true.
    pub(crate) fn env_remove_if(self, condition: bool, key: &str) -> Self {
        if condition {
            self.env_remove(key)
        } else {
            self
        }
    }

    /// Declare secret identities that Homeboy must resolve for this local
    /// child. Values are never stored in the runner or command string.
    pub fn secret_env_names(mut self, names: impl IntoIterator<Item = String>) -> Self {
        self.secret_env_names.extend(names);
        self.secret_env_names.sort();
        self.secret_env_names.dedup();
        self
    }

    pub fn test_secret_env_projections(
        mut self,
        projections: Vec<TestSecretEnvProjection>,
    ) -> Self {
        self.test_secret_env_projections = projections;
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
    ///
    /// The extension's own `structured_sidecars` declarations are applied, so a
    /// declared non-default `path` reaches the script instead of being silently
    /// replaced by the registry default (#11121).
    pub fn with_run_dir(mut self, run_dir: &homeboy_core::engine::run_dir::RunDir) -> Self {
        self.env_vars
            .extend(run_dir.legacy_env_vars_for(&self.declared_structured_sidecars()));
        self.env_vars.push((
            homeboy_core::server::DELEGATED_RUN_STATUS_FILE_ENV.to_string(),
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
    pub fn command_override(mut self, command: String) -> Self {
        self.command_override = Some(command);
        self
    }

    /// Control whether runner output is streamed to the terminal while captured.
    pub fn passthrough(mut self, passthrough: bool) -> Self {
        self.passthrough = passthrough;
        self
    }

    /// Whether this runner streams child output to the terminal. Summary-mode
    /// callers disable passthrough so large child streams are captured to
    /// evidence rather than flooding the terminal (#9845).
    #[cfg(test)]
    pub(crate) fn is_passthrough(&self) -> bool {
        self.passthrough
    }

    /// Stream stderr without streaming stdout. Useful for commands that emit
    /// live human progress while the parent process owns stdout JSON.
    pub(crate) fn stderr_passthrough(mut self, stderr_passthrough: bool) -> Self {
        self.stderr_passthrough = stderr_passthrough;
        self
    }

    pub fn timeout(mut self, timeout: Option<Duration>) -> Self {
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
            self.pre_loaded_component
                .as_ref()
                .or(Some(&self.execution_context.component)),
            self.path_override.as_deref(),
            &self.settings_overrides,
            &self.settings_json_overrides,
            self.command_override.is_some(),
        )?;

        let project_path = PathBuf::from(&prepared.execution.component.local_path);
        let invocation = self.acquire_invocation_guard()?;
        let secret_env_names = crate::extension::test::effective_secret_env_names(
            &self.secret_env_names,
            &self.test_secret_env_projections,
            &prepared.settings_json,
        )?;
        let secret_env = homeboy_core::secret_env::resolve_local_required(
            secret_env_names,
            "test.secret_env",
            "extension child",
        )?;
        let mut extra_env_vars =
            super::component_script::component_env_vars(&prepared.execution.component);
        extra_env_vars.extend(self.env_vars.clone());
        let explicit_cargo_target = extra_env_vars
            .iter()
            .rev()
            .find(|(key, _)| key == "CARGO_TARGET_DIR")
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var("CARGO_TARGET_DIR").ok());
        let _cargo_target = prepared
            .execution
            .component
            .managed_execution
            .shared_cargo_target
            .then(|| {
                homeboy_core::cleanup::acquire_managed_cargo_target(
                    &format!("component:{}", prepared.execution.component.id),
                    &project_path,
                    explicit_cargo_target.as_deref(),
                )
            })
            .transpose()?;
        if let Some(target) = _cargo_target.as_ref() {
            extra_env_vars.push((
                "CARGO_TARGET_DIR".to_string(),
                target.target_dir().to_string_lossy().to_string(),
            ));
            extra_env_vars.push((
                "HOMEBOY_CARGO_TARGET_RESOLUTION".to_string(),
                target.resolution().to_string(),
            ));
        }
        extra_env_vars.extend(secret_env.iter().cloned());
        if !secret_env.is_empty() {
            extra_env_vars.push((
                homeboy_core::server::CHILD_SECRET_ENV_NAMES_ENV.to_string(),
                secret_env
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ));
        }
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

        // Resolved once and reused by both sidecar seams below. Without a run
        // dir there are no sidecar files to seed or check, so the manifest
        // reload is skipped entirely.
        let declared_sidecars = if self.run_dir_path.is_some() {
            self.declared_structured_sidecars()
        } else {
            Vec::new()
        };
        if let Some(run_dir_path) = &self.run_dir_path {
            initialize_structured_sidecars(run_dir_path, &declared_sidecars)?;
        }

        let output = self.execute_script(
            &prepared.execution.extension_path,
            &env_vars,
            !secret_env.is_empty(),
        )?;
        let output = redact_runner_output(output, &secret_env);
        if self.execution_context.capability == ExtensionCapability::Test {
            if let Some(run_dir_path) = &self.run_dir_path {
                normalize_declared_test_failures(run_dir_path, &declared_sidecars)?;
            }
        }
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
        } else if let Some(run_dir_path) = &self.run_dir_path {
            validate_declared_structured_sidecars(run_dir_path, &declared_sidecars)?;
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
                homeboy_core::engine::run_dir::RunDir::from_existing(run_dir_path.clone())?;
            if let Some(artifacts) = invocation.preserve_artifacts(&run_dir)? {
                materialize_reported_test_artifacts(
                    self.execution_context.capability,
                    &artifacts,
                    &run_dir,
                    &output.stdout,
                    &output.stderr,
                )?;
            }
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
            cargo_target: _cargo_target.as_ref().map(|target| target.evidence()),
        })
    }

    /// Structured sidecars the resolved extension declares in its manifest.
    ///
    /// A manifest that will not load yields no declarations rather than an
    /// error: the execution context this runner holds was itself built by
    /// loading that manifest, so a failure here means it became unreadable
    /// mid-run, and losing the seed is a strictly better outcome than failing
    /// the run inside sidecar bookkeeping.
    fn declared_structured_sidecars(&self) -> Vec<crate::extension::StructuredSidecarDeclaration> {
        crate::extension_store::load_extension(&self.execution_context.extension_id)
            .map(|manifest| crate::extension::structured_sidecars(&manifest))
            .unwrap_or_default()
    }

    fn acquire_invocation_guard(&self) -> Result<Option<InvocationGuard>> {
        let Some(path) = &self.run_dir_path else {
            return Ok(None);
        };
        let run_dir = homeboy_core::engine::run_dir::RunDir::from_existing(path.clone())?;
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
                    homeboy_core::extension_store::extension_path(extension_id),
                ))
            })
            .collect()
    }

    fn execute_script(
        &self,
        extension_path: &Path,
        env_vars: &[(String, String)],
        contains_secrets: bool,
    ) -> Result<CommandOutput> {
        let command_override = (!self.env_removals.is_empty()).then(|| {
            format!(
                "unset {}; {}",
                shell::quote_args(&self.env_removals),
                self.command_string(extension_path)
            )
        });
        super::execution::execute_capability_script(
            extension_path,
            &self.execution_context.script_path,
            &self.script_args,
            env_vars,
            self.working_dir.as_deref(),
            command_override
                .as_deref()
                .or(self.command_override.as_deref()),
            super::execution::CapabilityScriptOptions {
                // Streaming an arbitrary child stream cannot guarantee exact
                // value redaction. Secret-bearing children are captured first,
                // then redacted before any Homeboy evidence is produced.
                passthrough: self.passthrough && !contains_secrets,
                stderr_passthrough: self.stderr_passthrough && !contains_secrets,
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

fn materialize_reported_test_artifacts(
    capability: ExtensionCapability,
    artifact_root: &Path,
    run_dir: &RunDir,
    stdout: &str,
    stderr: &str,
) -> Result<()> {
    if capability != ExtensionCapability::Test {
        return Ok(());
    }

    let manifest = homeboy_core::artifact_manifest::read_manifest_from_root(artifact_root)?;
    let locator = regex::Regex::new(r"artifact://files/[A-Za-z0-9._/-]+")
        .expect("artifact locator regex is valid");
    let mut reported = std::collections::BTreeSet::new();
    for value in [stdout, stderr] {
        for candidate in locator.find_iter(value).map(|matched| matched.as_str()) {
            let relative = candidate.trim_start_matches("artifact://files/");
            let path = Path::new(relative);
            if !path.as_os_str().is_empty()
                && !path.is_absolute()
                && path
                    .components()
                    .all(|component| matches!(component, PathComponent::Normal(_)))
            {
                reported.insert(path.to_path_buf());
            }
        }
    }

    for relative in reported.into_iter().take(32) {
        let manifest_suffix = Path::new("files").join(&relative);
        let suffix_matches = manifest
            .artifacts
            .iter()
            .filter(|entry| Path::new(&entry.path).ends_with(&manifest_suffix))
            .collect::<Vec<_>>();
        let relative_id = relative.to_string_lossy();
        let id_matches = suffix_matches
            .iter()
            .filter(|entry| entry.id.as_deref() == Some(relative_id.as_ref()))
            .copied()
            .collect::<Vec<_>>();
        let entry = match (id_matches.as_slice(), suffix_matches.as_slice()) {
            ([entry], _) => *entry,
            ([], [entry]) => *entry,
            ([], []) => continue,
            _ => {
                return Err(Error::validation_invalid_argument(
                    "artifact",
                    "reported test artifact matches multiple registered invocation artifacts",
                    Some(format!("artifact://files/{}", relative.display())),
                    None,
                ));
            }
        };

        let source = artifact_root.join(&entry.path);
        let destination = run_dir.path().join("files").join(&relative);
        if destination.exists() {
            continue;
        }
        let parent = destination
            .parent()
            .expect("artifact destination has parent");
        std::fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!(
                    "create test artifact directory {}",
                    parent.display()
                )),
            )
        })?;
        let canonical_run_dir = std::fs::canonicalize(run_dir.path()).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("resolve test run directory".to_string()),
            )
        })?;
        let canonical_parent = std::fs::canonicalize(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!(
                    "resolve test artifact directory {}",
                    parent.display()
                )),
            )
        })?;
        if !canonical_parent.starts_with(&canonical_run_dir) {
            return Err(Error::validation_invalid_argument(
                "artifact",
                "test artifact destination escapes its run directory",
                Some(destination.display().to_string()),
                None,
            ));
        }

        let temporary = parent.join(format!(".homeboy-artifact-{}.tmp", uuid::Uuid::new_v4()));
        let mut input = std::fs::File::open(&source).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!(
                    "open registered test artifact {}",
                    source.display()
                )),
            )
        })?;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("create test artifact {}", temporary.display())),
                )
            })?;
        if let Err(error) = io::copy(&mut input, &mut output) {
            let _ = std::fs::remove_file(&temporary);
            return Err(Error::internal_io(
                error.to_string(),
                Some(format!(
                    "copy registered test artifact {}",
                    source.display()
                )),
            ));
        }
        std::fs::rename(&temporary, &destination).map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            Error::internal_io(
                error.to_string(),
                Some(format!("publish test artifact {}", destination.display())),
            )
        })?;
    }
    Ok(())
}

fn redact_runner_output(
    mut output: CommandOutput,
    secret_env: &[(String, String)],
) -> CommandOutput {
    output.stdout = redact_resolved_secret_values(&output.stdout, secret_env);
    output.stderr = redact_resolved_secret_values(&output.stderr, secret_env);
    output
}

fn redact_resolved_secret_values(value: &str, secret_env: &[(String, String)]) -> String {
    let mut secrets = secret_env
        .iter()
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    secrets.sort_by_key(|value| std::cmp::Reverse(value.len()));
    secrets.dedup();

    let redacted = secrets
        .into_iter()
        .fold(value.to_string(), |output, secret| {
            output.replace(secret, "[REDACTED]")
        });
    homeboy_core::redaction::redact_string(&redacted)
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

/// Seed the structured sidecars *this extension declares* with their empty
/// shape, so a clean run that legitimately had nothing to report still leaves
/// readable evidence that it measured.
///
/// Before #11123 this hardcoded `capability == Lint` and both lint filenames
/// and never looked at `manifest.structured_sidecars` at all. That is the
/// core half of a three-layer arrangement solving one problem: core seeded
/// unconditionally, every extension seeded again via
/// `homeboy_lint_findings_init`, and core then hard-failed with an
/// `internal.io_error` if the file was still absent. None of the three
/// consulted the declaration, so an extension honestly declaring
/// `"lint.findings": false` was still seeded a file it never asked for and
/// still failed the evidence gate on a *passing* lint.
///
/// Seeding is now driven by the declaration, and which declared keys are safe
/// to seed is a property of `structured_sidecar::REGISTRY` rather than of this
/// function. It is deliberately not "every declared sidecar": for some keys the
/// *absence* of the file is load-bearing (a missing `test.results` is what
/// engages the declared-parser stdout fallback), so only keys flagged
/// `seed_on_start` are written here.
fn initialize_structured_sidecars(
    run_dir_path: &Path,
    declared: &[crate::extension::StructuredSidecarDeclaration],
) -> Result<()> {
    for declaration in declared {
        if !homeboy_core::structured_sidecar::seeds_on_start(&declaration.name) {
            continue;
        }
        let Some(empty) = homeboy_core::structured_sidecar::empty_payload(&declaration.name) else {
            continue;
        };
        let Some(path) = run_dir_relative_sidecar_path(run_dir_path, &declaration.path) else {
            continue;
        };
        write_json_sidecar(&path, &empty)?;
    }
    Ok(())
}

/// Canonicalize the legacy test producer envelope to the registry's array-only
/// failure contract. Counts and runner diagnoses belong to `test.results` and
/// captured output; this sidecar contains only individual failure records.
fn normalize_declared_test_failures(
    run_dir_path: &Path,
    declared: &[crate::extension::StructuredSidecarDeclaration],
) -> Result<()> {
    let Some(declaration) = declared
        .iter()
        .find(|declaration| declaration.name == "test.failures")
    else {
        return Ok(());
    };
    let Some(path) = run_dir_relative_sidecar_path(run_dir_path, &declaration.path) else {
        return Ok(());
    };

    let failures = std::fs::read_to_string(&path)
        .ok()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
        .and_then(|payload| match payload {
            serde_json::Value::Array(items) => Some(items),
            serde_json::Value::Object(mut object) => object
                .remove("failures")
                .and_then(|value| value.as_array().cloned()),
            _ => None,
        })
        .unwrap_or_default();

    write_json_sidecar(&path, &serde_json::Value::Array(failures))
}

/// Check every structured sidecar *this extension declares* against core's
/// registry contract once its runner has written them.
///
/// Before #11121 `validate_payload` was reached only by parsers that had
/// already chosen to read one specific sidecar, so a declared sidecar nobody
/// happened to parse could hold anything at all — a malformed `bench.results`
/// simply disappeared. A declaration is a promise about output, and this is
/// where the promise is finally checked.
///
/// Scope is deliberately narrow, because this can fail a run:
/// - only declared keys, so an extension is judged on what it claimed;
/// - only keys the registry knows, so a vendor key stays the vendor's business;
/// - only payloads that are present and wrong — absence is never a violation
///   (see `structured_sidecar::validate_sidecar_file`);
/// - only after a *successful* run, because a failed one already has core's
///   failure sidecars written over it and a real failure to report.
fn validate_declared_structured_sidecars(
    run_dir_path: &Path,
    declared: &[crate::extension::StructuredSidecarDeclaration],
) -> Result<()> {
    for declaration in declared {
        let Some(path) = run_dir_relative_sidecar_path(run_dir_path, &declaration.path) else {
            continue;
        };
        homeboy_core::structured_sidecar::validate_sidecar_file(&declaration.name, &path)?;
    }
    Ok(())
}

/// Resolve a manifest-declared sidecar path against the run dir, refusing
/// anything that would escape it. A declaration is extension-authored config,
/// and this function writes files, so an absolute path or a `..` component is
/// dropped rather than honoured.
fn run_dir_relative_sidecar_path(run_dir_path: &Path, declared: &str) -> Option<PathBuf> {
    let relative = Path::new(declared);
    if relative.as_os_str().is_empty() || relative.is_absolute() {
        return None;
    }
    if !relative
        .components()
        .all(|component| matches!(component, PathComponent::Normal(_)))
    {
        return None;
    }
    Some(run_dir_path.join(relative))
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
    write_json_sidecar(&path, &json!([]))?;
    write_json_sidecar(
        &run_dir_path.join(run_dir::files::LINT_PRODUCERS),
        &json!([{
            "tool": "homeboy-extension-runner",
            "status": "error",
            "finding_count": 0,
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
        Err(_) => false,
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

pub(crate) fn tail_lines(s: &str, max_lines: usize) -> (String, bool) {
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
    use crate::extension::ExtensionCapability;
    use homeboy_core::component::Component;
    use homeboy_core::engine::run_dir::RunDir;
    use homeboy_core::server::CommandObservation;
    use homeboy_core::test_support::with_isolated_home;

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
            == homeboy_core::server::DELEGATED_RUN_STATUS_FILE_ENV
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
    fn passthrough_defaults_on_and_summary_mode_disables_it() {
        // The runner streams child output to the terminal by default, but
        // summary-mode callers pass `passthrough(false)` so large child streams
        // are captured to evidence instead of flooding the terminal (#9845).
        let default_runner = ExtensionRunner::for_context(context());
        assert!(
            default_runner.is_passthrough(),
            "passthrough is on by default so non-summary runs still stream live output"
        );

        let summary_runner = ExtensionRunner::for_context(context()).passthrough(false);
        assert!(
            !summary_runner.is_passthrough(),
            "summary mode must disable passthrough so the child stream is captured, not streamed"
        );
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
            observation: CommandObservation::Complete,
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
    fn writes_lint_failure_sidecar_as_infrastructure_producer() {
        let run_dir = RunDir::create().expect("run dir");
        let output = CommandOutput {
            stdout: String::new(),
            stderr: "formatter missing".to_string(),
            success: false,
            exit_code: 127,
            timed_out: false,
            observation: CommandObservation::Complete,
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
        assert_eq!(payload, json!([]));
        let producers: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(run_dir.step_file(run_dir::files::LINT_PRODUCERS))
                .expect("producers file"),
        )
        .expect("json");
        assert_eq!(producers[0]["tool"], "homeboy-extension-runner");
        assert_eq!(producers[0]["status"], "error");
        assert_eq!(producers[0]["finding_count"], 0);
        assert_eq!(producers[0]["metadata"]["failure"]["exit_code"], 127);

        run_dir.cleanup();
    }

    fn declaration(name: &str) -> crate::extension::StructuredSidecarDeclaration {
        crate::extension::StructuredSidecarDeclaration {
            name: name.to_string(),
            path: homeboy_core::structured_sidecar::default_path(name)
                .unwrap_or(name)
                .to_string(),
            schema_version: homeboy_core::structured_sidecar::default_schema_version(name)
                .map(str::to_string),
            producer: homeboy_core::structured_sidecar::default_producer(name).map(str::to_string),
        }
    }

    #[test]
    fn initializes_empty_lint_sidecars_for_successful_zero_finding_runs() {
        let run_dir = RunDir::create().expect("run dir");

        initialize_structured_sidecars(
            run_dir.path(),
            &[declaration("lint.findings"), declaration("lint.producers")],
        )
        .expect("initialize lint evidence");

        let findings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(run_dir.step_file(run_dir::files::LINT_FINDINGS))
                .expect("findings file"),
        )
        .expect("findings json");
        let producers: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(run_dir.step_file(run_dir::files::LINT_PRODUCERS))
                .expect("producers file"),
        )
        .expect("producers json");
        assert_eq!(findings, json!([]));
        assert_eq!(producers, json!([]));

        run_dir.cleanup();
    }

    /// The declaration is what drives seeding now. An extension that does not
    /// declare `lint.findings` is not handed one, which is the half of #11123
    /// that stops core from manufacturing a file the manifest disclaims — and,
    /// paired with the gate in `lint::run::workflow`, stops it from then
    /// failing the run for that file's absence.
    #[test]
    fn seeds_nothing_when_the_manifest_declares_no_sidecars() {
        let run_dir = RunDir::create().expect("run dir");

        initialize_structured_sidecars(run_dir.path(), &[]).expect("initialize");

        assert!(!run_dir.step_file(run_dir::files::LINT_FINDINGS).exists());
        assert!(!run_dir.step_file(run_dir::files::LINT_PRODUCERS).exists());

        run_dir.cleanup();
    }

    /// Seeding is registry-driven, not "every declared key". `test.results`
    /// declares itself but must never be pre-seeded: `test::run` treats a
    /// missing results file as the signal to parse counts out of the declared
    /// parser's stdout, so an empty `{}` here would silently suppress a real
    /// result path.
    #[test]
    fn does_not_seed_sidecars_whose_absence_is_load_bearing() {
        let run_dir = RunDir::create().expect("run dir");

        initialize_structured_sidecars(
            run_dir.path(),
            &[
                declaration("test.results"),
                declaration("test.failures"),
                declaration("bench.results"),
                declaration("trace.results"),
            ],
        )
        .expect("initialize");

        assert!(!run_dir.step_file(run_dir::files::TEST_RESULTS).exists());
        assert!(!run_dir.step_file(run_dir::files::TEST_FAILURES).exists());
        assert!(!run_dir.step_file(run_dir::files::BENCH_RESULTS).exists());
        assert!(!run_dir.step_file(run_dir::files::TRACE_RESULTS).exists());

        run_dir.cleanup();
    }

    #[test]
    fn normalizes_test_failure_envelopes_without_losing_runner_diagnosis() {
        let run_dir = RunDir::create().expect("run dir");
        let failures = run_dir.step_file(run_dir::files::TEST_FAILURES);
        let output = CommandOutput {
            stdout: "PHPUNIT_ZERO_TESTS cause=changed_file_filter_mismatch".to_string(),
            stderr: String::new(),
            success: true,
            exit_code: 0,
            timed_out: false,
            observation: CommandObservation::Complete,
            child_resource: None,
        };
        std::fs::write(
            &failures,
            serde_json::to_string(&json!({
                "failures": [],
                "total": 0,
                "passed": 0,
            }))
            .expect("serialize fixture"),
        )
        .expect("write legacy failure envelope");

        normalize_declared_test_failures(run_dir.path(), &[declaration("test.failures")])
            .expect("normalize failures");

        let payload: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&failures).expect("normalized failures"))
                .expect("failure array");
        assert_eq!(payload, json!([]));
        homeboy_core::structured_sidecar::validate_payload("test.failures", &payload)
            .expect("schema-valid failure array");
        assert!(output
            .stdout
            .contains("PHPUNIT_ZERO_TESTS cause=changed_file_filter_mismatch"));

        std::fs::remove_file(&failures).expect("remove sidecar");
        normalize_declared_test_failures(run_dir.path(), &[declaration("test.failures")])
            .expect("normalize absent infrastructure sidecar");
        assert_eq!(
            std::fs::read_to_string(&failures)
                .expect("seeded failure array")
                .trim(),
            "[]"
        );

        run_dir.cleanup();
    }

    /// A key core has no registry contract for gets no invented empty shape.
    #[test]
    fn ignores_declared_sidecars_the_registry_does_not_know() {
        let run_dir = RunDir::create().expect("run dir");

        initialize_structured_sidecars(run_dir.path(), &[declaration("vendor.custom")])
            .expect("initialize");

        assert!(!run_dir.path().join("vendor.custom").exists());

        run_dir.cleanup();
    }

    /// A declared sidecar's payload is checked against the registry contract
    /// after a successful run. Declaring `lint.findings` and then writing an
    /// object where the contract says array is a producer bug that nothing
    /// noticed before #11121.
    #[test]
    fn validates_declared_sidecar_payloads_after_a_successful_run() {
        let run_dir = RunDir::create().expect("run dir");
        let findings = run_dir.step_file(run_dir::files::LINT_FINDINGS);

        std::fs::write(&findings, r#"[{"message":"boom"}]"#).expect("valid findings");
        validate_declared_structured_sidecars(run_dir.path(), &[declaration("lint.findings")])
            .expect("a payload matching the declared contract passes");

        std::fs::write(&findings, r#"{"message":"boom"}"#).expect("invalid findings");
        let err =
            validate_declared_structured_sidecars(run_dir.path(), &[declaration("lint.findings")])
                .expect_err("an object is not the declared array shape");
        assert!(err.to_string().contains("lint.findings"), "{err}");

        run_dir.cleanup();
    }

    /// Validation judges an extension on what it declared, and only where core
    /// has a contract. An undeclared file and a vendor key are both left alone.
    #[test]
    fn validation_ignores_undeclared_and_registry_unknown_sidecars() {
        let run_dir = RunDir::create().expect("run dir");
        std::fs::write(
            run_dir.step_file(run_dir::files::LINT_FINDINGS),
            "{ malformed",
        )
        .expect("malformed findings");
        std::fs::write(run_dir.path().join("vendor.custom"), "{ malformed")
            .expect("malformed vendor sidecar");

        validate_declared_structured_sidecars(run_dir.path(), &[])
            .expect("a manifest declaring nothing is judged on nothing");
        validate_declared_structured_sidecars(run_dir.path(), &[declaration("vendor.custom")])
            .expect("core has no contract for a vendor key");

        run_dir.cleanup();
    }

    /// Absence is not a violation. Several sidecars are legitimately never
    /// written (a missing `test.results` is what engages the stdout fallback),
    /// and two declared paths are directories rather than JSON documents —
    /// failing a run for any of those is the mistake that made the lint
    /// evidence gate fail passing lints.
    #[test]
    fn validation_does_not_fail_on_absent_or_directory_sidecars() {
        let run_dir = RunDir::create().expect("run dir");

        validate_declared_structured_sidecars(
            run_dir.path(),
            &[
                declaration("test.results"),
                declaration("bench.results"),
                declaration("annotations"),
            ],
        )
        .expect("absent and directory sidecars are not contract violations");

        run_dir.cleanup();
    }

    /// A declared path is extension-authored config and this writes files, so
    /// a traversal or absolute path is dropped rather than honoured.
    #[test]
    fn refuses_declared_sidecar_paths_that_escape_the_run_dir() {
        let run_dir = RunDir::create().expect("run dir");

        assert_eq!(
            run_dir_relative_sidecar_path(run_dir.path(), "lint-findings.json"),
            Some(run_dir.step_file(run_dir::files::LINT_FINDINGS))
        );
        assert_eq!(
            run_dir_relative_sidecar_path(run_dir.path(), "nested/findings.json"),
            Some(run_dir.path().join("nested").join("findings.json"))
        );
        for hostile in [
            "",
            "/etc/passwd",
            "../escape.json",
            "nested/../../escape.json",
        ] {
            assert_eq!(
                run_dir_relative_sidecar_path(run_dir.path(), hostile),
                None,
                "{hostile} must not resolve"
            );
        }

        run_dir.cleanup();
    }

    #[test]
    fn replaces_malformed_lint_findings_sidecar_with_infrastructure_producer() {
        let run_dir = RunDir::create().expect("run dir");
        std::fs::write(
            run_dir.step_file(run_dir::files::LINT_FINDINGS),
            "{ malformed",
        )
        .expect("malformed findings file");
        let output = CommandOutput {
            stdout: String::new(),
            stderr: "runner failed".to_string(),
            success: false,
            exit_code: 1,
            timed_out: false,
            observation: CommandObservation::Complete,
            child_resource: None,
        };

        write_structured_failure_sidecar(
            run_dir.path(),
            ExtensionCapability::Lint,
            "./lint.sh",
            &output,
        )
        .expect("write fallback");

        assert_eq!(
            std::fs::read_to_string(run_dir.step_file(run_dir::files::LINT_FINDINGS))
                .expect("findings file")
                .trim(),
            "[]"
        );
        assert!(run_dir.step_file(run_dir::files::LINT_PRODUCERS).is_file());

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

    #[test]
    fn strict_validation_dependencies_flag_reflects_env() {
        // A runner without the strict env proceeds (warn-and-continue); setting
        // HOMEBOY_STRICT_VALIDATION_DEPENDENCIES=1 makes it fail closed on a
        // behind-upstream dependency — this is what the release lint gate sets
        // so a stale checkout cannot silently determine the outcome (#9643).
        let lenient = ExtensionRunner::for_context(context());
        assert!(!lenient.strict_validation_dependencies());

        let strict =
            ExtensionRunner::for_context(context()).env(STRICT_VALIDATION_DEPENDENCIES_ENV, "1");
        assert!(strict.strict_validation_dependencies());

        // Only an explicit truthy value enables strict mode.
        let disabled =
            ExtensionRunner::for_context(context()).env(STRICT_VALIDATION_DEPENDENCIES_ENV, "0");
        assert!(!disabled.strict_validation_dependencies());
    }
}
