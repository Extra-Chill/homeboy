#![cfg(test)]

use std::path::{Path, PathBuf};
use std::process::Command;

use super::super::{
    agent_task_fanout_extra_workspaces, agent_task_plan_extra_workspaces, agent_task_plan_spec,
    declared_path_input_values, extension_source_extra_workspaces, path_setting_values,
    path_values_extra_workspaces, preflight_provider_config_source_cli_dependencies,
    provider_config_candidate_paths, provider_config_extra_workspaces,
    resolve_path_setting_workspace_refs_in_args,
    rig_component_path_env_extra_workspaces_from_entries, runtime_refresh_source_extra_workspaces,
    workspace_mapping_entries_for_git_dependency, workspace_ref_extra_workspaces,
    ExtraLabWorkspace,
};
use crate::runner::{
    ByteFileCounts, RunnerGitDependencyMaterializationOutput, RunnerWorkspaceSyncMode,
};
use crate::worktree;

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
fn runtime_refresh_source_syncs_local_source_and_records_dirty_identity() {
    let controller = tempfile::tempdir().expect("controller");
    let primary = controller.path().join("primary");
    let runtime_source = controller.path().join("homeboy-extensions");
    std::fs::create_dir_all(&primary).expect("primary dir");
    std::fs::create_dir_all(&runtime_source).expect("runtime source dir");
    std::fs::write(runtime_source.join("README.md"), "runtime\n").expect("runtime file");
    git(&runtime_source, &["init", "-b", "main"]);
    git(
        &runtime_source,
        &["config", "user.email", "test@example.com"],
    );
    git(&runtime_source, &["config", "user.name", "Homeboy Test"]);
    git(
        &runtime_source,
        &[
            "remote",
            "add",
            "origin",
            "https://example.test/runtime.git",
        ],
    );
    git(&runtime_source, &["add", "."]);
    git(&runtime_source, &["commit", "-m", "initial"]);
    std::fs::write(runtime_source.join("dirty.txt"), "dirty\n").expect("dirty file");
    let args = vec![
        "homeboy".to_string(),
        "runtime".to_string(),
        "refresh".to_string(),
        "opencode".to_string(),
        "--source".to_string(),
        runtime_source.display().to_string(),
    ];

    let workspaces = runtime_refresh_source_extra_workspaces(&args, &primary, true)
        .expect("runtime refresh source workspaces");

    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0].role, "runtime_refresh_source");
    assert_eq!(workspaces[0].path, runtime_source.canonicalize().unwrap());
    assert!(workspaces[0].allow_dirty_lab_workspace);
    let provenance = workspaces[0]
        .source_provenance
        .as_ref()
        .expect("source provenance");
    assert_eq!(provenance["schema"], "homeboy/runtime-refresh-source/v1");
    assert_eq!(provenance["git_branch"], "main");
    assert_eq!(provenance["git_remote"], "https://example.test/runtime.git");
    assert_eq!(provenance["dirty"], true);
    assert!(provenance["git_sha"].as_str().is_some());
}

#[test]
fn extension_refresh_source_syncs_local_source() {
    let controller = tempfile::tempdir().expect("controller");
    let primary = controller.path().join("primary");
    let extension_source = controller.path().join("homeboy-extensions/wordpress");
    std::fs::create_dir_all(&primary).expect("primary dir");
    std::fs::create_dir_all(&extension_source).expect("extension source dir");
    std::fs::write(extension_source.join("wordpress.json"), "{}\n").expect("extension manifest");
    let args = vec![
        "homeboy".to_string(),
        "extension".to_string(),
        "refresh".to_string(),
        extension_source.display().to_string(),
        "--id".to_string(),
        "wordpress".to_string(),
    ];

    let workspaces = extension_source_extra_workspaces(&args, &primary, true)
        .expect("extension source workspaces");

    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0].role, "extension_source");
    assert_eq!(workspaces[0].path, extension_source.canonicalize().unwrap());
    assert!(workspaces[0].allow_dirty_lab_workspace);
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
fn dispatch_provider_config_file_path_syncs_containing_checkout() {
    let controller = tempfile::tempdir().expect("controller");
    let source = controller.path().join("primary");
    let provider = controller.path().join("dispatch-provider");
    let contract = provider.join("contracts/component.json");
    std::fs::create_dir_all(&source).expect("source dir");
    std::fs::create_dir_all(contract.parent().unwrap()).expect("contract dir");
    std::fs::write(&contract, "{}\n").expect("contract file");
    git(&provider, &["init", "-b", "main"]);
    git(&provider, &["config", "user.email", "test@example.com"]);
    git(&provider, &["config", "user.name", "Homeboy Test"]);
    git(&provider, &["add", "."]);
    git(&provider, &["commit", "-m", "initial"]);

    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "controller".to_string(),
        "run-from-spec".to_string(),
        "loop.json".to_string(),
        "--dispatch-provider-config".to_string(),
        serde_json::json!({
            "provider_plugin_paths": [provider.join("provider-plugin")],
            "component_contracts": [{ "path": contract }],
        })
        .to_string(),
    ];

    let workspaces = provider_config_extra_workspaces(&args, &source).expect("workspaces");

    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0].role, "provider_config");
    assert_eq!(workspaces[0].path, provider.canonicalize().unwrap());
    assert!(workspaces[0]
        .snapshot_includes
        .contains(&"contracts".to_string()));
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
fn agent_task_fanout_extra_workspaces_syncs_child_cook_paths() {
    let controller = tempfile::tempdir().expect("controller");
    let source = controller.path().join("primary");
    let child = controller.path().join("homeboy@cook-one");
    let spec = source.join("fanout.json");
    std::fs::create_dir_all(&source).expect("source dir");
    std::fs::create_dir_all(&child).expect("child dir");
    std::fs::write(
        &spec,
        serde_json::json!({
            "schema": "homeboy/agent-task-batch-cook-fanout-plan/v1",
            "fanout_id": "fanout/test",
            "cooks": [{
                "cook_id": "one",
                "prompt": "fix it",
                "cwd": child,
                "to_worktree": "homeboy@fix-one",
                "head": "fix/one",
                "verify": ["cargo test -p homeboy"]
            }]
        })
        .to_string(),
    )
    .expect("fanout spec");

    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "fanout".to_string(),
        "run-plan".to_string(),
        "--input".to_string(),
        format!("@{}", spec.display()),
    ];

    let workspaces = agent_task_fanout_extra_workspaces(&args, &source).expect("workspaces");

    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0].role, "agent_task_fanout_cook_workspace");
    assert_eq!(workspaces[0].path, child.canonicalize().unwrap());
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

    let workspaces =
        path_values_extra_workspaces(path_setting_values(&args), &source, "path_setting")
            .expect("workspaces");

    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0].role, "path_setting");
    assert_eq!(workspaces[0].path, tool.canonicalize().unwrap());
    assert!(workspaces[0]
        .snapshot_includes
        .contains(&"packages/cli/dist/**".to_string()));
    assert!(workspaces[0].bootstrap_node_dependencies);
}

#[test]
fn path_setting_bench_env_directory_values_sync_extra_workspaces() {
    let controller = tempfile::tempdir().expect("controller");
    let source = controller.path().join("primary");
    let fixture_root = controller
        .path()
        .join("blocks-engine@matrix/fixtures/websites");
    let transformer_root = controller.path().join("blocks-engine@matrix");
    std::fs::create_dir_all(&source).expect("source dir");
    std::fs::create_dir_all(&fixture_root).expect("fixture root");
    std::fs::write(transformer_root.join("README.md"), "fixture owner\n").expect("repo marker");
    git(&transformer_root, &["init", "-b", "main"]);
    git(
        &transformer_root,
        &["config", "user.email", "test@example.com"],
    );
    git(&transformer_root, &["config", "user.name", "Homeboy Test"]);
    git(&transformer_root, &["add", "."]);
    git(&transformer_root, &["commit", "-m", "initial"]);

    let args = vec![
        "homeboy".to_string(),
        "bench".to_string(),
        "--rig".to_string(),
        "static-site-importer-fixture-matrix".to_string(),
        "--setting".to_string(),
        format!(
            "bench_env.SSI_FIXTURE_MATRIX_FIXTURE_ROOT={}",
            fixture_root.display()
        ),
        format!(
            "--setting=bench_env.SSI_FIXTURE_MATRIX_BLOCKS_ENGINE_PHP_TRANSFORMER_PATH={}",
            transformer_root.display()
        ),
    ];

    let workspaces =
        path_values_extra_workspaces(path_setting_values(&args), &source, "path_setting")
            .expect("workspaces");

    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0].role, "path_setting");
    assert_eq!(workspaces[0].path, transformer_root.canonicalize().unwrap());
}

#[test]
fn declared_path_inputs_preserve_early_late_alias_and_json_subpaths() {
    let inputs = vec![
        "bench_env.fixtures.roots".to_string(),
        "--fixture-root".to_string(),
    ];
    let args = |alias: &str| {
        vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--setting-json".to_string(),
            format!(r#"bench_env={{"fixtures":{{"roots":["{alias}/one","{alias}/two"]}}}}"#),
            "--".to_string(),
            "--fixture-root".to_string(),
            format!("{alias}/passthrough"),
        ]
    };

    let early = declared_path_input_values(&args("/controller/workspace"), &inputs);
    let late = declared_path_input_values(&args("/runner/workspace"), &inputs);

    assert_eq!(early.len(), 3);
    assert_eq!(
        late,
        early
            .iter()
            .map(|value| value.replace("/controller/workspace", "/runner/workspace"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn path_setting_workspace_ref_resolves_to_controller_path_and_syncs_workspace() {
    crate::test_support::with_isolated_home(|home| {
        let store = crate::paths::homeboy_data()
            .expect("homeboy data")
            .join("task-worktrees");
        std::fs::create_dir_all(&store).expect("worktree store");
        let source = home.path().join("primary");
        let worktree = home.path().join("repo@cook");
        let nested = worktree.join("fixtures/input.json");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(nested.parent().unwrap()).expect("nested dir");
        std::fs::write(&nested, "{}\n").expect("nested file");
        std::fs::write(
            store.join("repo_cook.json"),
            serde_json::json!({
                "id": "repo@cook",
                "component_id": "repo",
                "source_checkout": home.path().join("repo").display().to_string(),
                "worktree_path": worktree.display().to_string(),
                "branch": "cook",
                "base_ref": "HEAD",
                "cleanup_policy": "preserve_on_failure",
                "created_at": "2026-01-01T00:00:00Z",
                "state": "active"
            })
            .to_string(),
        )
        .expect("worktree record");
        let args = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "--setting".to_string(),
            "fixture=@workspace:repo@cook/fixtures/input.json".to_string(),
        ];

        let (rewritten, resolutions) =
            resolve_path_setting_workspace_refs_in_args(&args).expect("resolve refs");
        let expected = format!("fixture={}", nested.display());

        assert_eq!(rewritten[3], expected);
        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].handle, "repo@cook");
        assert_eq!(
            resolutions[0].subpath.as_deref(),
            Some("fixtures/input.json")
        );

        let workspaces =
            workspace_ref_extra_workspaces(&resolutions, &source).expect("extra workspaces");
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].role, "path_setting_workspace_ref");
        assert_eq!(workspaces[0].path, worktree.canonicalize().unwrap());
        assert_eq!(
            workspaces[0].source_provenance.as_ref().unwrap()["ref"],
            "@workspace:repo@cook/fixtures/input.json"
        );
    });
}

#[test]
fn path_setting_workspace_ref_resolves_inside_setting_json() {
    crate::test_support::with_isolated_home(|home| {
        let store = crate::paths::homeboy_data()
            .expect("homeboy data")
            .join("task-worktrees");
        std::fs::create_dir_all(&store).expect("worktree store");
        let worktree = home.path().join("repo@cook");
        let nested = worktree.join("data/corpus");
        std::fs::create_dir_all(&nested).expect("nested dir");
        std::fs::write(
            store.join("repo_cook.json"),
            serde_json::json!({
                "id": "repo@cook",
                "component_id": "repo",
                "source_checkout": home.path().join("repo").display().to_string(),
                "worktree_path": worktree.display().to_string(),
                "branch": "cook",
                "base_ref": "HEAD",
                "cleanup_policy": "preserve_on_failure",
                "created_at": "2026-01-01T00:00:00Z",
                "state": "active"
            })
            .to_string(),
        )
        .expect("worktree record");
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--setting-json".to_string(),
            r#"paths={"corpus":"@workspace:repo@cook/data/corpus","label":"kept"}"#.to_string(),
        ];

        let (rewritten, resolutions) =
            resolve_path_setting_workspace_refs_in_args(&args).expect("resolve refs");
        let raw = rewritten[3].strip_prefix("paths=").expect("paths setting");
        let json: serde_json::Value = serde_json::from_str(raw).expect("setting json");

        assert_eq!(json["corpus"], nested.display().to_string());
        assert_eq!(json["label"], "kept");
        assert_eq!(resolutions.len(), 1);
        assert!(path_setting_values(&rewritten)
            .iter()
            .any(|value| value == &nested.display().to_string()));
    });
}

#[test]
fn path_setting_workspace_ref_resolves_adopted_workspace() {
    crate::test_support::with_isolated_home(|home| {
        let source = home.path().join("primary");
        let workspace = home.path().join("external-workspace");
        let nested = workspace.join("fixtures/input.json");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(nested.parent().unwrap()).expect("nested dir");
        std::fs::write(&nested, "{}\n").expect("nested file");
        worktree::adopt(worktree::WorktreeAdoptOptions {
            handle: "external".to_string(),
            path: workspace.display().to_string(),
            kind: Some("local_checkout".to_string()),
            provenance: Some(serde_json::json!({
                "source": "test-harness",
                "note": "opaque caller metadata"
            })),
        })
        .expect("adopt workspace");
        let args = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "--setting".to_string(),
            "fixture=@workspace:external/fixtures/input.json".to_string(),
        ];

        let (rewritten, resolutions) =
            resolve_path_setting_workspace_refs_in_args(&args).expect("resolve refs");
        let expected = format!(
            "fixture={}",
            workspace
                .canonicalize()
                .unwrap()
                .join("fixtures/input.json")
                .display()
        );

        assert_eq!(rewritten[3], expected);
        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].handle, "external");
        assert_eq!(resolutions[0].source_kind, "adopted_workspace");
        assert_eq!(
            resolutions[0].source_provenance.as_ref().unwrap()["source"],
            "test-harness"
        );
        assert_eq!(
            resolutions[0].subpath.as_deref(),
            Some("fixtures/input.json")
        );

        let workspaces =
            workspace_ref_extra_workspaces(&resolutions, &source).expect("extra workspaces");
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].path, workspace.canonicalize().unwrap());
        let provenance = workspaces[0].source_provenance.as_ref().unwrap();
        assert_eq!(provenance["workspace_source"], "adopted_workspace");
        assert_eq!(
            provenance["workspace_provenance"]["note"],
            "opaque caller metadata"
        );
    });
}

#[test]
fn path_setting_workspace_ref_missing_adopted_path_fails_locally() {
    crate::test_support::with_isolated_home(|home| {
        let workspace = home.path().join("external-workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        worktree::adopt(worktree::WorktreeAdoptOptions {
            handle: "external".to_string(),
            path: workspace.display().to_string(),
            kind: None,
            provenance: None,
        })
        .expect("adopt workspace");
        std::fs::remove_dir_all(&workspace).expect("remove adopted workspace");
        let args = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "--setting=fixture=@workspace:external".to_string(),
        ];

        let err = resolve_path_setting_workspace_refs_in_args(&args)
            .expect_err("missing adopted workspace path should fail");

        assert_eq!(err.details["field"], "workspace_ref");
        assert!(err
            .message
            .contains("resolved to a missing controller path"));
    });
}

#[test]
fn path_setting_workspace_ref_missing_handle_fails_locally() {
    crate::test_support::with_isolated_home(|_| {
        let args = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "--setting=fixture=@workspace:missing@cook/file.json".to_string(),
        ];

        let err = resolve_path_setting_workspace_refs_in_args(&args)
            .expect_err("missing workspace ref should fail");

        assert_eq!(err.details["field"], "workspace_ref");
        assert!(err
            .message
            .contains("does not match a known workspace handle"));
    });
}

#[test]
fn path_setting_workspace_ref_removed_record_fails_as_stale() {
    crate::test_support::with_isolated_home(|home| {
        let store = crate::paths::homeboy_data()
            .expect("homeboy data")
            .join("task-worktrees");
        std::fs::create_dir_all(&store).expect("worktree store");
        let worktree = home.path().join("repo@old");
        std::fs::create_dir_all(&worktree).expect("worktree dir");
        std::fs::write(
            store.join("repo_old.json"),
            serde_json::json!({
                "id": "repo@old",
                "component_id": "repo",
                "source_checkout": home.path().join("repo").display().to_string(),
                "worktree_path": worktree.display().to_string(),
                "branch": "old",
                "base_ref": "HEAD",
                "cleanup_policy": "preserve_on_failure",
                "created_at": "2026-01-01T00:00:00Z",
                "state": "removed"
            })
            .to_string(),
        )
        .expect("worktree record");
        let args = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "--setting".to_string(),
            "fixture=@workspace:repo@old".to_string(),
        ];

        let err = resolve_path_setting_workspace_refs_in_args(&args)
            .expect_err("removed workspace ref should fail");

        assert_eq!(err.details["field"], "workspace_ref");
        assert!(err.message.contains("stale task_worktree"));
    });
}

#[test]
fn workspace_ref_provenance_is_recorded_on_mapping_entry() {
    let workspace = ExtraLabWorkspace {
        role: "path_setting_workspace_ref".to_string(),
        path: PathBuf::from("/local/repo@cook"),
        snapshot_includes: Vec::new(),
        bootstrap_node_dependencies: false,
        bootstrap_command: None,
        allow_dirty_lab_workspace: false,
        source_provenance: Some(serde_json::json!({
            "source_provenance": "workspace_ref",
            "ref": "@workspace:repo@cook/file.json"
        })),
    };

    assert_eq!(
        workspace.source_provenance.as_ref().unwrap()["source_provenance"],
        "workspace_ref"
    );
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
                "HOMEBOY_RIG_COMPONENT_PATH__TEST_RIG__PLUGIN".to_string(),
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
                "HOMEBOY_RIG_COMPONENT_PATH__TEST_RIG__PLUGIN".to_string(),
                missing.display().to_string(),
            )],
        )
        .expect_err("missing path");

        assert_eq!(
            err.details["field"],
            "HOMEBOY_RIG_COMPONENT_PATH__TEST_RIG__PLUGIN"
        );
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
        dependency_cache: None,
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
