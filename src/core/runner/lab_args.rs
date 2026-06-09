use std::path::PathBuf;

use serde_json::Value;

use crate::core::config::read_json_spec_to_string;
use crate::core::worktree;
use crate::core::{Error, Result};

pub(super) const EXPLICIT_PASSTHROUGH_SENTINEL: &str = "__homeboy_explicit_passthrough__";

/// A local -> remote path pair produced by Lab workspace sync, used to remap
/// controller-side absolute paths embedded in a `--provider-config` payload to
/// the synced locations on the runner.
#[derive(Debug, Clone)]
pub(super) struct LabPathRemap {
    pub local: String,
    pub remote: String,
}

/// Rewrite controller-local absolute paths inside a `--provider-config` value to
/// their synced remote equivalents, and inline the result so the runner does not
/// need to read a controller-local file.
///
/// A hand-authored provider-config embeds absolute paths that only exist on the
/// controller (`mounts[].source`, `workspace_root`, `runtime_component_paths.*`,
/// `provider_plugin_paths[]`, ...). Lab offload syncs those directories and
/// records local->remote pairs, but without rewriting the config the remote
/// sandbox cannot find them. This walks the JSON and replaces every string that
/// begins with a known local path prefix with the matching remote path, then
/// returns the config as inline JSON so it travels with the offloaded command.
pub(super) fn remap_provider_config_in_args(
    args: &[String],
    mappings: &[LabPathRemap],
) -> Vec<String> {
    if mappings.is_empty() {
        return args.to_vec();
    }

    // Longest local prefix first so nested paths remap against the most specific
    // workspace (e.g. a dependency under the primary checkout).
    let mut ordered: Vec<&LabPathRemap> = mappings.iter().collect();
    ordered.sort_by_key(|mapping| std::cmp::Reverse(mapping.local.len()));

    let mut out = Vec::with_capacity(args.len());
    let mut iter = args.iter().peekable();
    let mut passthrough = false;
    while let Some(arg) = iter.next() {
        if passthrough {
            out.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            out.push(arg.clone());
            continue;
        }
        if arg == "--provider-config" {
            out.push(arg.clone());
            if let Some(spec) = iter.next() {
                out.push(remap_provider_config_spec(spec, &ordered));
            }
            continue;
        }
        if let Some(spec) = arg.strip_prefix("--provider-config=") {
            out.push(format!(
                "--provider-config={}",
                remap_provider_config_spec(spec, &ordered)
            ));
            continue;
        }
        out.push(arg.clone());
    }
    out
}

pub(super) fn remap_agent_task_plan_in_args(
    args: &[String],
    mappings: &[LabPathRemap],
) -> Vec<String> {
    if mappings.is_empty() {
        return args.to_vec();
    }

    let mut ordered: Vec<&LabPathRemap> = mappings.iter().collect();
    ordered.sort_by_key(|mapping| std::cmp::Reverse(mapping.local.len()));

    let mut out = Vec::with_capacity(args.len());
    let mut iter = args.iter().peekable();
    let mut passthrough = false;
    while let Some(arg) = iter.next() {
        if passthrough {
            out.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            out.push(arg.clone());
            continue;
        }
        if arg == "--plan" {
            out.push(arg.clone());
            if let Some(spec) = iter.next() {
                out.push(remap_at_file_spec(spec, &ordered));
            }
            continue;
        }
        if let Some(spec) = arg.strip_prefix("--plan=") {
            out.push(format!("--plan={}", remap_at_file_spec(spec, &ordered)));
            continue;
        }
        out.push(arg.clone());
    }
    out
}

fn remap_at_file_spec(spec: &str, mappings: &[&LabPathRemap]) -> String {
    let Some(path) = spec.strip_prefix('@') else {
        return spec.to_string();
    };
    remap_local_path(path, mappings)
        .map(|remapped| format!("@{remapped}"))
        .unwrap_or_else(|| spec.to_string())
}

/// Resolve a provider-config spec (inline JSON / `@file` / `-`), remap its
/// embedded local paths, and return inline JSON. Falls back to the original spec
/// if it cannot be read or parsed so behavior is never worse than today.
fn remap_provider_config_spec(spec: &str, mappings: &[&LabPathRemap]) -> String {
    let raw = match read_json_spec_to_string(spec) {
        Ok(raw) => raw,
        Err(_) => return spec.to_string(),
    };
    let mut value: Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => return spec.to_string(),
    };
    remap_paths_in_value(&mut value, mappings);
    serde_json::to_string(&value).unwrap_or_else(|_| spec.to_string())
}

fn remap_paths_in_value(value: &mut Value, mappings: &[&LabPathRemap]) {
    match value {
        Value::String(text) => {
            if let Some(remapped) = remap_local_path(text, mappings) {
                *text = remapped;
            }
        }
        Value::Array(items) => {
            for item in items {
                remap_paths_in_value(item, mappings);
            }
        }
        Value::Object(map) => {
            for (_, item) in map.iter_mut() {
                remap_paths_in_value(item, mappings);
            }
        }
        _ => {}
    }
}

/// Replace a leading known local path with its remote equivalent. Matches whole
/// path or path-prefix boundaries (so `/a/b` does not match `/a/bc`).
fn remap_local_path(text: &str, mappings: &[&LabPathRemap]) -> Option<String> {
    for mapping in mappings {
        if mapping.local.is_empty() {
            continue;
        }
        if text == mapping.local {
            return Some(mapping.remote.clone());
        }
        let prefix = format!("{}/", mapping.local.trim_end_matches('/'));
        if let Some(rest) = text.strip_prefix(&prefix) {
            return Some(format!("{}/{}", mapping.remote.trim_end_matches('/'), rest));
        }
    }
    None
}

pub(super) fn lab_offload_source_path(args: &[String]) -> Result<PathBuf> {
    let mut iter = args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--path" || arg == "--cwd" {
            let value = iter.next().ok_or_else(|| {
                let field = arg.trim_start_matches("--");
                Error::validation_invalid_argument(
                    field,
                    format!("{arg} requires a value before Lab offload can sync the workspace"),
                    None,
                    None,
                )
            })?;
            return Ok(PathBuf::from(shellexpand::tilde(value).to_string()));
        }
        if arg == "--to-worktree" {
            let value = iter.next().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "to_worktree",
                    "--to-worktree requires a value before Lab offload can sync the target worktree",
                    None,
                    None,
                )
            })?;
            return worktree::resolve(value).map(|record| PathBuf::from(record.worktree_path));
        }
        if let Some(value) = arg.strip_prefix("--path=") {
            return Ok(PathBuf::from(shellexpand::tilde(value).to_string()));
        }
        if let Some(value) = arg.strip_prefix("--cwd=") {
            return Ok(PathBuf::from(shellexpand::tilde(value).to_string()));
        }
        if let Some(value) = arg.strip_prefix("--to-worktree=") {
            return worktree::resolve(value).map(|record| PathBuf::from(record.worktree_path));
        }
    }

    std::env::current_dir()
        .map_err(|err| Error::internal_io(err.to_string(), Some("read cwd".to_string())))
}

pub(super) fn rewrite_lab_offload_args(args: &[String], remote_path: &str) -> Vec<String> {
    let mut stripped = Vec::with_capacity(args.len());
    let mut iter = args.iter().peekable();
    let mut passthrough = false;
    let has_force_hot = args.iter().any(|arg| arg == "--force-hot");
    while let Some(arg) = iter.next() {
        if arg == EXPLICIT_PASSTHROUGH_SENTINEL {
            continue;
        }
        if passthrough {
            stripped.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            stripped.push(arg.clone());
            continue;
        }
        if arg == "--path" || arg == "--cwd" {
            stripped.push(arg.clone());
            let _ = iter.next();
            stripped.push(remote_path.to_string());
            continue;
        }
        if arg.starts_with("--path=") {
            stripped.push(format!("--path={remote_path}"));
            continue;
        }
        if arg.starts_with("--cwd=") {
            stripped.push(format!("--cwd={remote_path}"));
            continue;
        }
        if arg == "--runner" {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--runner=") {
            continue;
        }
        if arg == "--output" {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--output=") {
            continue;
        }
        stripped.push(arg.clone());
    }
    if !has_force_hot {
        stripped.insert(1, "--force-hot".to_string());
    }
    stripped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lab_source_path_uses_agent_task_dispatch_cwd() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--cwd".to_string(),
            "/Users/chubes/Developer/wp-site-generator".to_string(),
            "--prompt".to_string(),
            "cook".to_string(),
        ];

        assert_eq!(
            lab_offload_source_path(&args).expect("source path"),
            PathBuf::from("/Users/chubes/Developer/wp-site-generator")
        );
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
                local: "/Users/chubes/Developer/data-machine@cook".to_string(),
                remote: "/home/chubes/_lab_workspaces/data-machine@cook-abc".to_string(),
            },
            LabPathRemap {
                local: "/Users/chubes/Developer/data-machine-code".to_string(),
                remote: "/home/chubes/_lab_workspaces/data-machine-code-def".to_string(),
            },
        ];
        let config = serde_json::json!({
            "workspace_root": "/Users/chubes/Developer/data-machine@cook",
            "mounts": [{ "source": "/Users/chubes/Developer/data-machine@cook", "target": "/workspace/data-machine" }],
            "runtime_component_paths": { "agent_runtime_tools": "/Users/chubes/Developer/data-machine-code" },
            "provider_plugin_paths": ["/Users/chubes/Developer/data-machine@cook/vendor/provider"],
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
            "/home/chubes/_lab_workspaces/data-machine@cook-abc"
        );
        assert_eq!(
            remapped["mounts"][0]["source"],
            "/home/chubes/_lab_workspaces/data-machine@cook-abc"
        );
        assert_eq!(remapped["mounts"][0]["target"], "/workspace/data-machine");
        assert_eq!(
            remapped["runtime_component_paths"]["agent_runtime_tools"],
            "/home/chubes/_lab_workspaces/data-machine-code-def"
        );
        assert_eq!(
            remapped["provider_plugin_paths"][0],
            "/home/chubes/_lab_workspaces/data-machine@cook-abc/vendor/provider"
        );
        assert_eq!(remapped["model"], "claude-opus-4-8");
        // unrelated args preserved
        assert!(out.iter().any(|a| a == "--prompt"));
        assert!(out.iter().any(|a| a == "fix it"));
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

        // No mappings -> untouched
        let unchanged = remap_provider_config_in_args(&args, &[]);
        assert_eq!(unchanged, args);
    }

    #[test]
    fn remap_agent_task_run_plan_absolute_file_spec() {
        let mappings = vec![LabPathRemap {
            local: "/Users/chubes/Developer/wp-site-generator".to_string(),
            remote: "/home/chubes/Developer/wp-site-generator".to_string(),
        }];
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--plan".to_string(),
            "@/Users/chubes/Developer/wp-site-generator/.ci/plan.json".to_string(),
            "--record-run-id=loop-1".to_string(),
        ];

        assert_eq!(
            remap_agent_task_plan_in_args(&args, &mappings),
            vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "run-plan".to_string(),
                "--plan".to_string(),
                "@/home/chubes/Developer/wp-site-generator/.ci/plan.json".to_string(),
                "--record-run-id=loop-1".to_string(),
            ]
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
            "--cwd=/Users/chubes/Developer/wp-site-generator".to_string(),
            "--prompt".to_string(),
            "cook".to_string(),
        ];

        assert_eq!(
            rewrite_lab_offload_args(&args, "/home/chubes/Developer/wp-site-generator"),
            vec![
                "homeboy".to_string(),
                "--force-hot".to_string(),
                "agent-task".to_string(),
                "dispatch".to_string(),
                "--cwd=/home/chubes/Developer/wp-site-generator".to_string(),
                "--prompt".to_string(),
                "cook".to_string(),
            ]
        );
    }
}
