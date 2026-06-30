//! `--provider-config` argument handling for Lab offload.
//!
//! Provider-config payloads embed controller-local absolute paths and may be
//! supplied as `@file`/inline JSON. These helpers inline file specs, remap
//! embedded paths, and inject a default provider config for agent-task dispatch.

use std::path::Path;

use serde_json::Value;

use crate::core::agent_task_config_materialization::materialize_provider_config_refs;
use crate::core::config::read_json_spec_to_string;
use crate::core::defaults;
use crate::core::{Error, Result};

use super::envelope::{ArgValue, ExecutionEnvelope};
use super::path_remap::{remap_local_path, remap_paths_in_value, LabPathRemap};

const RUNTIME_MANIFEST_SCHEMA: &str = "homeboy/lab-provider-config-runtime/v1";

pub(in crate::core::runner) fn preflight_provider_config_paths_materialized_in_args(
    args: &[String],
    mappings: &[LabPathRemap],
) -> Result<()> {
    let envelope = ExecutionEnvelope::from_args(args);
    let mut failures = Vec::new();

    for config in &envelope.inputs.provider_configs {
        let Some((_source, raw)) = provider_config_raw_value(&config.value)? else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        collect_unmaterialized_paths(&value, "$", mappings, &mut failures);
    }

    if failures.is_empty() {
        return Ok(());
    }

    let preview = failures
        .iter()
        .take(5)
        .map(|failure| {
            format!(
                "{} at {} ({})",
                failure.path, failure.location, failure.reason
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    Err(Error::validation_invalid_argument(
        "provider-config",
        format!(
            "Lab offload cannot dispatch because {} provider-config runtime path(s) were not materialized on the selected runner",
            failures.len()
        ),
        Some(preview),
        Some(vec![
            "Use controller-local paths that exist so Lab can sync and rewrite them to runner paths before dispatch.".to_string(),
            "Remove stale runtime/component/provider-plugin paths from the provider config when the run should use runner-installed defaults.".to_string(),
            "Run with --force-hot --allow-local-hot only if you intentionally want to bypass Lab offload and execute locally.".to_string(),
        ]),
    ))
}

pub(in crate::core::runner) fn provider_config_runtime_manifest(args: &[String]) -> Value {
    let envelope = ExecutionEnvelope::from_args(args);
    let mut configs = Vec::new();
    for config in &envelope.inputs.provider_configs {
        let Ok(Some((source, raw))) = provider_config_raw_value(&config.value) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let mut paths = Vec::new();
        collect_runtime_manifest_paths(&value, "$", &mut paths);
        configs.push(serde_json::json!({
            "source": source,
            "paths": paths,
        }));
    }

    serde_json::json!({
        "schema": RUNTIME_MANIFEST_SCHEMA,
        "provider_configs": configs,
    })
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
pub(in crate::core::runner) fn remap_provider_config_in_args(
    args: &[String],
    mappings: &[LabPathRemap],
) -> Result<Vec<String>> {
    let envelope = ExecutionEnvelope::from_args(args);
    // NOTE: do not early-return on empty mappings. A `--provider-config @file`
    // spec must always be inlined to JSON before offload, because the
    // controller-local file path is meaningless on the remote runner and the
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

    envelope.try_rewrite_provider_config_values(|spec| remap_provider_config_spec(spec, &ordered))
}

pub(in crate::core::runner) fn inject_agent_task_default_provider_config_in_args(
    args: &[String],
) -> Result<Vec<String>> {
    let envelope = ExecutionEnvelope::from_args(args);
    if !is_agent_task_dispatch_or_cook(args) || !envelope.inputs.provider_configs.is_empty() {
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

fn provider_config_raw_value(value: &ArgValue) -> Result<Option<(String, String)>> {
    match value {
        ArgValue::InlineText(raw) => Ok(Some(("inline".to_string(), raw.clone()))),
        ArgValue::PathRef(path) => {
            let spec = format!("@{path}");
            let raw = read_json_spec_to_string(&spec).map_err(|err| {
                Error::validation_invalid_argument(
                    "provider-config",
                    "failed to read Lab offload --provider-config @file input",
                    Some(err.to_string()),
                    None,
                )
                .with_hint(
                    "Lab offload reads --provider-config @file specs on the controller before dispatch; provide a readable JSON file or inline JSON.",
                )
            })?;
            Ok(Some((spec, raw)))
        }
        ArgValue::Stdin | ArgValue::Missing => Ok(None),
    }
}

#[derive(Debug)]
struct UnmaterializedPath {
    location: String,
    path: String,
    reason: &'static str,
}

fn collect_unmaterialized_paths(
    value: &Value,
    location: &str,
    mappings: &[LabPathRemap],
    failures: &mut Vec<UnmaterializedPath>,
) {
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_unmaterialized_paths(
                    item,
                    &format!("{location}[{index}]"),
                    mappings,
                    failures,
                );
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                let child_location = format!("{location}.{key}");
                if is_materializable_provider_config_path_key(key) {
                    collect_unmaterialized_path_strings(item, &child_location, mappings, failures);
                } else {
                    collect_unmaterialized_paths(item, &child_location, mappings, failures);
                }
            }
        }
        _ => {}
    }
}

fn collect_unmaterialized_path_strings(
    value: &Value,
    location: &str,
    mappings: &[LabPathRemap],
    failures: &mut Vec<UnmaterializedPath>,
) {
    match value {
        Value::String(text) if is_controller_path_like(text) => {
            if is_provider_plugin_path_location(location)
                && provider_plugin_path_is_pruneable_before_offload(text, mappings)
            {
                return;
            }
            if let Some(reason) = unmaterialized_path_reason(text, mappings) {
                failures.push(UnmaterializedPath {
                    location: location.to_string(),
                    path: text.to_string(),
                    reason,
                });
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_unmaterialized_path_strings(
                    item,
                    &format!("{location}[{index}]"),
                    mappings,
                    failures,
                );
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                collect_unmaterialized_path_strings(
                    item,
                    &format!("{location}.{key}"),
                    mappings,
                    failures,
                );
            }
        }
        _ => {}
    }
}

fn is_materializable_provider_config_path_key(key: &str) -> bool {
    matches!(
        key,
        "workspace_root"
            | "source"
            | "source_cli"
            | "provider_root"
            | "provider_support"
            | "runtime_component_paths"
            | "provider_plugin_paths"
            | "component_contracts"
            | "path"
    )
}

fn is_provider_plugin_path_location(location: &str) -> bool {
    location.contains(".provider_plugin_paths")
}

fn provider_plugin_path_is_pruneable_before_offload(path: &str, mappings: &[LabPathRemap]) -> bool {
    let ordered = super::path_remap::order_mappings_by_specificity(mappings);
    remap_local_path(path, &ordered).is_none() && !path_is_under_remote_mapping(path, mappings)
}

fn unmaterialized_path_reason(path: &str, mappings: &[LabPathRemap]) -> Option<&'static str> {
    let expanded = shellexpand::tilde(path).to_string();
    if !Path::new(&expanded).exists() {
        return Some("controller path does not exist");
    }

    let ordered = super::path_remap::order_mappings_by_specificity(mappings);
    if remap_local_path(path, &ordered).is_some() || path_is_under_remote_mapping(path, mappings) {
        return None;
    }

    Some("controller path was not synced to the runner")
}

fn path_is_under_remote_mapping(path: &str, mappings: &[LabPathRemap]) -> bool {
    mappings.iter().any(|mapping| {
        if mapping.remote.is_empty() {
            return false;
        }
        path == mapping.remote
            || path.starts_with(&format!("{}/", mapping.remote.trim_end_matches('/')))
    })
}

fn collect_runtime_manifest_paths(value: &Value, location: &str, paths: &mut Vec<Value>) {
    match value {
        Value::String(text) if is_runtime_manifest_path(text) => {
            paths.push(serde_json::json!({
                "location": location,
                "path": text,
            }));
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_runtime_manifest_paths(item, &format!("{location}[{index}]"), paths);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                collect_runtime_manifest_paths(item, &format!("{location}.{key}"), paths);
            }
        }
        _ => {}
    }
}

fn is_runtime_manifest_path(value: &str) -> bool {
    is_controller_path_like(value) || value.starts_with('.')
}

fn is_controller_path_like(value: &str) -> bool {
    value.starts_with('/') || value.starts_with("~/")
}

/// Resolve a provider-config spec (inline JSON / `@file`), remap its
/// embedded local paths, and return inline JSON.
///
/// A `@file` spec is ALWAYS inlined to JSON, even when there are no path
/// mappings, because the controller-local path cannot be read by the remote
/// runner. Stdin (`-`) specs are rejected locally because offload argv
/// materialization cannot safely block on process stdin. A plain inline-JSON spec
/// is only rewritten when there are mappings to apply; otherwise it is returned
/// verbatim so behavior is never worse than passing the original argument
/// through.
///
/// If a `@file` spec cannot be read or parsed, Lab offload fails locally with
/// an actionable validation error instead of forwarding a controller-local spec
/// to the remote runner.
fn remap_provider_config_spec(spec: &str, mappings: &[&LabPathRemap]) -> Result<String> {
    if is_provider_config_stdin_spec(spec) {
        return Err(Error::validation_invalid_argument(
            "provider-config",
            "Lab offload does not support --provider-config -",
            None,
            None,
        )
        .with_hint(
            "Pass --provider-config as inline JSON or write stdin to a JSON file and pass --provider-config @path before Lab offload.",
        ));
    }

    let needs_inlining = is_provider_config_file_spec(spec);

    // Even with no mappings, an inline-JSON spec may carry stale controller-local
    // `provider_plugin_paths` that must be pruned before offload (a `@file`/`-`
    // spec always needs inlining). Without that pruning a stale path is forwarded
    // verbatim and breaks remote recipe validation, so we cannot early-return
    // here just because there is nothing to remap.
    if !needs_inlining
        && mappings.is_empty()
        && !spec_has_provider_plugin_paths(spec)
        && !spec_has_runtime_env_path_aliases(spec)
    {
        return Ok(spec.to_string());
    }

    let raw = match read_json_spec_to_string(spec) {
        Ok(raw) => raw,
        Err(err) if needs_inlining => {
            return Err(Error::validation_invalid_argument(
                "provider-config",
                "failed to read Lab offload --provider-config @file input",
                Some(err.to_string()),
                None,
            )
            .with_hint(
                "Lab offload reads --provider-config @file specs on the controller before dispatch; provide a readable JSON file or inline JSON.",
            ));
        }
        Err(_) => return Ok(spec.to_string()),
    };
    let mut value: Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(err) if needs_inlining => {
            return Err(Error::validation_invalid_json(
                err,
                Some("parse Lab offload --provider-config @file input".to_string()),
                Some(raw),
            )
            .with_hint(
                "Lab offload inlines --provider-config @file specs before dispatch; fix the JSON or pass valid inline JSON.",
            ));
        }
        Err(_) => return Ok(spec.to_string()),
    };
    remap_paths_in_value(&mut value, mappings);
    prune_unresolved_provider_plugin_paths(&mut value, mappings);
    normalize_runtime_env_path_aliases(&mut value);
    Ok(serde_json::to_string(&value).unwrap_or_else(|_| spec.to_string()))
}

/// Cheap pre-check so a plain inline-JSON spec with no mappings is only fully
/// parsed/rewritten when it actually carries `provider_plugin_paths` that might
/// need pruning. Avoids round-tripping every untouched provider-config.
fn spec_has_provider_plugin_paths(spec: &str) -> bool {
    spec.contains("provider_plugin_paths")
}

fn spec_has_runtime_env_path_aliases(spec: &str) -> bool {
    spec.contains("runtime_env_path_aliases") || spec.contains("runtime_env_aliases")
}

/// Normalize declared legacy/runtime env aliases to the structured component path.
///
/// Homeboy core stays provider-agnostic by requiring the provider config to
/// declare aliases explicitly, either as:
///
/// - `runtime_env_path_aliases`: `{ "component_key": "ENV_NAME" }`
/// - `runtime_env_aliases`: `{ "ENV_NAME": "component_key" }`
///
/// When both the structured component path and env alias are present but differ,
/// `runtime_component_paths` wins. The original env value and precedence rule are
/// retained in `runtime_env_path_alias_diagnostics` for operator diagnostics.
fn normalize_runtime_env_path_aliases(value: &mut Value) {
    let aliases = runtime_env_path_aliases(value);
    if aliases.is_empty() {
        return;
    }

    let Some(root) = value.as_object_mut() else {
        return;
    };
    let component_paths = root
        .get("runtime_component_paths")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if component_paths.is_empty() {
        return;
    }

    if !root.get("runtime_env").is_some_and(Value::is_object) {
        root.insert(
            "runtime_env".to_string(),
            Value::Object(serde_json::Map::new()),
        );
    }

    let mut diagnostics = Vec::new();
    let Some(runtime_env) = root.get_mut("runtime_env").and_then(Value::as_object_mut) else {
        return;
    };

    for (component_key, env_name) in aliases {
        let Some(selected_path) = component_paths
            .get(&component_key)
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let previous = runtime_env.get(&env_name).cloned().unwrap_or(Value::Null);
        if previous.as_str() == Some(selected_path.as_str()) {
            continue;
        }
        runtime_env.insert(env_name.clone(), Value::String(selected_path.clone()));
        diagnostics.push(serde_json::json!({
            "component_path_field": format!("runtime_component_paths.{component_key}"),
            "env_field": format!("runtime_env.{env_name}"),
            "selected_path": selected_path,
            "overridden_path": previous,
            "precedence": "runtime_component_paths wins over declared runtime_env alias before provider launch",
        }));
    }

    if !diagnostics.is_empty() {
        root.insert(
            "runtime_env_path_alias_diagnostics".to_string(),
            Value::Array(diagnostics),
        );
    }
}

fn runtime_env_path_aliases(value: &Value) -> Vec<(String, String)> {
    let mut aliases = Vec::new();
    if let Some(map) = value
        .get("runtime_env_path_aliases")
        .and_then(Value::as_object)
    {
        for (component_key, env_names) in map {
            collect_alias_env_names(component_key, env_names, &mut aliases);
        }
    }
    if let Some(map) = value.get("runtime_env_aliases").and_then(Value::as_object) {
        for (env_name, component_key) in map {
            if let Some(component_key) = component_key.as_str() {
                aliases.push((component_key.to_string(), env_name.to_string()));
            }
        }
    }
    aliases
}

fn collect_alias_env_names(
    component_key: &str,
    value: &Value,
    aliases: &mut Vec<(String, String)>,
) {
    match value {
        Value::String(env_name) => aliases.push((component_key.to_string(), env_name.to_string())),
        Value::Array(items) => {
            for item in items {
                if let Some(env_name) = item.as_str() {
                    aliases.push((component_key.to_string(), env_name.to_string()));
                }
            }
        }
        _ => {}
    }
}

/// Drop `provider_plugin_paths` entries that point at a controller-local
/// absolute path which was never remapped to a synced remote location.
///
/// Lab offload only syncs (and records local->remote mappings for) the
/// directories a cook actually references, so an absolute provider-plugin path
/// inherited from stale/global controller or runner settings (e.g. a provider
/// plugin path that is not part of this run's workspace) survives
/// remapping unchanged. Forwarding it would make the provider runtime declare
/// an extra plugin/workspace entry pointing at a directory that does not exist
/// on the runner, failing runtime validation with a `missing-path` error before the
/// task runs (homeboy #4829). Such entries are not materialized on the selected
/// runner, so we omit them; explicit, materializable refs and entries that did
/// remap into a synced remote location are preserved.
fn prune_unresolved_provider_plugin_paths(value: &mut Value, mappings: &[&LabPathRemap]) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    let Some(Value::Array(paths)) = map.get_mut("provider_plugin_paths") else {
        return;
    };
    paths.retain(|entry| match entry {
        // Non-string entries (e.g. materialized ref objects) are left untouched;
        // ref materialization already resolved them to a present path string.
        Value::String(path) => provider_plugin_path_is_resolvable(path, mappings),
        _ => true,
    });
}

/// A provider-plugin path is resolvable for the runner when it is not a bare
/// controller-local absolute path: relative paths are runner-relative, and any
/// path that already lives under a synced remote location (i.e. it was produced
/// by remapping) is valid. Only an absolute path that matches no synced remote
/// root is treated as stale/unresolvable.
fn provider_plugin_path_is_resolvable(path: &str, mappings: &[&LabPathRemap]) -> bool {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Relative paths resolve against the runner workspace, not the controller.
    if !trimmed.starts_with('/') {
        return true;
    }
    // A path that lives under a synced remote root was remapped (or authored to
    // point at the synced location) and will exist on the runner.
    mappings.iter().any(|mapping| {
        if mapping.remote.is_empty() {
            return false;
        }
        if trimmed == mapping.remote {
            return true;
        }
        let prefix = format!("{}/", mapping.remote.trim_end_matches('/'));
        trimmed.starts_with(&prefix)
    })
}

/// A provider-config spec that points at a controller-local file (`@path`). These
/// must be inlined before offload so the remote runner never tries to read a path
/// that only exists on the controller.
fn is_provider_config_file_spec(spec: &str) -> bool {
    let trimmed = spec.trim();
    trimmed.starts_with('@')
}

fn is_provider_config_stdin_spec(spec: &str) -> bool {
    spec.trim() == "-"
}
