//! Source-built CLI dependency discovery and bootstrap for lab workspaces.
//!
//! Collects candidate workspace paths from provider configs and component
//! contracts, resolves source-built Node CLI dependencies by parsing bare
//! module imports, and bootstraps their production dependencies on the runner.
//!
//! Split out of `lab_workspaces.rs` to keep the workspace-mapping entry points
//! separate from the dependency-discovery internals.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::core::component::{self, TargetSpec};
use crate::core::{Error, Result};

use super::lab_workspaces::{
    ExtraLabWorkspace, LAB_EXTRA_WORKSPACES_ENV, LAB_EXTRA_WORKSPACES_JSON_ENV,
};
use super::workspace::git_output;
use super::{
    exec, preflight_remote_argv_path_translation, RunnerCapabilityPreflight, RunnerExecOptions,
    RunnerRequiredTool,
};

pub(super) fn provider_config_candidate_paths(value: &serde_json::Value) -> Vec<String> {
    let mut paths = Vec::new();
    collect_provider_config_candidate_paths(value, &mut paths);
    paths
}

pub(super) fn component_contract_candidate_paths(value: &serde_json::Value) -> Vec<String> {
    let mut paths = Vec::new();
    collect_component_contract_candidate_paths(value, &mut paths);
    paths
}

pub(super) fn add_candidate_extra_workspace(
    candidate: &str,
    role: &str,
    source_canon: &Path,
    seen: &mut BTreeSet<PathBuf>,
    workspaces: &mut Vec<ExtraLabWorkspace>,
) -> Result<()> {
    let expanded = shellexpand::tilde(candidate).to_string();
    let path = Path::new(&expanded);
    let (workspace_path, snapshot_includes, bootstrap_node_dependencies) = if path.is_dir() {
        (containing_checkout_or_dir(path)?, Vec::new(), false)
    } else if path.is_file() {
        let workspace_path = containing_checkout_or_parent(path)?;
        let snapshot_includes = provider_config_file_snapshot_includes(&workspace_path, path);
        (
            workspace_path,
            snapshot_includes,
            is_node_cli_file(path) && source_cli_workspace_has_package_lock(path),
        )
    } else {
        return Ok(());
    };
    let canon = match workspace_path.canonicalize() {
        Ok(canon) => canon,
        Err(_) => return Ok(()),
    };
    // The primary workspace (and paths inside it) are already synced.
    if canon == source_canon || canon.starts_with(source_canon) {
        return Ok(());
    }
    if !seen.insert(canon.clone()) {
        if let Some(existing) = workspaces
            .iter_mut()
            .find(|workspace| workspace.path == canon)
        {
            for include in snapshot_includes {
                if !existing.snapshot_includes.contains(&include) {
                    existing.snapshot_includes.push(include);
                }
            }
            existing.bootstrap_node_dependencies |= bootstrap_node_dependencies;
        }
        return Ok(());
    }
    workspaces.push(ExtraLabWorkspace {
        role: role.to_string(),
        path: canon,
        snapshot_includes,
        bootstrap_node_dependencies,
        source_provenance: None,
    });
    Ok(())
}

pub(super) fn collect_provider_config_candidate_paths(
    value: &serde_json::Value,
    paths: &mut Vec<String>,
) {
    match value {
        serde_json::Value::String(text) => {
            if is_controller_path_like(text) {
                paths.push(text.to_string());
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_provider_config_candidate_paths(item, paths);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                collect_provider_config_candidate_paths(item, paths);
            }
        }
        _ => {}
    }
}

pub(super) fn collect_component_contract_candidate_paths(
    value: &serde_json::Value,
    paths: &mut Vec<String>,
) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_component_contract_candidate_paths(item, paths);
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(contracts) = map.get("component_contracts") {
                collect_component_contract_paths(contracts, paths);
            }
            for item in map.values() {
                collect_component_contract_candidate_paths(item, paths);
            }
        }
        _ => {}
    }
}

pub(super) fn collect_component_contract_paths(value: &serde_json::Value, paths: &mut Vec<String>) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_component_contract_paths(item, paths);
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(path) = map
                .get("path")
                .and_then(serde_json::Value::as_str)
                .filter(|path| is_controller_path_like(path))
            {
                paths.push(path.to_string());
            }
        }
        _ => {}
    }
}

pub(super) fn is_controller_path_like(value: &str) -> bool {
    value.starts_with('/') || value.starts_with("~/")
}

pub(super) fn provider_config_source_cli_files(value: &serde_json::Value) -> Vec<PathBuf> {
    provider_config_candidate_paths(value)
        .into_iter()
        .map(|candidate| shellexpand::tilde(&candidate).to_string())
        .map(PathBuf::from)
        .filter(|path| path.is_file() && is_node_cli_file(path))
        .collect()
}

pub(super) fn is_node_cli_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("js" | "mjs" | "cjs")
    )
}

pub(super) fn containing_checkout_or_parent(path: &Path) -> Result<PathBuf> {
    let dir = path.parent().unwrap_or(path);
    if let Ok(root) = git_output(dir, &["rev-parse", "--show-toplevel"]) {
        return Ok(PathBuf::from(root));
    }
    canonical_existing_dir(&dir.display().to_string(), "provider_config")
}

fn containing_checkout_or_dir(path: &Path) -> Result<PathBuf> {
    if let Ok(root) = git_output(path, &["rev-parse", "--show-toplevel"]) {
        return Ok(PathBuf::from(root));
    }
    canonical_existing_dir(&path.display().to_string(), "provider_config")
}

pub(super) fn bare_module_imports(content: &str) -> BTreeSet<String> {
    let mut imports = BTreeSet::new();
    for marker in [
        "from '",
        "from \"",
        "require('",
        "require(\"",
        "import('",
        "import(\"",
    ] {
        collect_imports_after_marker(content, marker, &mut imports);
    }
    imports
        .into_iter()
        .filter(|specifier| {
            !specifier.starts_with('.')
                && !specifier.starts_with('/')
                && !is_builtin_module_specifier(specifier)
        })
        .collect()
}

pub(super) fn is_builtin_module_specifier(specifier: &str) -> bool {
    let specifier = specifier.strip_prefix("node:").unwrap_or(specifier);
    matches!(
        specifier,
        "assert"
            | "buffer"
            | "child_process"
            | "crypto"
            | "events"
            | "fs"
            | "http"
            | "https"
            | "module"
            | "os"
            | "path"
            | "process"
            | "stream"
            | "url"
            | "util"
    )
}

pub(super) fn collect_imports_after_marker(
    content: &str,
    marker: &str,
    imports: &mut BTreeSet<String>,
) {
    let quote = marker.chars().last().unwrap_or('\'');
    let mut rest = content;
    while let Some(index) = rest.find(marker) {
        let after = &rest[index + marker.len()..];
        if let Some(end) = after.find(quote) {
            imports.insert(after[..end].to_string());
            rest = &after[end + 1..];
        } else {
            break;
        }
    }
}

pub(super) fn accepted_extra_lab_workspaces() -> Result<Vec<ExtraLabWorkspace>> {
    let mut paths = Vec::new();
    if let Ok(raw) = std::env::var(LAB_EXTRA_WORKSPACES_JSON_ENV) {
        if !raw.trim().is_empty() {
            let parsed: Vec<String> = serde_json::from_str(&raw).map_err(|err| {
                Error::validation_invalid_argument(
                    LAB_EXTRA_WORKSPACES_JSON_ENV,
                    format!("{LAB_EXTRA_WORKSPACES_JSON_ENV} must be a JSON array of paths: {err}"),
                    Some(raw.clone()),
                    None,
                )
            })?;
            paths.extend(parsed);
        }
    }
    if let Ok(raw) = std::env::var(LAB_EXTRA_WORKSPACES_ENV) {
        paths.extend(
            raw.lines()
                .flat_map(|line| line.split(','))
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .map(str::to_string),
        );
    }

    paths
        .into_iter()
        .map(|path| {
            Ok(ExtraLabWorkspace {
                role: "extra".to_string(),
                path: canonical_existing_dir(&path, "extra_workspace")?,
                snapshot_includes: Vec::new(),
                bootstrap_node_dependencies: false,
                source_provenance: None,
            })
        })
        .collect()
}

pub(super) fn discovered_validation_dependency_workspaces(
    source_path: &Path,
) -> Result<Vec<ExtraLabWorkspace>> {
    let source_path_string = source_path.display().to_string();
    let Ok(target) = component::resolve_target(TargetSpec {
        component_id: None,
        path_override: Some(&source_path_string),
        project: None,
        capability: None,
        allow_synthetic: true,
        accept_bare_directory: true,
        ..TargetSpec::default()
    }) else {
        return Ok(Vec::new());
    };
    let Some(extensions) = target.component.extensions.as_ref() else {
        return Ok(Vec::new());
    };

    let mut workspaces = Vec::new();
    for config in extensions.values() {
        let Some(dependencies) = config.settings.get("validation_dependencies") else {
            continue;
        };
        let Some(dependencies) = dependencies.as_array() else {
            continue;
        };
        for dependency in dependencies.iter().filter_map(|value| value.as_str()) {
            let path = resolve_dependency_workspace_path(dependency)?;
            workspaces.push(ExtraLabWorkspace {
                role: "dependency".to_string(),
                path,
                snapshot_includes: Vec::new(),
                bootstrap_node_dependencies: false,
                source_provenance: None,
            });
        }
    }

    Ok(workspaces)
}

pub(super) fn resolve_dependency_workspace_path(dependency: &str) -> Result<PathBuf> {
    let expanded = shellexpand::tilde(dependency).to_string();
    if Path::new(&expanded).is_dir() {
        return canonical_existing_dir(&expanded, "validation_dependencies");
    }

    let component = component::resolve_effective(Some(dependency), None, None).map_err(|err| {
        Error::validation_invalid_argument(
            "validation_dependencies",
            format!(
                "Runner workspace sync cannot resolve validation dependency `{dependency}` to a local checkout: {}",
                err.message
            ),
            Some(dependency.to_string()),
            Some(vec![
                format!("Register the dependency component locally, or pass an explicit checkout path via {LAB_EXTRA_WORKSPACES_JSON_ENV}."),
            ]),
        )
    })?;
    canonical_existing_dir(&component.local_path, "validation_dependencies")
}

pub(super) fn canonical_existing_dir(path: &str, field: &str) -> Result<PathBuf> {
    let expanded = shellexpand::tilde(path).to_string();
    let path = Path::new(&expanded);
    if !path.is_dir() {
        return Err(Error::validation_invalid_argument(
            field,
            format!("Runner workspace path must be an existing directory: {expanded}"),
            Some(expanded),
            None,
        ));
    }
    path.canonicalize().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("canonicalize runner workspace path".to_string()),
        )
    })
}

pub(super) fn provider_config_file_snapshot_includes(
    workspace_path: &Path,
    file_path: &Path,
) -> Vec<String> {
    let workspace_path = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let file_path = file_path
        .canonicalize()
        .unwrap_or_else(|_| file_path.to_path_buf());
    let Ok(relative) = file_path.strip_prefix(&workspace_path) else {
        return Vec::new();
    };
    let Some(parent) = relative.parent() else {
        return Vec::new();
    };
    let parent = parent.display().to_string();
    if parent.is_empty() {
        return Vec::new();
    }
    vec![parent.clone(), format!("{parent}/**")]
}

pub(super) fn source_cli_workspace_has_package_lock(file_path: &Path) -> bool {
    containing_checkout_or_parent(file_path)
        .ok()
        .is_some_and(|workspace| workspace.join("package-lock.json").is_file())
}

/// Capability-parity contract for the runner-side source-CLI dependency
/// bootstrap. The clean dependency install is executed by the runner's
/// JavaScript package-manager toolchain, so the runner must expose the
/// corresponding runtime and package-manager tools. `exec` short-circuits this
/// preflight for local runners and for SSH runners that already advertise the
/// tools, so it is behavior-preserving on a provisioned runner and fails loudly
/// before a remote run that would otherwise error mid-dispatch (#5422).
pub(super) fn source_cli_bootstrap_capability_preflight() -> RunnerCapabilityPreflight {
    RunnerCapabilityPreflight {
        command: "lab.source_cli_bootstrap".to_string(),
        required_tools: vec![RunnerRequiredTool::Node, RunnerRequiredTool::Npm],
        required_commands: Vec::new(),
        required_tool_capabilities: Vec::new(),
        required_components: Vec::new(),
        required_env: Vec::new(),
    }
}

/// Path-translation preflight for a runtime-overlay install step. The overlay
/// artifact has already been synced to a runner-side remote path and the opaque
/// install command runs with the resolved remote workdir as cwd. Assert that no
/// controller-local workspace path survived un-translated into the install argv
/// before handing it to the runner, so a missed remap fails loudly here instead
/// of installing against a non-existent local path. Kept ecosystem-agnostic:
/// the command is opaque config data and is only checked for stray local paths.
pub(super) fn preflight_runtime_overlay_install_argv(
    runner_id: &str,
    command: &[String],
    local_path: &Path,
    remote_workdir: &str,
) -> Result<()> {
    preflight_remote_argv_path_translation(
        "Lab runtime-overlay install",
        runner_id,
        command,
        local_path,
        remote_workdir,
    )
}

pub(super) fn bootstrap_source_cli_node_dependencies(
    runner_id: &str,
    local_path: &str,
    remote_path: &str,
) -> Result<()> {
    let command = vec![
        "npm".to_string(),
        "ci".to_string(),
        "--omit=dev".to_string(),
        "--ignore-scripts".to_string(),
    ];

    // Path-translation preflight: the source-built CLI workspace has already been
    // synced to a runner-side remote path; the dependency install runs with the
    // remote workspace as cwd. Assert that no controller-local workspace path
    // survived un-translated into the dispatched argv before handing it to the
    // remote runner, so a missed remap fails loudly here instead of installing
    // against a non-existent local path (#5422).
    preflight_remote_argv_path_translation(
        "Lab source-CLI dependency bootstrap",
        runner_id,
        &command,
        Path::new(local_path),
        remote_path,
    )?;

    let (output, exit_code) = exec(
        runner_id,
        RunnerExecOptions {
            cwd: Some(remote_path.to_string()),
            project_id: None,
            allow_diagnostic_ssh: false,
            command,
            env: HashMap::new(),
            secret_env_names: Vec::new(),
            capture_patch: false,
            raw_exec: true,
            source_snapshot: None,
            // Validate remote capability parity before dispatch: the bootstrap is
            // executed by the runner-side JavaScript package-manager toolchain, so
            // require those tools on the runner. `exec` no-ops this gate for local
            // and already-capable SSH runners, so behavior is unchanged on a
            // correctly provisioned runner and fails early otherwise (#5422).
            capability_preflight: Some(source_cli_bootstrap_capability_preflight()),
            required_extensions: Vec::new(),
            require_paths: Vec::new(),
            runner_workload: None,
            run_id: None,
            detach_after_handoff: false,
            mirror_evidence: true,
            print_handoff: true,
        },
    )?;
    if exit_code == 0 {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "provider_config",
        format!(
            "Lab offload could not install production dependencies for source-built CLI workspace `{remote_path}`"
        ),
        Some(remote_path.to_string()),
        Some(vec![
            format!("dependency install stderr: {}", output.stderr.trim()),
            "Build or package the CLI as a self-contained artifact, or make the source-built workspace installable on the runner.".to_string(),
        ]),
    ))
}
