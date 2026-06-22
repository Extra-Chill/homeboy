use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::{Error, Result};

use super::lab_workspaces_deps::{
    accepted_extra_lab_workspaces, add_candidate_extra_workspace, bare_module_imports,
    bootstrap_source_cli_node_dependencies, canonical_existing_dir,
    component_contract_candidate_paths, containing_checkout_or_parent,
    discovered_validation_dependency_workspaces, provider_config_candidate_paths,
    provider_config_source_cli_files,
};
use super::{
    sync_workspace, RunnerGitDependencyMaterializationOutput, RunnerWorkspaceSyncMode,
    RunnerWorkspaceSyncOptions, RunnerWorkspaceSyncOutput,
};

pub(super) const LAB_EXTRA_WORKSPACES_ENV: &str = concat!("HOME", "BOY_LAB_EXTRA_WORKSPACES");
pub(super) const LAB_EXTRA_WORKSPACES_JSON_ENV: &str =
    concat!("HOME", "BOY_LAB_EXTRA_WORKSPACES_JSON");
pub(super) const LAB_WORKSPACE_MAPPING_SCHEMA: &str = concat!("home", "boy/workspace-map/v1");

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct LabWorkspaceMappingEntry {
    role: String,
    local_path: String,
    remote_path: String,
    sync_mode: String,
    snapshot_identity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dependency_freshness: Option<serde_json::Value>,
}

impl LabWorkspaceMappingEntry {
    pub(super) fn local_path(&self) -> &str {
        &self.local_path
    }

    pub(super) fn remote_path(&self) -> &str {
        &self.remote_path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExtraLabWorkspace {
    pub(super) role: String,
    pub(super) path: PathBuf,
    pub(super) snapshot_includes: Vec<String>,
    pub(super) bootstrap_node_dependencies: bool,
}

pub(super) fn sync_extra_lab_workspaces(
    runner_id: &str,
    primary_local_path: &str,
    extra_workspaces: Vec<ExtraLabWorkspace>,
    workspace_mapping: &mut Vec<LabWorkspaceMappingEntry>,
) -> Result<Vec<LabWorkspaceMappingEntry>> {
    let primary = canonical_existing_dir(primary_local_path, "path")?;
    let mut seen = HashSet::from([primary]);
    let mut synced_entries = Vec::new();

    for extra in extra_workspaces {
        let local_path = canonical_existing_dir(&extra.path.display().to_string(), "workspace")?;
        if !seen.insert(local_path.clone()) {
            continue;
        }
        let synced = sync_workspace(
            runner_id,
            RunnerWorkspaceSyncOptions {
                path: local_path.display().to_string(),
                mode: RunnerWorkspaceSyncMode::Snapshot,
                controller_routed_git: false,
                changed_since_base: None,
                git_fetch_refs: Vec::new(),
                snapshot_includes: extra.snapshot_includes.clone(),
                allow_dirty_lab_workspace: false,
                run_isolation_token: None,
            },
        )?
        .0;
        let entry = workspace_mapping_entry(&extra.role, &synced);
        if extra.bootstrap_node_dependencies {
            bootstrap_source_cli_node_dependencies(
                runner_id,
                &synced.local_path,
                &synced.remote_path,
            )?;
        }
        workspace_mapping.push(entry.clone());
        synced_entries.push(entry);
    }

    Ok(synced_entries)
}

pub(super) fn workspace_mapping_entry(
    role: impl Into<String>,
    synced: &RunnerWorkspaceSyncOutput,
) -> LabWorkspaceMappingEntry {
    LabWorkspaceMappingEntry {
        role: role.into(),
        local_path: synced.local_path.clone(),
        remote_path: synced.remote_path.clone(),
        sync_mode: synced.sync_mode.label().to_string(),
        snapshot_identity: synced.snapshot_identity.clone(),
        dependency_freshness: None,
    }
}

pub(super) fn workspace_mapping_entry_for_git_dependency(
    role: impl Into<String>,
    dependency: &RunnerGitDependencyMaterializationOutput,
) -> LabWorkspaceMappingEntry {
    LabWorkspaceMappingEntry {
        role: role.into(),
        local_path: dependency.local_path.clone(),
        remote_path: dependency.remote_path.clone(),
        sync_mode: dependency.sync_mode.label().to_string(),
        snapshot_identity: dependency.head.clone(),
        dependency_freshness: Some(serde_json::json!({
            "local_path": dependency.local_path.as_str(),
            "remote": dependency.remote_url.as_str(),
            "before_sha": dependency.before_sha.as_deref(),
            "after_sha": dependency.after_sha.as_deref(),
            "upstream_sha": dependency.upstream_sha.as_deref(),
            "upstream": dependency.upstream.as_deref(),
            "status": dependency.status.as_str(),
            "pinned_ref": dependency.pinned_ref.as_deref(),
            "used_pinned_ref": dependency.used_pinned_ref,
            "dirty_overlay": dependency.dirty_overlay,
            "source_provenance": if dependency.dirty_overlay {
                "dirty_snapshot"
            } else {
                "clean_git"
            },
        })),
    }
}

pub(super) fn workspace_mapping_entries_for_git_dependency(
    role: impl Into<String>,
    dependency: &RunnerGitDependencyMaterializationOutput,
) -> Vec<LabWorkspaceMappingEntry> {
    let role = role.into();
    let mut entries = vec![workspace_mapping_entry_for_git_dependency(
        role.clone(),
        dependency,
    )];
    if let Some(subpath) = dependency
        .required_subpath
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        entries.push(LabWorkspaceMappingEntry {
            role,
            local_path: Path::new(&dependency.local_path)
                .join(subpath)
                .display()
                .to_string(),
            remote_path: Path::new(&dependency.remote_path)
                .join(subpath)
                .display()
                .to_string(),
            sync_mode: dependency.sync_mode.label().to_string(),
            snapshot_identity: dependency.head.clone(),
            dependency_freshness: None,
        });
    }
    entries
}

pub(super) fn lab_workspace_mapping_metadata(
    workspace_mapping: &[LabWorkspaceMappingEntry],
) -> serde_json::Value {
    let local_to_remote = workspace_mapping
        .iter()
        .map(|entry| {
            (
                entry.local_path.clone(),
                serde_json::Value::String(entry.remote_path.clone()),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    serde_json::json!({
        "schema": LAB_WORKSPACE_MAPPING_SCHEMA,
        "workspaces": workspace_mapping,
        "local_to_remote": local_to_remote,
    })
}

pub(super) fn lab_extra_workspaces(source_path: &Path) -> Result<Vec<ExtraLabWorkspace>> {
    let mut workspaces = accepted_extra_lab_workspaces()?;
    workspaces.extend(discovered_validation_dependency_workspaces(source_path)?);
    Ok(workspaces)
}

/// Discover controller-local directories referenced by a `--provider-config` in
/// the offloaded args (runtime component paths, provider plugin paths, mount
/// sources, workspace root) so they are synced to the runner and remappable.
///
/// Directories under `source_path` are skipped because the primary workspace
/// sync already covers them. Existing files are mapped to their containing git
/// checkout when available, falling back to their parent directory.
pub(super) fn provider_config_extra_workspaces(
    args: &[String],
    source_path: &Path,
) -> Result<Vec<ExtraLabWorkspace>> {
    let Some(spec) = provider_config_spec(args) else {
        return Ok(Vec::new());
    };
    let raw = match crate::core::config::read_json_spec_to_string(&spec) {
        Ok(raw) => raw,
        Err(_) => return Ok(Vec::new()),
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => return Ok(Vec::new()),
    };

    let source_canon = source_path
        .canonicalize()
        .unwrap_or_else(|_| source_path.to_path_buf());

    let mut seen = BTreeSet::new();
    let mut workspaces: Vec<ExtraLabWorkspace> = Vec::new();
    for candidate in provider_config_candidate_paths(&value) {
        add_candidate_extra_workspace(
            &candidate,
            "provider_config",
            &source_canon,
            &mut seen,
            &mut workspaces,
        )?;
    }
    Ok(workspaces)
}

/// Discover a controller-local `agent-task run-plan --plan @file` checkout so
/// Lab offload can remap the plan path instead of asking the runner to read a
/// controller-only filesystem path.
pub(super) fn agent_task_plan_extra_workspaces(
    args: &[String],
    source_path: &Path,
) -> Result<Vec<ExtraLabWorkspace>> {
    let Some(spec) = agent_task_plan_spec(args) else {
        return Ok(Vec::new());
    };
    let source_canon = source_path
        .canonicalize()
        .unwrap_or_else(|_| source_path.to_path_buf());
    let mut seen = BTreeSet::new();
    let mut workspaces = Vec::new();

    if let Some(path) = agent_task_plan_file_path(&spec, source_path) {
        if path.is_file() {
            let workspace_path = containing_checkout_or_parent(&path)?;
            if let Ok(canon) = workspace_path.canonicalize() {
                if canon != source_canon && !canon.starts_with(&source_canon) {
                    seen.insert(canon.clone());
                    workspaces.push(ExtraLabWorkspace {
                        role: "agent_task_plan".to_string(),
                        path: canon,
                        snapshot_includes: Vec::new(),
                        bootstrap_node_dependencies: false,
                    });
                }
            }
        }
    }

    let raw = match read_agent_task_plan_spec_to_string(&spec, source_path) {
        Ok(raw) => raw,
        Err(_) => return Ok(workspaces),
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => return Ok(workspaces),
    };
    for candidate in component_contract_candidate_paths(&value) {
        add_candidate_extra_workspace(
            &candidate,
            "component_contract",
            &source_canon,
            &mut seen,
            &mut workspaces,
        )?;
    }
    for candidate in provider_config_candidate_paths(&value) {
        add_candidate_extra_workspace(
            &candidate,
            "agent_task_plan_config",
            &source_canon,
            &mut seen,
            &mut workspaces,
        )?;
    }

    Ok(workspaces)
}

pub(super) fn path_setting_extra_workspaces(
    args: &[String],
    source_path: &Path,
) -> Result<Vec<ExtraLabWorkspace>> {
    let source_canon = source_path
        .canonicalize()
        .unwrap_or_else(|_| source_path.to_path_buf());
    let mut seen = BTreeSet::new();
    let mut workspaces = Vec::new();

    for value in path_setting_values(args) {
        add_candidate_extra_workspace(
            &value,
            "path_setting",
            &source_canon,
            &mut seen,
            &mut workspaces,
        )?;
    }

    Ok(workspaces)
}

pub(super) fn rig_component_path_env_extra_workspaces(
    source_path: &Path,
) -> Result<Vec<ExtraLabWorkspace>> {
    rig_component_path_env_extra_workspaces_from_entries(source_path, std::env::vars())
}

fn rig_component_path_env_extra_workspaces_from_entries(
    source_path: &Path,
    entries: impl IntoIterator<Item = (String, String)>,
) -> Result<Vec<ExtraLabWorkspace>> {
    let source_canon = source_path
        .canonicalize()
        .unwrap_or_else(|_| source_path.to_path_buf());
    let mut seen = BTreeSet::new();
    let mut workspaces = Vec::new();

    for (name, value) in entries
        .into_iter()
        .filter(|(name, value)| is_rig_component_path_env_name(name) && !value.trim().is_empty())
    {
        let expanded = shellexpand::tilde(&value).to_string();
        let path = Path::new(&expanded);
        if !path.exists() {
            return Err(Error::validation_invalid_argument(
                name.clone(),
                format!(
                    "Lab offload cannot forward `{name}` because its controller-side path does not exist"
                ),
                Some(value.clone()),
                Some(vec![
                    format!("Controller-side value: {value}"),
                    "Set the variable to an existing checkout/component path before offload, unset it to use the rig default, or run with --force-hot to keep the check local.".to_string(),
                ]),
            ));
        }
        add_candidate_extra_workspace(
            &value,
            "rig_component_path_env",
            &source_canon,
            &mut seen,
            &mut workspaces,
        )?;
    }

    Ok(workspaces)
}

pub(super) fn is_rig_component_path_env_name(name: &str) -> bool {
    name.starts_with("HOMEBOY_") && name.ends_with("_COMPONENT_PATH")
}

pub(super) fn preflight_provider_config_source_cli_dependencies(
    args: &[String],
    snapshot_excludes: &[String],
) -> Result<()> {
    if !snapshot_excludes
        .iter()
        .any(|exclude| exclude == "node_modules" || exclude == "node_modules/**")
    {
        return Ok(());
    }

    let Some(spec) = provider_config_spec(args) else {
        return Ok(());
    };
    let raw = match crate::core::config::read_json_spec_to_string(&spec) {
        Ok(raw) => raw,
        Err(_) => return Ok(()),
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => return Ok(()),
    };

    for file in provider_config_source_cli_files(&value) {
        let content = match fs::read_to_string(&file) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let imports = bare_module_imports(&content);
        if let Some(package) = imports.iter().next() {
            return Err(Error::validation_invalid_argument(
                "provider_config",
                format!(
                    "Lab offload cannot preflight source-built CLI `{}` because it imports package `{}` while node_modules is excluded from the synced snapshot",
                    file.display(),
                    package
                ),
                Some(file.display().to_string()),
                Some(vec![
                    format!(
                        "Materialize `{}` on the runner before execution, bundle it into the CLI artifact, or adjust runner snapshot policy to include the dependency path.",
                        package
                    ),
                    "Use runner policy snapshot_includes for generated CLI outputs that must travel with the snapshot.".to_string(),
                ]),
            ));
        }
    }

    Ok(())
}

fn provider_config_spec(args: &[String]) -> Option<String> {
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--provider-config" {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix("--provider-config=") {
            return Some(value.to_string());
        }
    }
    None
}

fn agent_task_plan_spec(args: &[String]) -> Option<String> {
    let run_plan_index = subcommand_index(args, "agent-task").and_then(|index| {
        args.get(index + 1)
            .filter(|arg| arg.as_str() == "run-plan")
            .map(|_| index + 1)
    })?;

    let mut iter = args.iter().skip(run_plan_index + 1).peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--plan" {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix("--plan=") {
            return Some(value.to_string());
        }
    }
    None
}

fn agent_task_plan_file_path(spec: &str, source_path: &Path) -> Option<PathBuf> {
    let path = spec.strip_prefix('@')?;
    if path.trim().is_empty() || path.contains("://") {
        return None;
    }

    let expanded = PathBuf::from(shellexpand::tilde(path).to_string());
    if expanded.is_file() || expanded.is_absolute() {
        return Some(expanded);
    }

    let source_relative = source_path.join(&expanded);
    if source_relative.is_file() {
        return Some(source_relative);
    }

    Some(expanded)
}

fn read_agent_task_plan_spec_to_string(spec: &str, source_path: &Path) -> Result<String> {
    let Some(_) = spec.strip_prefix('@') else {
        return crate::core::config::read_json_spec_to_string(spec);
    };
    let Some(path) = agent_task_plan_file_path(spec, source_path) else {
        return crate::core::config::read_json_spec_to_string(spec);
    };
    std::fs::read_to_string(&path).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("read agent-task plan {}", path.display())),
        )
    })
}

fn path_setting_values(args: &[String]) -> Vec<String> {
    let mut values = Vec::new();
    let mut iter = args.iter().peekable();
    let mut passthrough = false;
    while let Some(arg) = iter.next() {
        if passthrough {
            continue;
        }
        if arg == "--" {
            passthrough = true;
            continue;
        }
        if arg == "--setting" || arg == "--setting-json" {
            if let Some(raw) = iter.next() {
                push_path_setting_value(raw, &mut values);
            }
            continue;
        }
        if let Some(raw) = arg
            .strip_prefix("--setting=")
            .or_else(|| arg.strip_prefix("--setting-json="))
        {
            push_path_setting_value(raw, &mut values);
        }
    }
    values
}

fn push_path_setting_value(raw: &str, values: &mut Vec<String>) {
    let Some((key, value)) = raw.split_once('=') else {
        return;
    };
    if !key.trim().is_empty() && !value.trim().is_empty() {
        values.push(value.to_string());
    }
}

fn subcommand_index(args: &[String], subcommand: &str) -> Option<usize> {
    args.iter().position(|arg| arg == subcommand)
}

#[cfg(test)]
mod provider_config_candidate_paths_tests {
    use std::path::Path;
    use std::process::Command;

    use super::{
        agent_task_plan_extra_workspaces, agent_task_plan_spec, path_setting_extra_workspaces,
        preflight_provider_config_source_cli_dependencies, provider_config_candidate_paths,
        provider_config_extra_workspaces, rig_component_path_env_extra_workspaces_from_entries,
        workspace_mapping_entries_for_git_dependency,
    };
    use crate::core::runner::{
        ByteFileCounts, RunnerGitDependencyMaterializationOutput, RunnerWorkspaceSyncMode,
    };

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn extracts_all_local_path_sources_including_runtime_overlays() {
        let value = serde_json::json!({
            "workspace_root": "/local/sample-plugin@cook",
            "mounts": [{ "source": "/local/sample-plugin@cook", "target": "/workspace/sample-plugin" }],
            "runtime_component_paths": {
                "agent_runtime": "/local/sample-plugin",
                "agent_runtime_tools": "/local/sample-plugin-code"
            },
            "provider_plugin_paths": ["/local/ai-provider-for-claude-code"],
            "runtime_overlays": [
                { "kind": "bundled-library", "library": "portable-ai-client", "source": "/local/portable-ai-client@custom-provider-auth", "target": "/runtime/includes/portable-ai-client" }
            ],
            "provider_support": "/local/provider-support",
            "source_cli": "/local/provider/packages/cli/dist/index.js",
            "model": "claude-opus-4-8"
        });

        let paths = provider_config_candidate_paths(&value);

        for expected in [
            "/local/sample-plugin@cook",
            "/local/sample-plugin",
            "/local/sample-plugin-code",
            "/local/ai-provider-for-claude-code",
            "/local/portable-ai-client@custom-provider-auth",
            "/local/provider-support",
            "/local/provider/packages/cli/dist/index.js",
        ] {
            assert!(
                paths.iter().any(|p| p == expected),
                "missing candidate path: {expected}"
            );
        }
        // Non-path scalars are not collected.
        assert!(!paths.iter().any(|p| p == "claude-opus-4-8"));
    }

    #[test]
    fn empty_config_yields_no_candidates() {
        let value = serde_json::json!({ "model": "x" });
        assert!(provider_config_candidate_paths(&value).is_empty());
    }

    #[test]
    fn agent_task_plan_spec_allows_global_flags_before_agent_task() {
        let args = vec![
            "homeboy".to_string(),
            "--force-hot".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--plan=@/tmp/plan.json".to_string(),
        ];

        assert_eq!(
            agent_task_plan_spec(&args),
            Some("@/tmp/plan.json".to_string())
        );
    }

    #[test]
    fn provider_config_file_path_syncs_containing_checkout() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let provider = controller.path().join("provider-cli");
        let cli = provider.join("packages/cli/dist/index.js");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(cli.parent().unwrap()).expect("cli dist dir");
        std::fs::write(&cli, "#!/usr/bin/env node\n").expect("cli file");
        std::fs::write(provider.join("package-lock.json"), "{}\n").expect("package lock");
        git(&provider, &["init", "-b", "main"]);
        git(&provider, &["config", "user.email", "test@example.com"]);
        git(&provider, &["config", "user.name", "Homeboy Test"]);
        git(&provider, &["add", "."]);
        git(&provider, &["commit", "-m", "initial"]);

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--provider-config".to_string(),
            serde_json::json!({ "source_cli": cli }).to_string(),
        ];

        let workspaces = provider_config_extra_workspaces(&args, &source).expect("workspaces");

        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].path, provider.canonicalize().unwrap());
        assert!(workspaces[0]
            .snapshot_includes
            .contains(&"packages/cli/dist/**".to_string()));
        assert!(workspaces[0].bootstrap_node_dependencies);
    }

    #[test]
    fn provider_config_file_path_merges_snapshot_includes_for_duplicate_checkout() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let provider = controller.path().join("provider-cli");
        let cli = provider.join("packages/cli/dist/index.js");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(cli.parent().unwrap()).expect("cli dist dir");
        std::fs::write(&cli, "#!/usr/bin/env node\n").expect("cli file");
        std::fs::write(provider.join("package-lock.json"), "{}\n").expect("package lock");
        git(&provider, &["init", "-b", "main"]);
        git(&provider, &["config", "user.email", "test@example.com"]);
        git(&provider, &["config", "user.name", "Homeboy Test"]);
        git(&provider, &["add", "."]);
        git(&provider, &["commit", "-m", "initial"]);

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--provider-config".to_string(),
            serde_json::json!({
                "provider_root": provider,
                "source_cli": cli,
            })
            .to_string(),
        ];

        let workspaces = provider_config_extra_workspaces(&args, &source).expect("workspaces");

        assert_eq!(workspaces.len(), 1);
        assert!(workspaces[0]
            .snapshot_includes
            .contains(&"packages/cli/dist/**".to_string()));
        assert!(workspaces[0].bootstrap_node_dependencies);
    }

    #[test]
    fn agent_task_run_plan_file_path_syncs_containing_checkout() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let planner = controller.path().join("plan-owner");
        let tool = controller.path().join("tool-runner");
        let tool_bin = tool.join("packages/cli/dist/index.js");
        let plan = planner.join(".ci/site-generation-loop.agent-task-plan.json");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(plan.parent().unwrap()).expect("plan dir");
        std::fs::create_dir_all(tool_bin.parent().unwrap()).expect("tool cli dir");
        std::fs::write(&tool_bin, "#!/usr/bin/env node\n").expect("tool bin");
        std::fs::write(tool.join("package-lock.json"), "{}\n").expect("package lock");
        std::fs::write(
            &plan,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "site-generation-loop",
                "tasks": [{
                    "task_id": "task-1",
                    "executor": {
                        "backend": "tool-runner",
                        "config": {
                            "tool_bin": tool_bin,
                            "artifact_root": planner.join("artifacts")
                        }
                    },
                    "instructions": "test"
                }]
            })
            .to_string(),
        )
        .expect("plan file");
        git(&planner, &["init", "-b", "main"]);
        git(&planner, &["config", "user.email", "test@example.com"]);
        git(&planner, &["config", "user.name", "Homeboy Test"]);
        git(&planner, &["add", "."]);
        git(&planner, &["commit", "-m", "initial"]);
        git(&tool, &["init", "-b", "main"]);
        git(&tool, &["config", "user.email", "test@example.com"]);
        git(&tool, &["config", "user.name", "Homeboy Test"]);
        git(&tool, &["add", "."]);
        git(&tool, &["commit", "-m", "initial"]);

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--plan".to_string(),
            format!("@{}", plan.display()),
        ];

        let workspaces = agent_task_plan_extra_workspaces(&args, &source).expect("workspaces");

        assert_eq!(workspaces.len(), 2);
        assert_eq!(workspaces[0].role, "agent_task_plan");
        assert_eq!(workspaces[0].path, planner.canonicalize().unwrap());
        assert!(workspaces[0].snapshot_includes.is_empty());
        assert!(!workspaces[0].bootstrap_node_dependencies);
        assert_eq!(workspaces[1].role, "agent_task_plan_config");
        assert_eq!(workspaces[1].path, tool.canonicalize().unwrap());
        assert!(workspaces[1]
            .snapshot_includes
            .contains(&"packages/cli/dist/**".to_string()));
        assert!(workspaces[1].bootstrap_node_dependencies);
    }

    #[test]
    fn agent_task_run_plan_component_contract_paths_get_component_contract_evidence_role() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let component = controller.path().join("domain-component");
        let plan = source.join(".ci/plan.json");
        std::fs::create_dir_all(plan.parent().unwrap()).expect("plan dir");
        std::fs::create_dir_all(&component).expect("component dir");
        std::fs::write(
            &plan,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "plan-with-components",
                "component_contracts": [{
                    "slug": "domain-component",
                    "path": component,
                    "loadAs": "plugin",
                    "activate": true
                }],
                "tasks": [{ "task_id": "task-1", "instructions": "test", "executor": { "backend": "test" } }]
            })
            .to_string(),
        )
        .expect("plan file");

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            format!("--plan=@{}", plan.display()),
        ];

        let workspaces = agent_task_plan_extra_workspaces(&args, &source).expect("workspaces");

        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].role, "component_contract");
        assert_eq!(workspaces[0].path, component.canonicalize().unwrap());
    }

    #[test]
    fn agent_task_run_plan_file_inside_primary_workspace_needs_no_extra_sync() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let plan = source.join(".ci/site-generation-loop.agent-task-plan.json");
        std::fs::create_dir_all(plan.parent().unwrap()).expect("plan dir");
        std::fs::write(
            &plan,
            "{\"schema\":\"homeboy/agent-task-plan/v1\",\"tasks\":[]}\n",
        )
        .expect("plan file");

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            format!("--plan=@{}", plan.display()),
        ];

        let workspaces = agent_task_plan_extra_workspaces(&args, &source).expect("workspaces");

        assert!(workspaces.is_empty());
    }

    #[test]
    fn agent_task_run_plan_relative_file_reads_from_primary_workspace() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let tool = controller.path().join("tool-runner");
        let tool_bin = tool.join("packages/cli/dist/index.js");
        let plan = source.join(".ci/site-generation-loop.agent-task-plan.json");
        std::fs::create_dir_all(plan.parent().unwrap()).expect("plan dir");
        std::fs::create_dir_all(tool_bin.parent().unwrap()).expect("tool cli dir");
        std::fs::write(&tool_bin, "#!/usr/bin/env node\n").expect("tool bin");
        std::fs::write(tool.join("package-lock.json"), "{}\n").expect("package lock");
        std::fs::write(
            &plan,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "site-generation-loop",
                "tasks": [{
                    "task_id": "task-1",
                    "executor": {
                        "backend": "tool-runner",
                        "config": { "tool_bin": tool_bin }
                    }
                }]
            })
            .to_string(),
        )
        .expect("plan file");
        git(&tool, &["init", "-b", "main"]);
        git(&tool, &["config", "user.email", "test@example.com"]);
        git(&tool, &["config", "user.name", "Homeboy Test"]);
        git(&tool, &["add", "."]);
        git(&tool, &["commit", "-m", "initial"]);

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--plan".to_string(),
            "@.ci/site-generation-loop.agent-task-plan.json".to_string(),
        ];

        let workspaces = agent_task_plan_extra_workspaces(&args, &source).expect("workspaces");

        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].role, "agent_task_plan_config");
        assert_eq!(workspaces[0].path, tool.canonicalize().unwrap());
    }

    #[test]
    fn path_setting_local_file_syncs_containing_checkout() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let tool = controller.path().join("tool-runner");
        let tool_bin = tool.join("packages/cli/dist/index.js");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(tool_bin.parent().unwrap()).expect("tool cli dir");
        std::fs::write(&tool_bin, "#!/usr/bin/env node\n").expect("tool bin");
        std::fs::write(tool.join("package-lock.json"), "{}\n").expect("package lock");
        git(&tool, &["init", "-b", "main"]);
        git(&tool, &["config", "user.email", "test@example.com"]);
        git(&tool, &["config", "user.name", "Homeboy Test"]);
        git(&tool, &["add", "."]);
        git(&tool, &["commit", "-m", "initial"]);

        let args = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "--setting".to_string(),
            format!("tool_bin={}", tool_bin.display()),
        ];

        let workspaces = path_setting_extra_workspaces(&args, &source).expect("workspaces");

        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].role, "path_setting");
        assert_eq!(workspaces[0].path, tool.canonicalize().unwrap());
        assert!(workspaces[0]
            .snapshot_includes
            .contains(&"packages/cli/dist/**".to_string()));
        assert!(workspaces[0].bootstrap_node_dependencies);
    }

    #[test]
    fn rig_component_path_env_extra_workspaces_syncs_existing_component_path() {
        crate::test_support::with_isolated_home(|home| {
            let source = home.path().join("primary");
            std::fs::create_dir_all(&source).expect("source path");
            let component_path = home.path().join("Developer/plugin/includes");
            std::fs::create_dir_all(&component_path).expect("component path");

            let workspaces = rig_component_path_env_extra_workspaces_from_entries(
                &source,
                [(
                    "HOMEBOY_TEST_COMPONENT_PATH".to_string(),
                    component_path.display().to_string(),
                )],
            )
            .expect("workspaces");

            assert_eq!(workspaces.len(), 1);
            assert_eq!(workspaces[0].role, "rig_component_path_env");
            assert_eq!(workspaces[0].path, component_path.canonicalize().unwrap());
        });
    }

    #[test]
    fn rig_component_path_env_extra_workspaces_rejects_missing_component_path() {
        crate::test_support::with_isolated_home(|home| {
            let missing = home.path().join("missing-plugin");

            let err = rig_component_path_env_extra_workspaces_from_entries(
                home.path(),
                [(
                    "HOMEBOY_MISSING_COMPONENT_PATH".to_string(),
                    missing.display().to_string(),
                )],
            )
            .expect_err("missing path");

            assert_eq!(err.details["field"], "HOMEBOY_MISSING_COMPONENT_PATH");
            assert!(err.message.contains("controller-side path does not exist"));
        });
    }

    #[test]
    #[cfg(unix)]
    fn agent_task_run_plan_syncs_symlinked_dependency_target_inside_primary_workspace() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let tool = controller.path().join("tool-runner");
        let tool_bin = tool.join("packages/cli/dist/index.js");
        let symlink = source.join(".ci/tool-runner");
        let plan = source.join(".ci/site-generation-loop.agent-task-plan.json");
        std::fs::create_dir_all(symlink.parent().unwrap()).expect("ci dir");
        std::fs::create_dir_all(tool_bin.parent().unwrap()).expect("tool cli dir");
        std::fs::write(&tool_bin, "#!/usr/bin/env node\n").expect("tool bin");
        std::fs::write(tool.join("package-lock.json"), "{}\n").expect("package lock");
        std::os::unix::fs::symlink(&tool, &symlink).expect("tool symlink");
        std::fs::write(
            &plan,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "site-generation-loop",
                "tasks": [{
                    "task_id": "task-1",
                    "executor": {
                        "backend": "tool-runner",
                        "config": {
                            "tool_bin": symlink.join("packages/cli/dist/index.js")
                        }
                    },
                    "instructions": "test"
                }]
            })
            .to_string(),
        )
        .expect("plan file");
        git(&tool, &["init", "-b", "main"]);
        git(&tool, &["config", "user.email", "test@example.com"]);
        git(&tool, &["config", "user.name", "Homeboy Test"]);
        git(&tool, &["add", "."]);
        git(&tool, &["commit", "-m", "initial"]);

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--plan".to_string(),
            format!("@{}", plan.display()),
        ];

        let workspaces = agent_task_plan_extra_workspaces(&args, &source).expect("workspaces");

        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].role, "agent_task_plan_config");
        assert_eq!(workspaces[0].path, tool.canonicalize().unwrap());
        assert!(workspaces[0]
            .snapshot_includes
            .contains(&"packages/cli/dist/**".to_string()));
        assert!(workspaces[0].bootstrap_node_dependencies);
    }

    #[test]
    fn rig_dependency_workspace_mapping_uses_dependency_sync_mode_and_subpath() {
        let dependency = RunnerGitDependencyMaterializationOutput {
            local_path: "/local/example-repo".to_string(),
            remote_path: "/remote/example-repo".to_string(),
            remote_url: "https://example.test/example/repo.git".to_string(),
            head: "snapshot:abc".to_string(),
            status: "snapshotted".to_string(),
            branch: Some("main".to_string()),
            before_sha: Some("abc".to_string()),
            after_sha: Some("abc".to_string()),
            upstream_sha: Some("abc".to_string()),
            upstream: Some("origin/main".to_string()),
            pinned_ref: None,
            required_subpath: Some("packages/component".to_string()),
            used_pinned_ref: false,
            dirty_overlay: false,
            sync_mode: RunnerWorkspaceSyncMode::Snapshot,
            counts: ByteFileCounts {
                files: 7,
                bytes: 42,
            },
        };

        let entries =
            workspace_mapping_entries_for_git_dependency("rig_component_dependency", &dependency);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].local_path, "/local/example-repo");
        assert_eq!(entries[0].remote_path, "/remote/example-repo");
        assert_eq!(entries[0].sync_mode, "snapshot");
        assert_eq!(entries[0].snapshot_identity, "snapshot:abc");
        assert_eq!(
            entries[0].dependency_freshness.as_ref().unwrap()["upstream"],
            "origin/main"
        );
        assert_eq!(
            entries[0].dependency_freshness.as_ref().unwrap()["after_sha"],
            "abc"
        );
        assert_eq!(
            entries[1].local_path,
            "/local/example-repo/packages/component"
        );
        assert_eq!(
            entries[1].remote_path,
            "/remote/example-repo/packages/component"
        );
        assert_eq!(entries[1].sync_mode, "snapshot");
        assert_eq!(entries[1].snapshot_identity, "snapshot:abc");
        assert!(entries[1].dependency_freshness.is_none());
    }

    #[test]
    fn source_cli_preflight_names_missing_workspace_package_and_importer() {
        let provider = tempfile::tempdir().expect("provider checkout");
        let cli = provider.path().join("packages/cli/dist/index.js");
        std::fs::create_dir_all(cli.parent().unwrap()).expect("cli dist dir");
        std::fs::write(
            &cli,
            "import { run } from '@example/provider-core';\nrun();\n",
        )
        .expect("cli file");
        git(provider.path(), &["init", "-b", "main"]);

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--provider-config".to_string(),
            serde_json::json!({ "source_cli": cli }).to_string(),
        ];
        let excludes = vec!["node_modules".to_string(), "node_modules/**".to_string()];

        let err = preflight_provider_config_source_cli_dependencies(&args, &excludes)
            .expect_err("workspace package import should fail preflight");

        assert_eq!(err.details["field"], "provider_config");
        assert!(err.message.contains("@example/provider-core"));
        assert!(err.message.contains("index.js"));
    }
}
