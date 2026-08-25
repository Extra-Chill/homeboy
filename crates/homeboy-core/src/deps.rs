use crate::component::{self, Component};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod dependency_graph;
#[path = "deps_provider.rs"]
pub(crate) mod provider;

pub use dependency_graph::{
    stack_apply, stack_apply_plan, stack_plan, stack_plan_from_components, stack_status,
    DependencyStackApplyResult, DependencyStackApplyStep, DependencyStackCommandResult,
    DependencyStackEdgeStatus, DependencyStackPlan, DependencyStackPlanStep, DependencyStackStatus,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DependencyPackage {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_section: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constraint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked_reference: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyStatus {
    pub component_id: String,
    pub component_path: String,
    pub package_manager: String,
    pub packages: Vec<DependencyPackage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DependencyUpdateResult {
    pub component_id: String,
    pub component_path: String,
    pub package_manager: String,
    pub package: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_constraint: Option<String>,
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<DependencyPackage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<DependencyPackage>,
    pub stdout: String,
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install: Option<DependencyCommandResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rebuild: Option<DependencyCommandResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DependencyCommandResult {
    pub command: Vec<String>,
    pub skipped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyInstallResult {
    pub component_id: String,
    pub component_path: String,
    pub package_manager: String,
    /// One entry per dependency provider that ran an install. Providers that
    /// report nothing to install (e.g. no manifest detected) are omitted.
    pub installs: Vec<DependencyCommandResult>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DependencyHydrationStatus {
    Reused,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DependencyHydrationTermination {
    NotStarted,
    Completed,
    ExitFailure,
    TimedOut,
    NoProgress,
    Cancelled,
    SpawnFailed,
    OutputValidationFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DependencyHydrationOutcome {
    pub schema: String,
    pub workspace: String,
    pub package_root: String,
    pub provider_id: String,
    pub command: Vec<String>,
    pub reason: String,
    pub duration_ms: u128,
    pub termination: DependencyHydrationTermination,
    pub status: DependencyHydrationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct DependencyHydrationProgress {
    pub provider_id: String,
    pub phase: String,
    pub elapsed_ms: u128,
    pub last_progress_ms_ago: Option<u128>,
}

pub struct DependencyHydrationPolicy {
    pub timeout: Duration,
    pub no_progress_timeout: Duration,
    pub heartbeat_interval: Duration,
    pub is_cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    pub on_progress: Arc<dyn Fn(&DependencyHydrationProgress) + Send + Sync>,
}

impl Default for DependencyHydrationPolicy {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30 * 60),
            no_progress_timeout: Duration::from_secs(5 * 60),
            heartbeat_interval: Duration::from_secs(5),
            is_cancelled: Arc::new(|| false),
            on_progress: Arc::new(|_| {}),
        }
    }
}

const DEPENDENCY_HYDRATION_SCHEMA: &str = "homeboy/dependency-hydration-outcome/v1";
const DEPENDENCY_HYDRATION_OUTPUT_LIMIT_BYTES: usize = 64 * 1024;

/// Hydrate dependencies through provider-declared reusable-state, install, and
/// output contracts. Package manifests, command argv, and freshness semantics
/// remain provider-owned; this function only supervises and records outcomes.
pub fn hydrate_declared_dependencies(
    path: &Path,
    workspace: &str,
    package_root: &str,
    policy: &DependencyHydrationPolicy,
) -> Result<Vec<DependencyHydrationOutcome>> {
    let path_arg = path.display().to_string();
    let Ok(mut component) = component::resolve_effective(None, Some(&path_arg), None) else {
        return Ok(Vec::new());
    };
    component.local_path = path_arg;
    let providers = provider::resolve_dependency_providers_optional(&component, path)?;
    let mut outcomes = Vec::new();

    for provider in providers {
        let Some(plan) = provider.hydration_plan(&component, path)? else {
            continue;
        };
        let provider_id = crate::redaction::redact_string(&plan.provider_id);
        let install_command = crate::redaction::redact_argv(&plan.install.argv());
        let started = Instant::now();
        (policy.on_progress)(&DependencyHydrationProgress {
            provider_id: provider_id.clone(),
            phase: "assessing_reusable_state".to_string(),
            elapsed_ms: 0,
            last_progress_ms_ago: None,
        });

        let mut stale_reason = "provider_did_not_declare_reusable_state".to_string();
        if let Some(reusable) = &plan.reusable {
            let assessment = run_hydration_command(
                &reusable.command,
                &provider_id,
                "assessing_reusable_state",
                policy,
            );
            let reusable_command = crate::redaction::redact_argv(&reusable.command.argv());
            let reusable_reason = crate::redaction::redact_string(&reusable.reusable_reason);
            stale_reason = crate::redaction::redact_string(&reusable.stale_reason);
            let execution = match assessment {
                Ok(execution) => execution,
                Err(termination) => {
                    outcomes.push(hydration_outcome(
                        workspace,
                        package_root,
                        provider_id,
                        reusable_command,
                        "reusable_state_assessment_failed".to_string(),
                        started.elapsed(),
                        termination,
                        DependencyHydrationStatus::Failed,
                        None,
                    ));
                    break;
                }
            };
            if reusable
                .reusable_exit_codes
                .contains(&execution.exit_code.unwrap_or(-1))
                && declared_outputs_ready(path, &plan.outputs)
            {
                outcomes.push(hydration_outcome(
                    workspace,
                    package_root,
                    provider_id,
                    install_command,
                    reusable_reason,
                    started.elapsed(),
                    DependencyHydrationTermination::NotStarted,
                    DependencyHydrationStatus::Reused,
                    None,
                ));
                continue;
            }
        } else if !plan.outputs.is_empty() && declared_outputs_ready(path, &plan.outputs) {
            outcomes.push(hydration_outcome(
                workspace,
                package_root,
                provider_id,
                install_command,
                "provider_declared_outputs_ready".to_string(),
                started.elapsed(),
                DependencyHydrationTermination::NotStarted,
                DependencyHydrationStatus::Reused,
                None,
            ));
            continue;
        }

        (policy.on_progress)(&DependencyHydrationProgress {
            provider_id: provider_id.clone(),
            phase: "installing".to_string(),
            elapsed_ms: started.elapsed().as_millis(),
            last_progress_ms_ago: None,
        });
        let execution =
            match run_hydration_command(&plan.install, &provider_id, "installing", policy) {
                Ok(execution) => execution,
                Err(termination) => {
                    outcomes.push(hydration_outcome(
                        workspace,
                        package_root,
                        provider_id,
                        install_command,
                        stale_reason,
                        started.elapsed(),
                        termination,
                        DependencyHydrationStatus::Failed,
                        None,
                    ));
                    break;
                }
            };
        let exit_code = execution.exit_code;
        if !exit_code.is_some_and(|code| plan.install_success_exit_codes.contains(&code)) {
            outcomes.push(hydration_outcome(
                workspace,
                package_root,
                provider_id,
                install_command,
                stale_reason,
                started.elapsed(),
                DependencyHydrationTermination::ExitFailure,
                DependencyHydrationStatus::Failed,
                exit_code,
            ));
            break;
        }
        if !declared_outputs_ready(path, &plan.outputs) {
            outcomes.push(hydration_outcome(
                workspace,
                package_root,
                provider_id,
                install_command,
                "provider_declared_outputs_missing_after_install".to_string(),
                started.elapsed(),
                DependencyHydrationTermination::OutputValidationFailed,
                DependencyHydrationStatus::Failed,
                exit_code,
            ));
            break;
        }
        outcomes.push(hydration_outcome(
            workspace,
            package_root,
            provider_id,
            install_command,
            stale_reason,
            started.elapsed(),
            DependencyHydrationTermination::Completed,
            DependencyHydrationStatus::Succeeded,
            exit_code,
        ));
    }

    Ok(outcomes)
}

struct HydrationCommandExecution {
    exit_code: Option<i32>,
}

fn run_hydration_command(
    command: &provider::DependencyProviderCommand,
    provider_id: &str,
    phase: &str,
    policy: &DependencyHydrationPolicy,
) -> std::result::Result<HydrationCommandExecution, DependencyHydrationTermination> {
    if (policy.is_cancelled)() {
        return Err(DependencyHydrationTermination::Cancelled);
    }
    let declared_program = Path::new(&command.program);
    let resolved_program =
        if declared_program.is_relative() && declared_program.components().count() > 1 {
            command.cwd.join(declared_program)
        } else {
            declared_program.to_path_buf()
        };
    let mut process = Command::new(resolved_program);
    process
        .args(&command.args)
        .current_dir(&command.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    homeboy_engine_primitives::command::isolate_process_tree(&mut process);
    let mut child = process
        .spawn()
        .map_err(|_| DependencyHydrationTermination::SpawnFailed)?;
    let progress = Arc::clone(&policy.on_progress);
    let cancellation = Arc::clone(&policy.is_cancelled);
    let output =
        homeboy_engine_primitives::command::wait_with_bounded_output_supervised_with_progress(
            &mut child,
            DEPENDENCY_HYDRATION_OUTPUT_LIMIT_BYTES,
            policy.timeout.max(Duration::from_millis(1)),
            Some(policy.no_progress_timeout.max(Duration::from_millis(1))),
            policy.heartbeat_interval.max(Duration::from_millis(1)),
            move || cancellation(),
            |heartbeat| {
                progress(&DependencyHydrationProgress {
                    provider_id: provider_id.to_string(),
                    phase: phase.to_string(),
                    elapsed_ms: heartbeat.elapsed.as_millis(),
                    last_progress_ms_ago: heartbeat
                        .last_progress_elapsed
                        .map(|value| value.as_millis()),
                });
                Ok(())
            },
        )
        .map_err(|_| DependencyHydrationTermination::SpawnFailed)?;
    use homeboy_engine_primitives::command::SupervisedCommandTermination;
    match output.termination {
        SupervisedCommandTermination::Completed => Ok(HydrationCommandExecution {
            exit_code: output.output.status.code(),
        }),
        SupervisedCommandTermination::Cancelled => Err(DependencyHydrationTermination::Cancelled),
        SupervisedCommandTermination::TimedOut => Err(DependencyHydrationTermination::TimedOut),
        SupervisedCommandTermination::NoProgress => Err(DependencyHydrationTermination::NoProgress),
    }
}

fn declared_outputs_ready(path: &Path, outputs: &[DependencyInstallOutput]) -> bool {
    outputs.iter().all(|output| {
        let output_path = path.join(&output.path);
        match output.kind {
            DependencyInstallOutputKind::Path => output_path.exists(),
            DependencyInstallOutputKind::File => output_path.is_file(),
            DependencyInstallOutputKind::Directory => output_path.is_dir(),
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn hydration_outcome(
    workspace: &str,
    package_root: &str,
    provider_id: String,
    command: Vec<String>,
    reason: String,
    duration: Duration,
    termination: DependencyHydrationTermination,
    status: DependencyHydrationStatus,
    exit_code: Option<i32>,
) -> DependencyHydrationOutcome {
    DependencyHydrationOutcome {
        schema: DEPENDENCY_HYDRATION_SCHEMA.to_string(),
        workspace: workspace.to_string(),
        package_root: package_root.to_string(),
        provider_id,
        command,
        reason: crate::redaction::redact_string(&reason),
        duration_ms: duration.as_millis(),
        termination,
        status,
        exit_code,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DependencyUpdateOptions {
    pub install: bool,
    pub rebuild: bool,
}

impl Default for DependencyUpdateOptions {
    fn default() -> Self {
        Self {
            install: true,
            rebuild: false,
        }
    }
}

pub fn status(
    component_id: Option<&str>,
    path_override: Option<&str>,
    package_filter: Option<&str>,
) -> Result<DependencyStatus> {
    let (component, path) = resolve_component_path(component_id, path_override)?;
    let providers = provider::resolve_dependency_providers(&component, &path)?;
    let mut statuses = Vec::new();

    for provider in providers {
        statuses.push(provider.status(&component, &path, package_filter)?);
    }

    Ok(combine_provider_statuses(&component, &path, statuses))
}

pub fn status_value(
    component_id: Option<&str>,
    path_override: Option<&str>,
    package_filter: Option<&str>,
) -> Result<serde_json::Value> {
    serialize_dependency_output(
        status(component_id, path_override, package_filter)?,
        "serialize deps status",
    )
}

pub fn update(
    component_id: Option<&str>,
    path_override: Option<&str>,
    package: &str,
    constraint: Option<&str>,
    options: DependencyUpdateOptions,
) -> Result<DependencyUpdateResult> {
    let (component, path) = resolve_component_path(component_id, path_override)?;
    let providers = provider::resolve_dependency_providers(&component, &path)?;

    for provider in providers {
        if provider.handles_package(&component, &path, package)? {
            let mut result = provider.update(&component, &path, package, constraint)?;
            if options.install {
                result.install = provider.install(&component, &path)?;
            }
            if options.rebuild {
                result.rebuild = Some(rebuild_component(&component, &path)?);
            }
            return Ok(result);
        }
    }

    Err(Error::validation_invalid_argument(
        "package",
        format!(
            "No dependency provider for component '{}' manages package '{}'",
            component.id, package
        ),
        Some(package.to_string()),
        None,
    ))
}

pub fn update_value(
    component_id: Option<&str>,
    path_override: Option<&str>,
    package: &str,
    constraint: Option<&str>,
    install: bool,
    rebuild: bool,
) -> Result<serde_json::Value> {
    serialize_dependency_output(
        update(
            component_id,
            path_override,
            package,
            constraint,
            DependencyUpdateOptions { install, rebuild },
        )?,
        "serialize deps update",
    )
}

/// Install a component's dependencies through its resolved dependency providers.
///
/// This is the detection/config-driven replacement for hardcoded
/// per-ecosystem install CI policy: the package manager(s) are chosen by
/// [`provider::resolve_dependency_providers`] based on the manifest and lock
/// files present in the workspace and the component/extension manifest — not
/// by shell literals in the calling environment. CI (or any caller) runs
/// `homeboy component setup` or `homeboy deps install` and lets core own the
/// policy.
pub fn install(
    component_id: Option<&str>,
    path_override: Option<&str>,
) -> Result<DependencyInstallResult> {
    let (component, path) = resolve_component_path(component_id, path_override)?;
    // Reuse the command-facing resolver so a dependency-less component returns
    // the same actionable "no provider" error as `deps status`/`deps update`.
    provider::resolve_dependency_providers(&component, &path)?;
    Ok(
        install_for_resolved(&component, &path)?.unwrap_or_else(|| DependencyInstallResult {
            component_id: component.id.clone(),
            component_path: path.display().to_string(),
            package_manager: String::new(),
            installs: Vec::new(),
        }),
    )
}

pub fn install_value(
    component_id: Option<&str>,
    path_override: Option<&str>,
) -> Result<serde_json::Value> {
    serialize_dependency_output(
        install(component_id, path_override)?,
        "serialize deps install",
    )
}

/// Run dependency installs for an already-resolved component/workspace pair.
///
/// Shared by [`install`] and the higher-level `component setup` orchestrator so
/// the provider-resolution and best-effort install policy lives in exactly one
/// place.
///
/// Returns `None` when the workspace exposes no dependency provider (nothing to
/// install) so callers can treat a dependency-less component as a no-op.
pub fn install_for_resolved(
    component: &Component,
    path: &Path,
) -> Result<Option<DependencyInstallResult>> {
    let (dependency_root, providers) = resolve_dependency_workspace(component, path)?;
    if providers.is_empty() {
        return Ok(None);
    }

    let mut installs = Vec::new();
    let mut package_managers = Vec::new();
    for provider in providers {
        let status = provider.status(component, &dependency_root, None)?;
        if let Some(result) = provider.install(component, &dependency_root)? {
            package_managers.push(status.package_manager);
            installs.push(result);
        }
    }

    let package_manager = match package_managers.as_slice() {
        [] => String::new(),
        [only] => only.clone(),
        many => many.join(","),
    };

    Ok(Some(DependencyInstallResult {
        component_id: component.id.clone(),
        component_path: dependency_root.display().to_string(),
        package_manager,
        installs,
    }))
}

/// Resolve providers from the component path, then walk to the repository root
/// so monorepo components can use a workspace-level dependency provider.
fn resolve_dependency_workspace(
    component: &Component,
    path: &Path,
) -> Result<(PathBuf, Vec<provider::DependencyProvider>)> {
    let repository_root = crate::git::get_git_root(&path.display().to_string())
        .ok()
        .map(PathBuf::from);
    let canonical_repository_root = repository_root
        .as_ref()
        .and_then(|root| root.canonicalize().ok());
    let mut candidate = path.to_path_buf();

    loop {
        let providers = provider::resolve_dependency_providers_optional(component, &candidate)?;
        if !providers.is_empty() {
            return Ok((candidate, providers));
        }
        if repository_root.as_deref() == Some(candidate.as_path())
            || candidate.canonicalize().ok().as_ref() == canonical_repository_root.as_ref()
        {
            break;
        }
        let Some(parent) = candidate.parent() else {
            break;
        };
        candidate = parent.to_path_buf();
        if repository_root.is_none() {
            break;
        }
    }

    Ok((path.to_path_buf(), Vec::new()))
}

/// A single provider's dependency-install command for a detected workspace,
/// without executing it. Produced by [`dependency_install_plan`] so callers
/// (e.g. Lab workspace hydration) can detect providers on the controller using
/// the existing machinery and run the same install command on a runner (#7366).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyInstallPlanStep {
    /// Dependency-provider id reporting the manifest that triggered detection.
    /// Matches the `package_manager` reported by `deps status` for the same
    /// provider.
    pub provider_id: String,
    /// Portable install invocation the runner can execute without receiving a
    /// controller-local extension path.
    pub invocation: DependencyInstallInvocation,
    /// Filesystem outputs that prove this provider's install/build preparation
    /// is present in a materialized workspace.
    pub outputs: Vec<DependencyInstallOutput>,
}

/// A provider-declared output required before a prepared source can be reused.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyInstallOutput {
    pub path: String,
    pub kind: DependencyInstallOutputKind,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DependencyInstallOutputKind {
    Path,
    File,
    Directory,
}

/// An install command suitable for crossing a controller/runner boundary.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DependencyInstallInvocation {
    /// A provider command that does not reference an installed extension path.
    Argv { argv: Vec<String> },
    /// An extension-owned entrypoint. The runner resolves `entrypoint` inside
    /// its own materialized copy of `extension_id` before executing `argv`.
    ExtensionEntrypoint {
        extension_id: String,
        entrypoint: String,
        /// The executable and argument list with the entrypoint removed.
        argv: Vec<String>,
        entrypoint_index: usize,
    },
}

/// Detect dependency providers for a workspace path and return the install
/// command each would run, without executing any of them.
///
/// Reuses [`provider::resolve_dependency_providers_optional`] (the detection
/// behind `homeboy deps install`) so a manifest detected by an existing provider
/// surfaces its install command here. A linked extension set that does not
/// provide dependency support is equivalent to no provider for this optional
/// planning surface; invalid explicit capability ownership still fails.
/// Providers whose install cannot be expressed as a standalone shell command
/// (component-script/extension providers) are omitted. Returns an empty vector
/// when no provider detects the workspace.
///
/// The lockfile/manifest files are part of the synced snapshot (only built
/// dependency trees like `vendor/`/`node_modules/` are excluded), so detecting
/// against the controller-side source path yields the same providers the
/// materialized runner workspace exposes.
pub fn dependency_install_plan(path: &Path) -> Result<Vec<DependencyInstallPlanStep>> {
    let (component, resolved_path) =
        resolve_component_path(None, Some(&path.display().to_string()))?;
    let providers =
        match provider::resolve_dependency_providers_optional(&component, &resolved_path) {
            Ok(providers) => providers,
            Err(error) => {
                if !crate::extension_execution::has_linked_extension_for_capability(
                    &component,
                    homeboy_extension_contract::ExtensionCapability::Deps,
                )? {
                    Vec::new()
                } else {
                    return Err(error);
                }
            }
        };
    let mut steps = Vec::new();
    for provider in providers {
        let status = provider.status(&component, &resolved_path, None)?;
        if let Some(command) = provider.install_command(&component, &resolved_path)? {
            steps.push(DependencyInstallPlanStep {
                provider_id: status.package_manager,
                invocation: dependency_install_invocation(command.argv())?,
                outputs: provider.install_outputs()?,
            });
        }
    }
    Ok(steps)
}

fn dependency_install_invocation(argv: Vec<String>) -> Result<DependencyInstallInvocation> {
    for (entrypoint_index, value) in argv.iter().enumerate() {
        if let Some((extension_id, entrypoint)) = installed_extension_entrypoint(Path::new(value)) {
            return Ok(extension_install_invocation(
                argv,
                entrypoint_index,
                extension_id,
                entrypoint,
            ));
        }
    }
    for extension in crate::extension_store::load_all_extensions()? {
        let Some(root) = extension.extension_path else {
            continue;
        };
        for (entrypoint_index, value) in argv.iter().enumerate() {
            let path = Path::new(value);
            if !path.is_absolute() {
                continue;
            }
            if let Ok(entrypoint) = path.strip_prefix(&root) {
                let entrypoint = entrypoint.to_string_lossy().to_string();
                return Ok(extension_install_invocation(
                    argv,
                    entrypoint_index,
                    extension.id,
                    entrypoint,
                ));
            }
        }
    }
    Ok(DependencyInstallInvocation::Argv { argv })
}

fn installed_extension_entrypoint(path: &Path) -> Option<(String, String)> {
    let components = path.components().collect::<Vec<_>>();
    let marker = [".config", "homeboy", "extensions"];
    let marker_index = components.windows(marker.len()).position(|window| {
        window
            .iter()
            .zip(marker)
            .all(|(component, expected)| component.as_os_str() == expected)
    })?;
    let extension_id = components.get(marker_index + marker.len())?;
    let entrypoint = components.get(marker_index + marker.len() + 1..)?;
    if entrypoint.is_empty() {
        return None;
    }
    Some((
        extension_id.as_os_str().to_string_lossy().to_string(),
        entrypoint
            .iter()
            .map(|component| component.as_os_str())
            .collect::<PathBuf>()
            .to_string_lossy()
            .to_string(),
    ))
}

fn extension_install_invocation(
    mut argv: Vec<String>,
    entrypoint_index: usize,
    extension_id: String,
    entrypoint: String,
) -> DependencyInstallInvocation {
    argv.remove(entrypoint_index);
    DependencyInstallInvocation::ExtensionEntrypoint {
        extension_id,
        entrypoint,
        argv,
        entrypoint_index,
    }
}

fn rebuild_component(component: &Component, path: &Path) -> Result<DependencyCommandResult> {
    let mut build_component = component.clone();
    build_component.local_path = path.display().to_string();
    let (result, exit_code) =
        crate::component_build_provider::run_component_build(&build_component)?;
    let stdout = serde_json::to_string(&result).map_err(|e| {
        Error::internal_json(e.to_string(), Some("serialize deps rebuild".to_string()))
    })?;
    let command = vec![
        "homeboy".to_string(),
        "build".to_string(),
        component.id.clone(),
        "--path".to_string(),
        path.display().to_string(),
    ];

    if exit_code != 0 {
        return Err(Error::validation_invalid_argument(
            "rebuild",
            format!(
                "Dependency update rebuild failed for '{}' with status {}",
                component.id, exit_code
            ),
            Some(component.id.clone()),
            Some(vec![format!("Run manually: {}", command.join(" "))]),
        ));
    }

    Ok(DependencyCommandResult {
        command,
        skipped: false,
        status: Some(exit_code),
        stdout,
        stderr: String::new(),
    })
}

pub fn stack_status_value() -> Result<serde_json::Value> {
    serialize_dependency_output(stack_status()?, "serialize deps stack status")
}

pub fn stack_plan_value(upstream: &str) -> Result<serde_json::Value> {
    serialize_dependency_output(stack_plan(upstream)?, "serialize deps stack plan")
}

pub fn stack_apply_value(
    upstream: &str,
    constraint: Option<&str>,
    dry_run: bool,
    install: bool,
    rebuild: bool,
) -> Result<serde_json::Value> {
    serialize_dependency_output(
        stack_apply(upstream, constraint, dry_run, install, rebuild)?,
        "serialize deps stack apply",
    )
}

fn serialize_dependency_output<T: Serialize>(value: T, context: &str) -> Result<serde_json::Value> {
    serde_json::to_value(value)
        .map_err(|e| Error::internal_json(e.to_string(), Some(context.to_string())))
}

fn resolve_component_path(
    component_id: Option<&str>,
    path_override: Option<&str>,
) -> Result<(Component, PathBuf)> {
    let component = component::resolve_effective(component_id, path_override, None)?;
    let path = PathBuf::from(shellexpand::tilde(&component.local_path).as_ref());

    if !path.exists() {
        return Err(Error::validation_invalid_argument(
            "component_path",
            format!(
                "Component '{}' path does not exist: {}",
                component.id,
                path.display()
            ),
            Some(component.id.clone()),
            None,
        ));
    }

    Ok((component, path))
}

fn combine_provider_statuses(
    component: &Component,
    path: &Path,
    statuses: Vec<provider::ProviderDependencyStatus>,
) -> DependencyStatus {
    let package_manager = match statuses.as_slice() {
        [only] => only.package_manager.clone(),
        _ => statuses
            .iter()
            .map(|status| status.package_manager.as_str())
            .collect::<Vec<_>>()
            .join(","),
    };
    let packages = statuses
        .into_iter()
        .flat_map(|status| status.packages)
        .collect();

    DependencyStatus {
        component_id: component.id.clone(),
        component_path: path.display().to_string(),
        package_manager,
        packages,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    #[test]
    fn extension_owned_install_path_becomes_portable_invocation() {
        crate::test_support::with_isolated_home(|home| {
            let extension_id = "fixture-runtime";
            let extension_root = home
                .path()
                .join(".config/homeboy/extensions")
                .join(extension_id);
            std::fs::create_dir_all(extension_root.join("scripts")).expect("extension root");
            std::fs::write(
                extension_root.join(format!("{extension_id}.json")),
                r#"{"name":"Fixture runtime","version":"1.0.0"}"#,
            )
            .expect("extension manifest");
            let invocation = dependency_install_invocation(vec![
                "sh".to_string(),
                extension_root
                    .join("scripts/install.sh")
                    .display()
                    .to_string(),
                "install".to_string(),
            ])
            .expect("portable invocation");

            assert_eq!(
                invocation,
                DependencyInstallInvocation::ExtensionEntrypoint {
                    extension_id: extension_id.to_string(),
                    entrypoint: "scripts/install.sh".to_string(),
                    argv: vec!["sh".to_string(), "install".to_string()],
                    entrypoint_index: 1,
                }
            );
        });
    }

    #[test]
    fn configured_extension_asset_from_foreign_controller_home_becomes_portable() {
        crate::test_support::with_isolated_home(|_active_home| {
            let invocation = dependency_install_invocation(vec![
                "bash".to_string(),
                "/controller/home/.config/homeboy/extensions/fixture-runtime/scripts/install.sh"
                    .to_string(),
                "install".to_string(),
            ])
            .expect("portable invocation");

            assert_eq!(
                invocation,
                DependencyInstallInvocation::ExtensionEntrypoint {
                    extension_id: "fixture-runtime".to_string(),
                    entrypoint: "scripts/install.sh".to_string(),
                    argv: vec!["bash".to_string(), "install".to_string()],
                    entrypoint_index: 1,
                }
            );
        });
    }

    #[test]
    fn dependency_install_plan_skips_linked_extensions_without_deps_support() {
        crate::test_support::with_isolated_home(|home| {
            let project = tempfile::tempdir().expect("project tempdir");
            let extension_id = "fixture-non-deps";
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions")
                .join(extension_id);
            std::fs::create_dir_all(&extension_dir).expect("extension dir");
            std::fs::write(
                extension_dir.join(format!("{extension_id}.json")),
                r#"{"name":"Fixture non-deps","version":"1.0.0"}"#,
            )
            .expect("extension manifest");
            std::fs::write(
                project.path().join("homeboy.json"),
                format!(
                    r#"{{"id":"fixture","local_path":"{}","extensions":{{"{extension_id}":{{}}}}}}"#,
                    project.path().display()
                ),
            )
            .expect("component manifest");

            let plan = dependency_install_plan(project.path())
                .expect("unrelated linked extensions do not require dependency hydration");

            assert!(plan.is_empty());
        });
    }

    fn hydration_policy(
        cancelled: Arc<AtomicBool>,
        progress: Arc<Mutex<Vec<String>>>,
    ) -> DependencyHydrationPolicy {
        DependencyHydrationPolicy {
            timeout: Duration::from_secs(3),
            no_progress_timeout: Duration::from_millis(150),
            heartbeat_interval: Duration::from_millis(10),
            is_cancelled: Arc::new(move || cancelled.load(Ordering::SeqCst)),
            on_progress: Arc::new(move |event| {
                progress.lock().unwrap().push(event.phase.clone());
            }),
        }
    }

    #[test]
    fn provider_declared_reusable_state_skips_install() {
        crate::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("provider workspace");
            std::fs::write(
                root.path().join("homeboy-deps.json"),
                r#"{
                    "provider":"fixture-provider",
                    "commands":{
                        "reusable":{
                            "argv":["sh","-c","exit 0"],
                            "reusable_reason":"fixture_state_matches",
                            "stale_reason":"fixture_state_differs"
                        },
                        "install":{"argv":["sh","-c","printf installed > install-ran"]}
                    }
                }"#,
            )
            .expect("provider manifest");
            let outcomes = hydrate_declared_dependencies(
                root.path(),
                "fixture",
                ".",
                &hydration_policy(
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(Mutex::new(Vec::new())),
                ),
            )
            .expect("reusable state assessment");

            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].status, DependencyHydrationStatus::Reused);
            assert_eq!(outcomes[0].reason, "fixture_state_matches");
            assert_eq!(
                outcomes[0].termination,
                DependencyHydrationTermination::NotStarted
            );
            assert!(!root.path().join("install-ran").exists());
        });
    }

    #[cfg(unix)]
    #[test]
    fn stale_state_runs_exact_declared_command_and_reports_progress() {
        crate::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("provider workspace");
            std::fs::write(
                root.path().join("install-fixture"),
                "#!/bin/sh\nprintf '%s' \"$*\" > invoked-argv\nprintf ready > prepared.state\n",
            )
            .expect("fixture installer");
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                root.path().join("install-fixture"),
                std::fs::Permissions::from_mode(0o755),
            )
            .expect("fixture permissions");
            std::fs::write(
                root.path().join("homeboy-deps.json"),
                r#"{
                    "provider":"fixture-provider",
                    "commands":{
                        "reusable":{
                            "argv":["sh","-c","exit 1"],
                            "reusable_reason":"fixture_state_matches",
                            "stale_reason":"fixture_state_differs"
                        },
                        "install":{"argv":["./install-fixture","--mode","declared"]}
                    },
                    "outputs":[{"path":"prepared.state","kind":"file"}]
                }"#,
            )
            .expect("provider manifest");
            let progress = Arc::new(Mutex::new(Vec::new()));
            let outcomes = hydrate_declared_dependencies(
                root.path(),
                "fixture",
                ".",
                &hydration_policy(Arc::new(AtomicBool::new(false)), Arc::clone(&progress)),
            )
            .expect("stale state hydration");

            assert_eq!(
                std::fs::read_to_string(root.path().join("invoked-argv")).unwrap(),
                "--mode declared"
            );
            assert_eq!(outcomes[0].status, DependencyHydrationStatus::Succeeded);
            assert_eq!(outcomes[0].reason, "fixture_state_differs");
            assert_eq!(
                outcomes[0].command,
                vec!["./install-fixture", "--mode", "declared"]
            );
            let progress = progress.lock().unwrap();
            assert!(progress
                .iter()
                .any(|phase| phase == "assessing_reusable_state"));
            assert!(progress.iter().any(|phase| phase == "installing"));
        });
    }

    #[test]
    fn stalled_declared_install_is_bounded_by_no_progress() {
        crate::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("provider workspace");
            std::fs::write(
                root.path().join("homeboy-deps.json"),
                r#"{"provider":"fixture-provider","commands":{"install":{"argv":["sh","-c","sleep 5"]}}}"#,
            )
            .expect("provider manifest");
            let started = Instant::now();
            let outcomes = hydrate_declared_dependencies(
                root.path(),
                "fixture",
                ".",
                &hydration_policy(
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(Mutex::new(Vec::new())),
                ),
            )
            .expect("bounded hydration outcome");

            assert_eq!(outcomes[0].status, DependencyHydrationStatus::Failed);
            assert_eq!(
                outcomes[0].termination,
                DependencyHydrationTermination::NoProgress
            );
            assert!(started.elapsed() < Duration::from_secs(2));
        });
    }

    #[test]
    fn cancellation_stops_declared_install() {
        crate::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("provider workspace");
            std::fs::write(
                root.path().join("homeboy-deps.json"),
                r#"{"provider":"fixture-provider","commands":{"install":{"argv":["sh","-c","sleep 5"]}}}"#,
            )
            .expect("provider manifest");
            let started = Instant::now();
            let outcomes = hydrate_declared_dependencies(
                root.path(),
                "fixture",
                ".",
                &hydration_policy(
                    Arc::new(AtomicBool::new(true)),
                    Arc::new(Mutex::new(Vec::new())),
                ),
            )
            .expect("cancelled hydration outcome");

            assert_eq!(
                outcomes[0].termination,
                DependencyHydrationTermination::Cancelled
            );
            assert!(started.elapsed() < Duration::from_secs(2));
        });
    }

    #[cfg(unix)]
    #[test]
    fn hydration_evidence_redacts_declared_command_secrets() {
        crate::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("provider workspace");
            std::fs::write(
                root.path().join("install-fixture"),
                "#!/bin/sh\nprintf ready > prepared.state\n",
            )
            .expect("fixture installer");
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                root.path().join("install-fixture"),
                std::fs::Permissions::from_mode(0o755),
            )
            .expect("fixture permissions");
            std::fs::write(
                root.path().join("homeboy-deps.json"),
                r#"{
                    "provider":"fixture-provider",
                    "commands":{"install":{"argv":["./install-fixture","--token","fixture-secret-value"]}},
                    "outputs":[{"path":"prepared.state","kind":"file"}]
                }"#,
            )
            .expect("provider manifest");
            let outcomes = hydrate_declared_dependencies(
                root.path(),
                "fixture",
                ".",
                &hydration_policy(
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(Mutex::new(Vec::new())),
                ),
            )
            .expect("redacted hydration evidence");
            let evidence = serde_json::to_string(&outcomes).unwrap();

            assert!(!evidence.contains("fixture-secret-value"));
            assert!(evidence.contains("[REDACTED]"));
        });
    }
}
