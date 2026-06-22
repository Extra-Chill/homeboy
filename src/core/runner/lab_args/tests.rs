//! Tests for Lab offload argument rewriting.

use super::*;
use crate::core::defaults;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[test]
fn lab_source_path_uses_agent_task_dispatch_cwd() {
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "dispatch".to_string(),
        "--cwd".to_string(),
        "/Users/user/Developer/wp-site-generator".to_string(),
        "--prompt".to_string(),
        "cook".to_string(),
    ];

    assert_eq!(
        lab_offload_source_path(&args).expect("source path"),
        PathBuf::from("/Users/user/Developer/wp-site-generator")
    );
}

#[test]
fn runner_resident_rewrite_preserves_runner_side_cwd() {
    let args = vec![
        "homeboy".to_string(),
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "tunnel".to_string(),
        "service".to_string(),
        "start".to_string(),
        "preview".to_string(),
        "--cwd".to_string(),
        "/home/user/Developer/_lab_workspaces/site".to_string(),
        "--command".to_string(),
        "npm run dev".to_string(),
    ];

    assert_eq!(
        rewrite_runner_resident_lab_offload_args(&args),
        vec![
            "homeboy".to_string(),
            "--force-hot".to_string(),
            "tunnel".to_string(),
            "service".to_string(),
            "start".to_string(),
            "preview".to_string(),
            "--cwd".to_string(),
            "/home/user/Developer/_lab_workspaces/site".to_string(),
            "--command".to_string(),
            "npm run dev".to_string(),
        ]
    );
}

#[test]
fn runner_resident_rewrite_expose_drops_server_and_marks_runner_local() {
    // `tunnel service expose --runner homeboy-lab --server homeboy-lab`
    // should not carry a server declaration to the runner: the runner is
    // the server in that context, so the rewrite drops --server and marks
    // the runner-side declaration runner-local (#4606/#4607).
    let args = vec![
        "homeboy".to_string(),
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "tunnel".to_string(),
        "service".to_string(),
        "expose".to_string(),
        "preview".to_string(),
        "--server".to_string(),
        "homeboy-lab".to_string(),
        "--remote-host".to_string(),
        "127.0.0.1".to_string(),
        "--remote-port".to_string(),
        "7331".to_string(),
        "--auth-mode".to_string(),
        "ssh-only".to_string(),
    ];

    let rewritten = rewrite_runner_resident_lab_offload_args(&args);
    assert_eq!(
        rewritten,
        vec![
            "homeboy".to_string(),
            "--force-hot".to_string(),
            "tunnel".to_string(),
            "service".to_string(),
            "expose".to_string(),
            "preview".to_string(),
            "--remote-host".to_string(),
            "127.0.0.1".to_string(),
            "--remote-port".to_string(),
            "7331".to_string(),
            "--auth-mode".to_string(),
            "ssh-only".to_string(),
            "--runner-local".to_string(),
        ]
    );
    assert!(!rewritten.iter().any(|arg| arg == "--server"));
    assert!(rewritten.iter().any(|arg| arg == "--runner-local"));
}

#[test]
fn runner_resident_rewrite_expose_handles_inline_server_value() {
    let args = vec![
        "homeboy".to_string(),
        "--runner=homeboy-lab".to_string(),
        "tunnel".to_string(),
        "service".to_string(),
        "expose".to_string(),
        "preview".to_string(),
        "--server=homeboy-lab".to_string(),
        "--remote-host".to_string(),
        "127.0.0.1".to_string(),
    ];

    let rewritten = rewrite_runner_resident_lab_offload_args(&args);
    assert!(!rewritten.iter().any(|arg| arg.starts_with("--server")));
    assert!(rewritten.iter().any(|arg| arg == "--runner-local"));
}

#[test]
fn lab_source_path_uses_agent_task_loop_to_worktree() {
    crate::test_support::with_isolated_home(|home| {
        let store = crate::core::paths::homeboy_data()
            .expect("homeboy data")
            .join("task-worktrees");
        std::fs::create_dir_all(&store).expect("worktree store");
        let worktree_path = home.path().join("homeboy@smoke");
        std::fs::create_dir_all(&worktree_path).expect("worktree path");
        std::fs::write(
            store.join("homeboy_smoke.json"),
            serde_json::json!({
                "id": "homeboy@smoke",
                "component_id": "homeboy",
                "source_checkout": home.path().join("homeboy").display().to_string(),
                "worktree_path": worktree_path.display().to_string(),
                "branch": "smoke",
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
            "agent-task".to_string(),
            "loop".to_string(),
            "--to-worktree".to_string(),
            "homeboy@smoke".to_string(),
            "--verify".to_string(),
            "true".to_string(),
            "--prompt".to_string(),
            "cook".to_string(),
        ];

        assert_eq!(
            lab_offload_source_path(&args).expect("source path"),
            worktree_path
        );
    });
}

#[test]
fn remap_inlines_and_rewrites_provider_config_local_paths() {
    let mappings = vec![
        LabPathRemap {
            local: "/Users/user/Developer/sample-plugin@cook".to_string(),
            remote: "/home/user/_lab_workspaces/sample-plugin@cook-abc".to_string(),
        },
        LabPathRemap {
            local: "/Users/user/Developer/sample-plugin-code".to_string(),
            remote: "/home/user/_lab_workspaces/sample-plugin-code-def".to_string(),
        },
    ];
    let config = serde_json::json!({
        "workspace_root": "/Users/user/Developer/sample-plugin@cook",
        "mounts": [{ "source": "/Users/user/Developer/sample-plugin@cook", "target": "/workspace/sample-plugin" }],
        "runtime_component_paths": { "agent_runtime_tools": "/Users/user/Developer/sample-plugin-code" },
        "provider_plugin_paths": ["/Users/user/Developer/sample-plugin@cook/vendor/provider"],
        "model": "claude-opus-4-8"
    })
    .to_string();
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--provider-config".to_string(),
        config,
        "--prompt".to_string(),
        "fix it".to_string(),
    ];

    let out = remap_provider_config_in_args(&args, &mappings);
    let cfg_idx = out.iter().position(|a| a == "--provider-config").unwrap() + 1;
    let remapped: serde_json::Value = serde_json::from_str(&out[cfg_idx]).expect("inline json");

    assert_eq!(
        remapped["workspace_root"],
        "/home/user/_lab_workspaces/sample-plugin@cook-abc"
    );
    assert_eq!(
        remapped["mounts"][0]["source"],
        "/home/user/_lab_workspaces/sample-plugin@cook-abc"
    );
    assert_eq!(remapped["mounts"][0]["target"], "/workspace/sample-plugin");
    assert_eq!(
        remapped["runtime_component_paths"]["agent_runtime_tools"],
        "/home/user/_lab_workspaces/sample-plugin-code-def"
    );
    assert_eq!(
        remapped["provider_plugin_paths"][0],
        "/home/user/_lab_workspaces/sample-plugin@cook-abc/vendor/provider"
    );
    assert_eq!(remapped["model"], "claude-opus-4-8");
    // unrelated args preserved
    assert!(out.iter().any(|a| a == "--prompt"));
    assert!(out.iter().any(|a| a == "fix it"));
}

#[test]
fn remap_prunes_stale_unresolved_provider_plugin_path() {
    // #4829: a `provider_plugin_paths` entry inherited from stale/global
    // settings points at a controller-local absolute directory that is not part
    // of this run's synced workspace, so no local->remote mapping is recorded
    // for it. Forwarding it verbatim makes the provider runtime declare an
    // extra plugin/workspace entry for a directory that does not exist on the runner,
    // failing recipe validation. Such entries must be pruned before offload.
    let mappings = vec![LabPathRemap {
        local: "/Users/user/Developer/sample-plugin@cook".to_string(),
        remote: "/home/user/_lab_workspaces/sample-plugin@cook-abc".to_string(),
    }];
    let config = serde_json::json!({
        "provider": "example-oauth",
        "provider_plugin_paths": [
            "/Users/user/Developer/sample-plugin@cook/vendor/provider",
            "/Users/user/Developer/example-oauth-provider"
        ]
    })
    .to_string();
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--provider-config".to_string(),
        config,
    ];

    let out = remap_provider_config_in_args(&args, &mappings);
    let cfg_idx = out.iter().position(|a| a == "--provider-config").unwrap() + 1;
    let remapped: serde_json::Value = serde_json::from_str(&out[cfg_idx]).expect("inline json");

    // The synced workspace path is remapped and kept; the stale path is dropped.
    let paths = remapped["provider_plugin_paths"]
        .as_array()
        .expect("plugin paths array");
    assert_eq!(
        paths.len(),
        1,
        "stale plugin path should be pruned: {paths:?}"
    );
    assert_eq!(
        paths[0],
        "/home/user/_lab_workspaces/sample-plugin@cook-abc/vendor/provider"
    );
}

#[test]
fn remap_prunes_all_provider_plugin_paths_when_no_mappings() {
    // With no synced workspace mappings, every absolute provider plugin path is
    // unresolvable on the runner and must be pruned so recipe validation never
    // sees a missing extra-plugin path. The array stays present but empty.
    let config = serde_json::json!({
        "provider": "example-oauth",
        "provider_plugin_paths": [
            "/home/chubes/Developer/example-oauth-provider"
        ]
    })
    .to_string();
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--provider-config".to_string(),
        config,
    ];

    let out = remap_provider_config_in_args(&args, &[]);
    let cfg_idx = out.iter().position(|a| a == "--provider-config").unwrap() + 1;
    let remapped: serde_json::Value = serde_json::from_str(&out[cfg_idx]).expect("inline json");

    assert_eq!(
        remapped["provider_plugin_paths"]
            .as_array()
            .expect("plugin paths array")
            .len(),
        0
    );
    // Unrelated config is preserved.
    assert_eq!(remapped["provider"], "example-oauth");
}

#[test]
fn remap_preserves_relative_and_materialized_provider_plugin_paths() {
    // Relative paths resolve against the runner workspace, and non-string
    // entries (materialized ref objects) are left untouched. Neither should be
    // pruned even when there are no path mappings.
    let config = serde_json::json!({
        "provider_plugin_paths": [
            "vendor/runner-relative-provider",
            { "materialized_path": "/runner/checkout/provider", "ref": "main" }
        ]
    })
    .to_string();
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--provider-config".to_string(),
        config,
    ];

    let out = remap_provider_config_in_args(&args, &[]);
    let cfg_idx = out.iter().position(|a| a == "--provider-config").unwrap() + 1;
    let remapped: serde_json::Value = serde_json::from_str(&out[cfg_idx]).expect("inline json");

    let paths = remapped["provider_plugin_paths"]
        .as_array()
        .expect("plugin paths array");
    assert_eq!(paths.len(), 2, "no entries should be pruned: {paths:?}");
    assert_eq!(paths[0], "vendor/runner-relative-provider");
    assert_eq!(paths[1]["materialized_path"], "/runner/checkout/provider");
}

#[test]
fn injects_default_provider_config_for_agent_task_cook() {
    crate::test_support::with_isolated_home(|_| {
        defaults::save_config(&defaults::HomeboyConfig {
            settings: HashMap::from([
                ("provider".to_string(), serde_json::json!("example-oauth")),
                (
                    "provider_plugin_paths".to_string(),
                    serde_json::json!(["/Users/user/Developer/example-provider@oauth"]),
                ),
            ]),
            ..defaults::HomeboyConfig::default()
        })
        .expect("save config");

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "fix it".to_string(),
        ];

        let out = inject_agent_task_default_provider_config_in_args(&args).expect("inject");
        let cfg_idx = out
            .iter()
            .position(|arg| arg == "--provider-config")
            .unwrap()
            + 1;
        let config: serde_json::Value = serde_json::from_str(&out[cfg_idx]).expect("config");

        assert_eq!(config["provider"], "example-oauth");
        assert_eq!(
            config["provider_plugin_paths"][0],
            "/Users/user/Developer/example-provider@oauth"
        );
        assert!(out.iter().any(|arg| arg == "--prompt"));
    });
}

#[test]
fn injected_default_provider_config_is_remappable() {
    crate::test_support::with_isolated_home(|_| {
        defaults::save_config(&defaults::HomeboyConfig {
            settings: HashMap::from([(
                "provider_plugin_paths".to_string(),
                serde_json::json!(["/Users/user/Developer/example-provider@oauth"]),
            )]),
            ..defaults::HomeboyConfig::default()
        })
        .expect("save config");

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--prompt".to_string(),
            "fix it".to_string(),
        ];
        let injected = inject_agent_task_default_provider_config_in_args(&args).expect("inject");
        let remapped = remap_provider_config_in_args(
            &injected,
            &[LabPathRemap {
                local: "/Users/user/Developer/example-provider@oauth".to_string(),
                remote: "/home/user/Developer/_lab_workspaces/example-provider@oauth".to_string(),
            }],
        );
        let cfg_idx = remapped
            .iter()
            .position(|arg| arg == "--provider-config")
            .unwrap()
            + 1;
        let config: serde_json::Value = serde_json::from_str(&remapped[cfg_idx]).expect("config");

        assert_eq!(
            config["provider_plugin_paths"][0],
            "/home/user/Developer/_lab_workspaces/example-provider@oauth"
        );
    });
}

#[test]
fn explicit_provider_config_prevents_default_injection() {
    crate::test_support::with_isolated_home(|_| {
        defaults::save_config(&defaults::HomeboyConfig {
            settings: HashMap::from([(
                "provider_plugin_paths".to_string(),
                serde_json::json!(["/Users/user/Developer/example-provider@oauth"]),
            )]),
            ..defaults::HomeboyConfig::default()
        })
        .expect("save config");

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--provider-config".to_string(),
            "{\"provider\":\"explicit\"}".to_string(),
        ];

        let out = inject_agent_task_default_provider_config_in_args(&args).expect("inject");
        assert_eq!(out, args);
    });
}

#[test]
fn remap_handles_provider_config_equals_form_and_no_mappings() {
    let mappings = vec![LabPathRemap {
        local: "/local/repo".to_string(),
        remote: "/remote/repo".to_string(),
    }];
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--provider-config={\"workspace_root\":\"/local/repo\"}".to_string(),
    ];
    let out = remap_provider_config_in_args(&args, &mappings);
    let val = out
        .iter()
        .find(|a| a.starts_with("--provider-config="))
        .unwrap();
    assert!(val.contains("/remote/repo"));
    assert!(!val.contains("/local/repo"));

    // No mappings -> inline JSON spec untouched (nothing to remap, no @file).
    let unchanged = remap_provider_config_in_args(&args, &[]);
    assert_eq!(unchanged, args);
}

#[test]
fn remap_inlines_provider_config_at_file_without_mappings() {
    // Regression for #3770: a `--provider-config @file` spec must be inlined
    // to JSON before Lab offload even when there are no path mappings, so the
    // remote runner never tries to read a controller-local path and fail with
    // "failed to read agent-task dispatch provider-config input: IO error".
    let temp = tempfile::tempdir().expect("tempdir");
    let cfg = temp.path().join("cfg.json");
    std::fs::write(
        &cfg,
        r#"{"model":"claude-opus-4-8","backend":"sample-runtime"}"#,
    )
    .expect("write provider config");

    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--provider-config".to_string(),
        format!("@{}", cfg.display()),
        "--prompt".to_string(),
        "fix it".to_string(),
    ];

    // No mappings on purpose: inlining must still happen.
    let out = remap_provider_config_in_args(&args, &[]);
    let cfg_idx = out.iter().position(|a| a == "--provider-config").unwrap() + 1;
    let spec = &out[cfg_idx];

    // The @file reference must be gone; the value must be inline JSON.
    assert!(
        !spec.starts_with('@'),
        "provider-config @file should be inlined, got: {spec}"
    );
    let inlined: serde_json::Value = serde_json::from_str(spec).expect("inline json");
    assert_eq!(inlined["model"], "claude-opus-4-8");
    assert_eq!(inlined["backend"], "sample-runtime");
    // Unrelated args preserved.
    assert!(out.iter().any(|a| a == "--prompt"));
    assert!(out.iter().any(|a| a == "fix it"));
}

#[test]
fn remap_inlines_and_rewrites_provider_config_at_file_with_mappings() {
    // #3770: a `--provider-config @file` is inlined AND its controller-local
    // paths are remapped to the synced remote workspace in one pass.
    let temp = tempfile::tempdir().expect("tempdir");
    let cfg = temp.path().join("cfg.json");
    std::fs::write(
        &cfg,
        r#"{"workspace_root":"/local/repo","model":"claude-opus-4-8"}"#,
    )
    .expect("write provider config");

    let mappings = vec![LabPathRemap {
        local: "/local/repo".to_string(),
        remote: "/remote/repo".to_string(),
    }];
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        format!("--provider-config=@{}", cfg.display()),
    ];

    let out = remap_provider_config_in_args(&args, &mappings);
    let val = out
        .iter()
        .find(|a| a.starts_with("--provider-config="))
        .unwrap();
    let spec = val.strip_prefix("--provider-config=").unwrap();
    assert!(
        !spec.starts_with('@'),
        "provider-config @file should be inlined, got: {spec}"
    );
    let inlined: serde_json::Value = serde_json::from_str(spec).expect("inline json");
    assert_eq!(inlined["workspace_root"], "/remote/repo");
    assert_eq!(inlined["model"], "claude-opus-4-8");
}

#[test]
fn remap_provider_config_inline_json_without_mappings_is_untouched() {
    // Plain inline JSON (no @file, no mappings) must pass through verbatim so
    // behavior is never worse than passing the original argument through.
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--provider-config".to_string(),
        r#"{"model":"claude-opus-4-8"}"#.to_string(),
    ];
    let out = remap_provider_config_in_args(&args, &[]);
    assert_eq!(out, args);
}

#[test]
fn remap_agent_task_run_plan_inlines_remapped_plan_json() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plan = temp.path().join("plan.json");
    std::fs::write(
        &plan,
        serde_json::json!({
            "schema": "homeboy/agent-task-plan/v1",
            "plan_id": "plan-1",
            "tasks": [{
                "task_id": "task-1",
                "executor": {
                    "backend": "tool-runner",
                    "config": {
                        "tool_bin": "/Users/user/Developer/example-project/.ci/tool-runner/packages/cli/dist/index.js",
                        "artifact_root": "/Users/user/Developer/example-project/artifacts"
                    }
                },
                "instructions": "test"
            }]
        })
        .to_string(),
    )
    .expect("write plan");
    let mappings = vec![LabPathRemap {
        local: "/Users/user/Developer/example-project".to_string(),
        remote: "/home/user/Developer/example-project".to_string(),
    }];
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "run-plan".to_string(),
        "--plan".to_string(),
        format!("@{}", plan.display()),
        "--record-run-id=loop-1".to_string(),
    ];

    let out = remap_agent_task_plan_in_args(&args, &mappings, temp.path()).expect("remap plan");
    let plan_idx = out.iter().position(|a| a == "--plan").unwrap() + 1;
    let remapped: serde_json::Value = serde_json::from_str(&out[plan_idx]).expect("inline plan");

    assert_eq!(
        remapped["tasks"][0]["executor"]["config"]["tool_bin"],
        "/home/user/Developer/example-project/.ci/tool-runner/packages/cli/dist/index.js"
    );
    assert_eq!(
        remapped["tasks"][0]["executor"]["config"]["artifact_root"],
        "/home/user/Developer/example-project/artifacts"
    );
    assert!(out.iter().any(|a| a == "--record-run-id=loop-1"));
}

#[test]
fn remap_agent_task_run_plan_remaps_component_contract_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plan = temp.path().join("plan.json");
    std::fs::write(
        &plan,
        serde_json::json!({
            "schema": "homeboy/agent-task-plan/v1",
            "plan_id": "plan-1",
            "component_contracts": [{
                "slug": "generic-component",
                "path": "/Users/user/Developer/generic-component",
                "loadAs": "plugin",
                "activate": true,
                "opaque": { "preserved": true }
            }],
            "tasks": [{
                "task_id": "task-1",
                "executor": { "backend": "tool-runner" },
                "instructions": "test"
            }]
        })
        .to_string(),
    )
    .expect("write plan");
    let mappings = vec![LabPathRemap {
        local: "/Users/user/Developer/generic-component".to_string(),
        remote: "/srv/homeboy/_lab_workspaces/generic-component-snapshot".to_string(),
    }];
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "run-plan".to_string(),
        format!("--plan=@{}", plan.display()),
    ];

    let out = remap_agent_task_plan_in_args(&args, &mappings, temp.path()).expect("remap plan");
    let remapped: serde_json::Value = serde_json::from_str(
        out.iter()
            .find(|arg| arg.starts_with("--plan="))
            .and_then(|arg| arg.strip_prefix("--plan="))
            .expect("inline plan"),
    )
    .expect("inline plan json");

    assert_eq!(
        remapped["component_contracts"][0]["path"],
        "/srv/homeboy/_lab_workspaces/generic-component-snapshot"
    );
    assert_eq!(remapped["component_contracts"][0]["loadAs"], "plugin");
    assert_eq!(
        remapped["component_contracts"][0]["opaque"]["preserved"],
        true
    );
}

#[test]
#[cfg(unix)]
fn remap_agent_task_run_plan_prefers_canonical_symlink_target() {
    let temp = tempfile::tempdir().expect("tempdir");
    let primary = temp.path().join("example-project");
    let tool = temp.path().join("tool-runner");
    let tool_bin = tool.join("packages/cli/dist/index.js");
    let symlink = primary.join(".ci/tool-runner");
    let plan = primary.join(".ci/site-generation-loop.agent-task-plan.json");
    std::fs::create_dir_all(symlink.parent().unwrap()).expect("ci dir");
    std::fs::create_dir_all(tool_bin.parent().unwrap()).expect("tool bin dir");
    std::fs::write(&tool_bin, "#!/usr/bin/env node\n").expect("tool bin");
    std::os::unix::fs::symlink(&tool, &symlink).expect("tool symlink");
    let symlinked_bin = symlink.join("packages/cli/dist/index.js");
    std::fs::write(
        &plan,
        serde_json::json!({
            "schema": "homeboy/agent-task-plan/v1",
            "plan_id": "plan-1",
            "tasks": [{
                "task_id": "task-1",
                "executor": {
                    "backend": "tool-runner",
                    "config": { "tool_bin": symlinked_bin }
                },
                "instructions": "test"
            }]
        })
        .to_string(),
    )
    .expect("write plan");

    let mappings = vec![
        LabPathRemap {
            local: primary.canonicalize().unwrap().display().to_string(),
            remote: "/home/user/_lab_workspaces/wp-site-generator".to_string(),
        },
        LabPathRemap {
            local: tool.canonicalize().unwrap().display().to_string(),
            remote: "/home/user/_lab_workspaces/tool-runner".to_string(),
        },
    ];
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "run-plan".to_string(),
        "--plan".to_string(),
        format!("@{}", plan.display()),
    ];

    let out = remap_agent_task_plan_in_args(&args, &mappings, &primary).expect("remap plan");
    let plan_idx = out.iter().position(|a| a == "--plan").unwrap() + 1;
    let remapped: serde_json::Value = serde_json::from_str(&out[plan_idx]).expect("inline plan");

    assert_eq!(
        remapped["tasks"][0]["executor"]["config"]["tool_bin"],
        "/home/user/_lab_workspaces/tool-runner/packages/cli/dist/index.js"
    );
}

#[test]
fn remap_agent_task_run_plan_relative_file_spec_uses_source_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("example-project");
    let plan = source.join(".ci/plan.json");
    std::fs::create_dir_all(plan.parent().unwrap()).expect("plan dir");
    std::fs::write(
        &plan,
        serde_json::json!({
            "schema": "homeboy/agent-task-plan/v1",
            "tasks": [{
                "task_id": "task-1",
                "executor": {
                    "backend": "tool-runner",
                    "config": { "artifact_root": source.join("artifacts") }
                }
            }]
        })
        .to_string(),
    )
    .expect("write plan");
    let mappings = vec![LabPathRemap {
        local: source.display().to_string(),
        remote: "/home/user/Developer/example-project".to_string(),
    }];
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "run-plan".to_string(),
        "--plan".to_string(),
        "@.ci/plan.json".to_string(),
    ];

    let out = remap_agent_task_plan_in_args(&args, &mappings, &source).expect("remap plan");
    let remapped: serde_json::Value = serde_json::from_str(&out[4]).expect("inline plan");

    assert_eq!(
        remapped["tasks"][0]["executor"]["config"]["artifact_root"],
        "/home/user/Developer/example-project/artifacts"
    );
}

#[test]
fn remap_agent_task_run_plan_rejects_missing_file_spec() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mappings = vec![LabPathRemap {
        local: temp.path().display().to_string(),
        remote: "/home/user/Developer/example-project".to_string(),
    }];
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "run-plan".to_string(),
        "--plan".to_string(),
        "@.ci/missing.json".to_string(),
    ];

    let err = remap_agent_task_plan_in_args(&args, &mappings, temp.path())
        .expect_err("missing plan must fail locally");

    assert_eq!(err.details["field"], "plan");
    assert!(err.message.contains("controller-side file does not exist"));
}

#[test]
fn remap_agent_task_run_plan_rejects_remote_url_file_spec() {
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "run-plan".to_string(),
        "--plan=@https://example.test/plan.json".to_string(),
    ];

    let err = remap_agent_task_plan_in_args(&args, &[], Path::new("/tmp"))
        .expect_err("remote plan spec must fail locally");

    assert_eq!(err.details["field"], "plan");
    assert!(err.message.contains("local filesystem @file"));
}

#[test]
fn inline_agent_task_prompt_files_reads_absolute_prompt_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let prompt = temp.path().join("prompt.md");
    std::fs::write(&prompt, "Cook this repo\nwith care").expect("write prompt");
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--prompt".to_string(),
        format!("@{}", prompt.display()),
        "--backend=sample-runtime".to_string(),
    ];

    let out = inline_agent_task_prompt_files_in_args(&args, temp.path()).expect("inline prompt");

    assert_eq!(out[4], "Cook this repo\nwith care");
    assert!(out.iter().all(|arg| !arg.starts_with('@')));
    assert_eq!(out[5], "--backend=sample-runtime");
}

#[test]
fn inline_agent_task_prompt_files_reads_relative_task_and_tasks_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("task.md"), "Fix issue 1").expect("write task");
    std::fs::write(temp.path().join("tasks.json"), "[\"Fix issue 2\"]").expect("write tasks");
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--task=@task.md".to_string(),
        "--tasks".to_string(),
        "@tasks.json".to_string(),
    ];

    let out = inline_agent_task_prompt_files_in_args(&args, temp.path()).expect("inline files");

    assert_eq!(out[3], "--task=Fix issue 1");
    assert_eq!(out[5], "[\"Fix issue 2\"]");
}

#[test]
fn inline_agent_task_prompt_files_preserves_passthrough_args() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("prompt.md"), "Inline before passthrough")
        .expect("write prompt");
    std::fs::write(temp.path().join("ignored.md"), "must stay referenced").expect("write ignored");
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--prompt=@prompt.md".to_string(),
        "--".to_string(),
        "--task".to_string(),
        "@ignored.md".to_string(),
    ];

    let out = inline_agent_task_prompt_files_in_args(&args, temp.path()).expect("inline files");

    assert_eq!(out[3], "--prompt=Inline before passthrough");
    assert_eq!(out[5], "--task");
    assert_eq!(out[6], "@ignored.md");
}

#[test]
fn inline_agent_task_prompt_files_rejects_missing_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--prompt".to_string(),
        "@missing.md".to_string(),
    ];

    let err = inline_agent_task_prompt_files_in_args(&args, temp.path())
        .expect_err("missing prompt must fail locally");

    assert_eq!(err.details["field"], "prompt");
    assert!(err.message.contains("controller-side file does not exist"));
}

#[test]
fn remap_path_settings_rewrites_local_path_values() {
    let mappings = vec![LabPathRemap {
        local: "/Users/user/Developer/tool-runner".to_string(),
        remote: "/home/user/_lab_workspaces/tool-runner".to_string(),
    }];
    let args = vec![
        "homeboy".to_string(),
        "trace".to_string(),
        "--setting".to_string(),
        "tool_bin=/Users/user/Developer/tool-runner/packages/cli/dist/index.js".to_string(),
        "--setting=mode=fast".to_string(),
    ];

    let out = remap_path_settings_in_args(&args, &mappings);

    assert_eq!(
        out[3],
        "tool_bin=/home/user/_lab_workspaces/tool-runner/packages/cli/dist/index.js"
    );
    assert_eq!(out[4], "--setting=mode=fast");
}

#[test]
fn remap_path_settings_rewrites_json_array_path_values() {
    let mappings = vec![LabPathRemap {
        local: "/Users/user/Developer/woocommerce-gateway-stripe".to_string(),
        remote: "/home/user/_lab_workspaces/woocommerce-gateway-stripe".to_string(),
    }];
    let args = vec![
        "homeboy".to_string(),
        "bench".to_string(),
        "--setting-json".to_string(),
        "validation_dependencies=[\"/Users/user/Developer/woocommerce-gateway-stripe\"]"
            .to_string(),
        "--setting-json=depends_on={\"plugins\":[\"/Users/user/Developer/woocommerce-gateway-stripe/includes\"],\"token\":\"keep-secret-like-string\"}".to_string(),
    ];

    let out = remap_path_settings_in_args(&args, &mappings);

    assert_eq!(
        out[3],
        "validation_dependencies=[\"/home/user/_lab_workspaces/woocommerce-gateway-stripe\"]"
    );
    assert_eq!(
        out[4],
        "--setting-json=depends_on={\"plugins\":[\"/home/user/_lab_workspaces/woocommerce-gateway-stripe/includes\"],\"token\":\"keep-secret-like-string\"}"
    );
}

#[test]
fn remap_does_not_match_sibling_path_prefixes() {
    let mappings = vec![LabPathRemap {
        local: "/a/b".to_string(),
        remote: "/x/y".to_string(),
    }];
    let args = vec![
        "homeboy".to_string(),
        "cook".to_string(),
        "--provider-config".to_string(),
        serde_json::json!({ "p": "/a/bc/keep", "q": "/a/b/move" }).to_string(),
    ];
    let out = remap_provider_config_in_args(&args, &mappings);
    let idx = out.iter().position(|a| a == "--provider-config").unwrap() + 1;
    let v: serde_json::Value = serde_json::from_str(&out[idx]).unwrap();
    assert_eq!(v["p"], "/a/bc/keep"); // sibling prefix untouched
    assert_eq!(v["q"], "/x/y/move"); // real prefix remapped
}

#[test]
fn lab_args_rewrite_agent_task_dispatch_cwd() {
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "dispatch".to_string(),
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "--cwd=/Users/user/Developer/wp-site-generator".to_string(),
        "--prompt".to_string(),
        "cook".to_string(),
    ];

    assert_eq!(
        rewrite_lab_offload_args(&args, "/home/user/Developer/wp-site-generator", &[]),
        vec![
            "homeboy".to_string(),
            "--force-hot".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--cwd=/home/user/Developer/wp-site-generator".to_string(),
            "--prompt".to_string(),
            "cook".to_string(),
        ]
    );
}

#[test]
fn lab_args_rewrite_path_with_dependency_mapping() {
    let args = vec![
        "homeboy".to_string(),
        "bench".to_string(),
        "--path".to_string(),
        "/controller/repo/packages/component".to_string(),
    ];
    let mappings = vec![LabPathRemap {
        local: "/controller/repo".to_string(),
        remote: "/runner/repo".to_string(),
    }];

    assert_eq!(
        rewrite_lab_offload_args(&args, "/runner/primary", &mappings),
        vec![
            "homeboy".to_string(),
            "--force-hot".to_string(),
            "bench".to_string(),
            "--path".to_string(),
            "/runner/repo/packages/component".to_string(),
        ]
    );
}

#[test]
fn lab_args_rewrite_path_prefers_more_specific_duplicate_local_mapping() {
    let args = vec![
        "homeboy".to_string(),
        "bench".to_string(),
        "--path=/controller/repo/packages/component".to_string(),
    ];
    let mappings = vec![
        LabPathRemap {
            local: "/controller/repo/packages/component".to_string(),
            remote: "/runner/primary".to_string(),
        },
        LabPathRemap {
            local: "/controller/repo/packages/component".to_string(),
            remote: "/runner/repo/packages/component".to_string(),
        },
    ];

    assert_eq!(
        rewrite_lab_offload_args(&args, "/runner/primary", &mappings),
        vec![
            "homeboy".to_string(),
            "--force-hot".to_string(),
            "bench".to_string(),
            "--path=/runner/repo/packages/component".to_string(),
        ]
    );
}
