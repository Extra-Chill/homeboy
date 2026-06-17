use std::fs;
use std::path::Path;
use std::path::PathBuf;

use serde_json::Value;

use crate::core::agent_task_config_materialization::materialize_provider_config_refs;
use crate::core::config::read_json_spec_to_string;
use crate::core::defaults;
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
    // NOTE: do not early-return on empty mappings. A `--provider-config @file`
    // (or `-` stdin) spec must always be inlined to JSON before offload, because
    // the controller-local file path is meaningless on the remote runner and the
    // remote dispatch would fail with "failed to read agent-task dispatch
    // provider-config input: IO error". Path remapping is a no-op without
    // mappings, but inlining still has to happen.
    //
    // Longest local prefix first so nested paths remap against the most specific
    // workspace (e.g. a dependency under the primary checkout).
    let mut ordered: Vec<&LabPathRemap> = mappings.iter().collect();
    ordered.sort_by_key(|mapping| {
        (
            std::cmp::Reverse(mapping.local.len()),
            std::cmp::Reverse(mapping.remote.len()),
        )
    });

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

pub(super) fn inject_agent_task_default_provider_config_in_args(
    args: &[String],
) -> Result<Vec<String>> {
    if !is_agent_task_dispatch_or_cook(args) || args_have_provider_config(args) {
        return Ok(args.to_vec());
    }

    let settings = defaults::load_config().settings;
    if settings.is_empty() {
        return Ok(args.to_vec());
    }

    let config = materialize_provider_config_refs(Value::Object(settings.into_iter().collect()))?;
    if config.as_object().is_none_or(|map| map.is_empty()) {
        return Ok(args.to_vec());
    }

    let config = serde_json::to_string(&config).map_err(|err| {
        Error::validation_invalid_argument(
            "provider_config",
            "failed to serialize materialized agent-task provider config for Lab offload",
            Some(err.to_string()),
            None,
        )
    })?;

    let mut out = Vec::with_capacity(args.len() + 2);
    let mut inserted = false;
    for arg in args {
        if !inserted && arg == "--" {
            out.push("--provider-config".to_string());
            out.push(config.clone());
            inserted = true;
        }
        out.push(arg.clone());
    }
    if !inserted {
        out.push("--provider-config".to_string());
        out.push(config);
    }
    Ok(out)
}

pub(super) fn remap_agent_task_plan_in_args(
    args: &[String],
    mappings: &[LabPathRemap],
    source_path: &Path,
) -> Result<Vec<String>> {
    let mut ordered: Vec<&LabPathRemap> = mappings.iter().collect();
    ordered.sort_by_key(|mapping| {
        (
            std::cmp::Reverse(mapping.local.len()),
            std::cmp::Reverse(mapping.remote.len()),
        )
    });

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
                out.push(remap_agent_task_plan_spec(spec, &ordered, source_path)?);
            }
            continue;
        }
        if let Some(spec) = arg.strip_prefix("--plan=") {
            out.push(format!(
                "--plan={}",
                remap_agent_task_plan_spec(spec, &ordered, source_path)?
            ));
            continue;
        }
        out.push(arg.clone());
    }
    Ok(out)
}

pub(super) fn inline_agent_task_prompt_files_in_args(
    args: &[String],
    source_path: &Path,
) -> Result<Vec<String>> {
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
        if matches!(arg.as_str(), "--prompt" | "--task" | "--tasks") {
            out.push(arg.clone());
            if let Some(spec) = iter.next() {
                out.push(read_agent_task_text_spec_to_inline(spec, source_path, arg)?);
            }
            continue;
        }
        if let Some(spec) = arg.strip_prefix("--prompt=") {
            out.push(format!(
                "--prompt={}",
                read_agent_task_text_spec_to_inline(spec, source_path, "--prompt")?
            ));
            continue;
        }
        if let Some(spec) = arg.strip_prefix("--task=") {
            out.push(format!(
                "--task={}",
                read_agent_task_text_spec_to_inline(spec, source_path, "--task")?
            ));
            continue;
        }
        if let Some(spec) = arg.strip_prefix("--tasks=") {
            out.push(format!(
                "--tasks={}",
                read_agent_task_text_spec_to_inline(spec, source_path, "--tasks")?
            ));
            continue;
        }
        out.push(arg.clone());
    }
    Ok(out)
}

pub(super) fn remap_path_settings_in_args(
    args: &[String],
    mappings: &[LabPathRemap],
) -> Vec<String> {
    if mappings.is_empty() {
        return args.to_vec();
    }

    let mut ordered: Vec<&LabPathRemap> = mappings.iter().collect();
    ordered.sort_by_key(|mapping| {
        (
            std::cmp::Reverse(mapping.local.len()),
            std::cmp::Reverse(mapping.remote.len()),
        )
    });

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
        if arg == "--setting" {
            out.push(arg.clone());
            if let Some(raw) = iter.next() {
                out.push(remap_path_setting_pair(raw, &ordered));
            }
            continue;
        }
        if arg == "--setting-json" {
            out.push(arg.clone());
            if let Some(raw) = iter.next() {
                out.push(remap_path_json_setting_pair(raw, &ordered));
            }
            continue;
        }
        if let Some(raw) = arg.strip_prefix("--setting=") {
            out.push(format!(
                "--setting={}",
                remap_path_setting_pair(raw, &ordered)
            ));
            continue;
        }
        if let Some(raw) = arg.strip_prefix("--setting-json=") {
            out.push(format!(
                "--setting-json={}",
                remap_path_json_setting_pair(raw, &ordered)
            ));
            continue;
        }
        out.push(arg.clone());
    }
    out
}

/// Resolve an agent-task plan spec, remap every controller-local path embedded
/// in the JSON, and inline the result so the runner never reads stale local
/// paths from a synced-but-unmodified plan file.
fn remap_agent_task_plan_spec(
    spec: &str,
    mappings: &[&LabPathRemap],
    source_path: &Path,
) -> Result<String> {
    if spec == "-" {
        return Ok(spec.to_string());
    }

    let raw = read_agent_task_plan_spec_to_string(spec, source_path)?;
    let mut value: Value = serde_json::from_str(&raw).map_err(|err| {
        Error::validation_invalid_json(
            err,
            Some(format!("parse agent-task run-plan --plan {spec}")),
            None,
        )
    })?;
    remap_paths_in_value(&mut value, mappings);
    serde_json::to_string(&value).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("serialize remapped agent-task plan".to_string()),
        )
    })
}

fn read_agent_task_plan_spec_to_string(spec: &str, source_path: &Path) -> Result<String> {
    let Some(raw_path) = spec.strip_prefix('@') else {
        return read_json_spec_to_string(spec);
    };
    if raw_path.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "plan",
            "Lab offload cannot materialize empty agent-task plan file spec '@'",
            Some(spec.to_string()),
            Some(vec![
                "Pass inline JSON, '-', or @path/to/plan.json.".to_string()
            ]),
        ));
    }
    if raw_path.contains("://") {
        return Err(Error::validation_invalid_argument(
            "plan",
            "Lab offload only supports local filesystem @file plan specs",
            Some(spec.to_string()),
            Some(vec![
                "Use an absolute path, a path relative to the current directory, or a path relative to --cwd/--path.".to_string(),
                "Remote URLs must be downloaded or generated locally before Lab offload.".to_string(),
            ]),
        ));
    }

    let expanded = PathBuf::from(shellexpand::tilde(raw_path).to_string());
    let mut candidates = vec![expanded.clone()];
    if expanded.is_relative() {
        candidates.push(source_path.join(&expanded));
    }

    let mut tried = Vec::new();
    for candidate in candidates {
        tried.push(candidate.display().to_string());
        if candidate.is_file() {
            return fs::read_to_string(&candidate).map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some(format!("read agent-task plan {}", candidate.display())),
                )
            });
        }
    }

    Err(Error::validation_invalid_argument(
        "plan",
        "Lab offload cannot materialize agent-task run-plan @file because the controller-side file does not exist",
        Some(spec.to_string()),
        Some(tried),
    ))
}

fn read_agent_task_text_spec_to_inline(
    spec: &str,
    source_path: &Path,
    field: &str,
) -> Result<String> {
    if spec == "-" || !spec.starts_with('@') {
        return Ok(spec.to_string());
    }

    let raw_path = spec.trim_start_matches('@');
    if raw_path.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            field.trim_start_matches("--"),
            format!("Lab offload cannot materialize empty agent-task {field} file spec '@'"),
            Some(spec.to_string()),
            Some(vec![
                "Pass inline text, '-', or @path/to/prompt.md.".to_string()
            ]),
        ));
    }
    if raw_path.contains("://") {
        return Err(Error::validation_invalid_argument(
            field.trim_start_matches("--"),
            format!("Lab offload only supports local filesystem {field} @file specs"),
            Some(spec.to_string()),
            Some(vec![
                "Use an absolute path, a path relative to the current directory, or a path relative to --cwd/--path.".to_string(),
                "Remote URLs must be downloaded or generated locally before Lab offload.".to_string(),
            ]),
        ));
    }

    let expanded = PathBuf::from(shellexpand::tilde(raw_path).to_string());
    let mut candidates = vec![expanded.clone()];
    if expanded.is_relative() {
        candidates.push(source_path.join(&expanded));
    }

    let mut tried = Vec::new();
    for candidate in candidates {
        tried.push(candidate.display().to_string());
        if candidate.is_file() {
            return fs::read_to_string(&candidate).map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some(format!("read agent-task {field} {}", candidate.display())),
                )
            });
        }
    }

    Err(Error::validation_invalid_argument(
        field.trim_start_matches("--"),
        format!("Lab offload cannot materialize agent-task {field} @file because the controller-side file does not exist"),
        Some(spec.to_string()),
        Some(tried),
    ))
}

fn remap_path_setting_pair(raw: &str, mappings: &[&LabPathRemap]) -> String {
    let Some((key, value)) = raw.split_once('=') else {
        return raw.to_string();
    };
    remap_local_path(value, mappings)
        .map(|remapped| format!("{key}={remapped}"))
        .unwrap_or_else(|| raw.to_string())
}

fn remap_path_json_setting_pair(raw: &str, mappings: &[&LabPathRemap]) -> String {
    let Some((key, value)) = raw.split_once('=') else {
        return raw.to_string();
    };
    let mut value: Value = match serde_json::from_str(value) {
        Ok(value) => value,
        Err(_) => return remap_path_setting_pair(raw, mappings),
    };
    remap_paths_in_value(&mut value, mappings);
    serde_json::to_string(&value)
        .map(|value| format!("{key}={value}"))
        .unwrap_or_else(|_| raw.to_string())
}

fn is_agent_task_dispatch_or_cook(args: &[String]) -> bool {
    let Some(agent_task_index) = args.iter().position(|arg| arg == "agent-task") else {
        return false;
    };
    args.iter()
        .skip(agent_task_index + 1)
        .find(|arg| !arg.starts_with('-'))
        .is_some_and(|command| matches!(command.as_str(), "dispatch" | "cook"))
}

fn args_have_provider_config(args: &[String]) -> bool {
    let mut passthrough = false;
    for arg in args {
        if passthrough {
            continue;
        }
        if arg == "--" {
            passthrough = true;
            continue;
        }
        if arg == "--provider-config" || arg.starts_with("--provider-config=") {
            return true;
        }
    }
    false
}

/// Resolve a provider-config spec (inline JSON / `@file` / `-`), remap its
/// embedded local paths, and return inline JSON.
///
/// A `@file` or `-` (stdin) spec is ALWAYS inlined to JSON, even when there are
/// no path mappings, because the controller-local path cannot be read by the
/// remote runner. A plain inline-JSON spec is only rewritten when there are
/// mappings to apply; otherwise it is returned verbatim so behavior is never
/// worse than passing the original argument through.
///
/// If a `@file`/`-` spec cannot be read or parsed, the original spec is returned
/// (the remote read will then surface the original, actionable error).
fn remap_provider_config_spec(spec: &str, mappings: &[&LabPathRemap]) -> String {
    let needs_inlining = is_provider_config_file_spec(spec);

    if !needs_inlining && mappings.is_empty() {
        return spec.to_string();
    }

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

/// A provider-config spec that points at a controller-local file (`@path`) or
/// stdin (`-`). These must be inlined before offload so the remote runner never
/// tries to read a path that only exists on the controller.
fn is_provider_config_file_spec(spec: &str) -> bool {
    let trimmed = spec.trim();
    trimmed == "-" || trimmed.starts_with('@')
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
    if let Some(remapped) = remap_existing_canonical_path(text, mappings) {
        return Some(remapped);
    }

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

fn remap_existing_canonical_path(text: &str, mappings: &[&LabPathRemap]) -> Option<String> {
    if !is_controller_path_like(text) {
        return None;
    }
    let expanded = shellexpand::tilde(text).to_string();
    let canonical = Path::new(&expanded).canonicalize().ok()?;
    let canonical = canonical.to_string_lossy().to_string();
    for mapping in mappings {
        if canonical == mapping.local {
            return Some(mapping.remote.clone());
        }
        let prefix = format!("{}/", mapping.local.trim_end_matches('/'));
        if let Some(rest) = canonical.strip_prefix(&prefix) {
            return Some(format!("{}/{}", mapping.remote.trim_end_matches('/'), rest));
        }
    }
    None
}

fn is_controller_path_like(value: &str) -> bool {
    value.starts_with('/') || value.starts_with("~/")
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

pub(super) fn rewrite_lab_offload_args(
    args: &[String],
    remote_path: &str,
    mappings: &[LabPathRemap],
) -> Vec<String> {
    let mut ordered: Vec<&LabPathRemap> = mappings.iter().collect();
    ordered.sort_by_key(|mapping| {
        (
            std::cmp::Reverse(mapping.local.len()),
            std::cmp::Reverse(mapping.remote.len()),
        )
    });
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
            let value = iter.next();
            stripped.push(
                value
                    .and_then(|value| remap_local_path(value, &ordered))
                    .unwrap_or_else(|| remote_path.to_string()),
            );
            continue;
        }
        if let Some(value) = arg.strip_prefix("--path=") {
            let rewritten =
                remap_local_path(value, &ordered).unwrap_or_else(|| remote_path.to_string());
            stripped.push(format!("--path={rewritten}"));
            continue;
        }
        if let Some(value) = arg.strip_prefix("--cwd=") {
            let rewritten =
                remap_local_path(value, &ordered).unwrap_or_else(|| remote_path.to_string());
            stripped.push(format!("--cwd={rewritten}"));
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

pub(super) fn rewrite_runner_resident_lab_offload_args(args: &[String]) -> Vec<String> {
    // A runner-side `tunnel service expose` should not require a separate
    // server declaration for the selected runner: in that context the runner
    // itself is the server (#4606). Drop the controller-side `--server` value
    // and mark the runner-side declaration as runner-local instead.
    let is_service_expose = is_tunnel_service_command(args, "expose");
    let already_runner_local = args.iter().any(|arg| arg == "--runner-local");
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
        if is_service_expose && arg == "--server" {
            let _ = iter.next();
            continue;
        }
        if is_service_expose && arg.starts_with("--server=") {
            continue;
        }
        stripped.push(arg.clone());
    }
    if is_service_expose && !already_runner_local {
        stripped.push("--runner-local".to_string());
    }
    if !has_force_hot {
        stripped.insert(1, "--force-hot".to_string());
    }
    stripped
}

/// Returns true when `args` invoke `tunnel service <subcommand>`.
fn is_tunnel_service_command(args: &[String], subcommand: &str) -> bool {
    args.windows(3).any(|window| {
        matches!(
            window,
            [first, second, third]
                if first == "tunnel" && second == "service" && third == subcommand
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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
            "/home/chubes/Developer/_lab_workspaces/site".to_string(),
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
                "/home/chubes/Developer/_lab_workspaces/site".to_string(),
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
    fn injects_default_provider_config_for_agent_task_cook() {
        crate::test_support::with_isolated_home(|_| {
            defaults::save_config(&defaults::HomeboyConfig {
                settings: HashMap::from([
                    ("provider".to_string(), serde_json::json!("codex")),
                    (
                        "provider_plugin_paths".to_string(),
                        serde_json::json!(["/Users/chubes/Developer/ai-provider-for-openai@codex"]),
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

            assert_eq!(config["provider"], "codex");
            assert_eq!(
                config["provider_plugin_paths"][0],
                "/Users/chubes/Developer/ai-provider-for-openai@codex"
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
                    serde_json::json!(["/Users/chubes/Developer/ai-provider-for-openai@codex"]),
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
            let injected =
                inject_agent_task_default_provider_config_in_args(&args).expect("inject");
            let remapped = remap_provider_config_in_args(
                &injected,
                &[LabPathRemap {
                    local: "/Users/chubes/Developer/ai-provider-for-openai@codex".to_string(),
                    remote: "/home/chubes/Developer/_lab_workspaces/ai-provider-for-openai@codex"
                        .to_string(),
                }],
            );
            let cfg_idx = remapped
                .iter()
                .position(|arg| arg == "--provider-config")
                .unwrap()
                + 1;
            let config: serde_json::Value =
                serde_json::from_str(&remapped[cfg_idx]).expect("config");

            assert_eq!(
                config["provider_plugin_paths"][0],
                "/home/chubes/Developer/_lab_workspaces/ai-provider-for-openai@codex"
            );
        });
    }

    #[test]
    fn explicit_provider_config_prevents_default_injection() {
        crate::test_support::with_isolated_home(|_| {
            defaults::save_config(&defaults::HomeboyConfig {
                settings: HashMap::from([(
                    "provider_plugin_paths".to_string(),
                    serde_json::json!(["/Users/chubes/Developer/ai-provider-for-openai@codex"]),
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
        std::fs::write(&cfg, r#"{"model":"claude-opus-4-8","backend":"codebox"}"#)
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
        assert_eq!(inlined["backend"], "codebox");
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
                            "tool_bin": "/Users/chubes/Developer/example-project/.ci/tool-runner/packages/cli/dist/index.js",
                            "artifact_root": "/Users/chubes/Developer/example-project/artifacts"
                        }
                    },
                    "instructions": "test"
                }]
            })
            .to_string(),
        )
        .expect("write plan");
        let mappings = vec![LabPathRemap {
            local: "/Users/chubes/Developer/example-project".to_string(),
            remote: "/home/chubes/Developer/example-project".to_string(),
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
        let remapped: serde_json::Value =
            serde_json::from_str(&out[plan_idx]).expect("inline plan");

        assert_eq!(
            remapped["tasks"][0]["executor"]["config"]["tool_bin"],
            "/home/chubes/Developer/example-project/.ci/tool-runner/packages/cli/dist/index.js"
        );
        assert_eq!(
            remapped["tasks"][0]["executor"]["config"]["artifact_root"],
            "/home/chubes/Developer/example-project/artifacts"
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
                    "path": "/Users/chubes/Developer/generic-component",
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
            local: "/Users/chubes/Developer/generic-component".to_string(),
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
                remote: "/home/chubes/_lab_workspaces/wp-site-generator".to_string(),
            },
            LabPathRemap {
                local: tool.canonicalize().unwrap().display().to_string(),
                remote: "/home/chubes/_lab_workspaces/tool-runner".to_string(),
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
        let remapped: serde_json::Value =
            serde_json::from_str(&out[plan_idx]).expect("inline plan");

        assert_eq!(
            remapped["tasks"][0]["executor"]["config"]["tool_bin"],
            "/home/chubes/_lab_workspaces/tool-runner/packages/cli/dist/index.js"
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
            remote: "/home/chubes/Developer/example-project".to_string(),
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
            "/home/chubes/Developer/example-project/artifacts"
        );
    }

    #[test]
    fn remap_agent_task_run_plan_rejects_missing_file_spec() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mappings = vec![LabPathRemap {
            local: temp.path().display().to_string(),
            remote: "/home/chubes/Developer/example-project".to_string(),
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
            "--backend=codebox".to_string(),
        ];

        let out =
            inline_agent_task_prompt_files_in_args(&args, temp.path()).expect("inline prompt");

        assert_eq!(out[4], "Cook this repo\nwith care");
        assert!(out.iter().all(|arg| !arg.starts_with('@')));
        assert_eq!(out[5], "--backend=codebox");
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
            local: "/Users/chubes/Developer/tool-runner".to_string(),
            remote: "/home/chubes/_lab_workspaces/tool-runner".to_string(),
        }];
        let args = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "--setting".to_string(),
            "tool_bin=/Users/chubes/Developer/tool-runner/packages/cli/dist/index.js".to_string(),
            "--setting=mode=fast".to_string(),
        ];

        let out = remap_path_settings_in_args(&args, &mappings);

        assert_eq!(
            out[3],
            "tool_bin=/home/chubes/_lab_workspaces/tool-runner/packages/cli/dist/index.js"
        );
        assert_eq!(out[4], "--setting=mode=fast");
    }

    #[test]
    fn remap_path_settings_rewrites_json_array_path_values() {
        let mappings = vec![LabPathRemap {
            local: "/Users/chubes/Developer/woocommerce-gateway-stripe".to_string(),
            remote: "/home/chubes/_lab_workspaces/woocommerce-gateway-stripe".to_string(),
        }];
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--setting-json".to_string(),
            "validation_dependencies=[\"/Users/chubes/Developer/woocommerce-gateway-stripe\"]"
                .to_string(),
            "--setting-json=depends_on={\"plugins\":[\"/Users/chubes/Developer/woocommerce-gateway-stripe/includes\"],\"token\":\"keep-secret-like-string\"}".to_string(),
        ];

        let out = remap_path_settings_in_args(&args, &mappings);

        assert_eq!(
            out[3],
            "validation_dependencies=[\"/home/chubes/_lab_workspaces/woocommerce-gateway-stripe\"]"
        );
        assert_eq!(
            out[4],
            "--setting-json=depends_on={\"plugins\":[\"/home/chubes/_lab_workspaces/woocommerce-gateway-stripe/includes\"],\"token\":\"keep-secret-like-string\"}"
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
            rewrite_lab_offload_args(&args, "/home/chubes/Developer/wp-site-generator", &[]),
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
}
