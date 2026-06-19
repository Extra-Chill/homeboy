//! `--provider-config` argument handling for Lab offload.
//!
//! Provider-config payloads embed controller-local absolute paths and may be
//! supplied as `@file`/`-`/inline JSON. These helpers inline file specs, remap
//! embedded paths, and inject a default provider config for agent-task dispatch.

use serde_json::Value;

use crate::core::agent_task_config_materialization::materialize_provider_config_refs;
use crate::core::config::read_json_spec_to_string;
use crate::core::defaults;
use crate::core::{Error, Result};

use super::path_remap::{remap_paths_in_value, LabPathRemap};

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
pub(in crate::core::runner) fn remap_provider_config_in_args(
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

pub(in crate::core::runner) fn inject_agent_task_default_provider_config_in_args(
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
