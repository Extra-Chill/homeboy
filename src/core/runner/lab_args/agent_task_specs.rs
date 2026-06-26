//! Agent-task `--plan`/`--prompt`/`--task`/`--tasks` spec materialization for
//! Lab offload.
//!
//! These helpers inline controller-local `@file` specs and remap embedded paths
//! so an offloaded agent-task command never references a path that only exists
//! on the controller.

use std::fs;
use std::path::Path;
use std::path::PathBuf;

use serde_json::Value;

use crate::core::config::read_json_spec_to_string;
use crate::core::{Error, Result};

use super::envelope::ExecutionEnvelope;
use super::path_remap::{
    order_mappings_by_specificity, remap_paths_in_value, try_rewrite_flag_value_args, LabPathRemap,
};

pub(in crate::core::runner) struct AgentTaskSpecMaterialization<T> {
    pub(in crate::core::runner) argv: Vec<String>,
    pub(in crate::core::runner) workspace_entries: Vec<AgentTaskSpecWorkspaceEntry<T>>,
}

pub(in crate::core::runner) struct AgentTaskSpecWorkspaceEntry<T> {
    pub(in crate::core::runner) step_id: &'static str,
    pub(in crate::core::runner) entry: T,
}

pub(in crate::core::runner) struct AgentTaskInlineJsonSpec<'a> {
    pub(in crate::core::runner) spec: &'a str,
    pub(in crate::core::runner) filename: &'static str,
    pub(in crate::core::runner) role: &'static str,
}

pub(in crate::core::runner) fn materialize_agent_task_specs_in_args<T>(
    args: &[String],
    mappings: &[LabPathRemap],
    source_path: &Path,
    mut sync_inline_json: impl FnMut(AgentTaskInlineJsonSpec<'_>) -> Result<Option<(String, T)>>,
) -> Result<AgentTaskSpecMaterialization<T>> {
    let remapped_args = remap_agent_task_plan_in_args(args, mappings, source_path)?;
    let remapped_args =
        remap_agent_task_fanout_input_in_args(&remapped_args, mappings, source_path)?;
    let remapped_args = inline_agent_task_prompt_files_in_args(&remapped_args, source_path)?;
    let (argv, workspace_entries) =
        materialize_inline_agent_task_json_specs_in_args(&remapped_args, |spec| {
            sync_inline_json(spec)
        })?;

    Ok(AgentTaskSpecMaterialization {
        argv,
        workspace_entries,
    })
}

fn remap_agent_task_fanout_input_in_args(
    args: &[String],
    mappings: &[LabPathRemap],
    source_path: &Path,
) -> Result<Vec<String>> {
    if !agent_task_subcommand_is(args, &["fanout", "run-plan"]) {
        return Ok(args.to_vec());
    }
    let ordered = order_mappings_by_specificity(mappings);

    try_rewrite_flag_value_args(args, |arg, iter, out| {
        if arg == "--input" {
            out.push(arg.to_string());
            if let Some(spec) = iter.next() {
                out.push(remap_agent_task_fanout_input_spec(
                    spec,
                    &ordered,
                    source_path,
                )?);
            }
            return Ok(());
        }
        if let Some(spec) = arg.strip_prefix("--input=") {
            out.push(format!(
                "--input={}",
                remap_agent_task_fanout_input_spec(spec, &ordered, source_path)?
            ));
            return Ok(());
        }
        out.push(arg.to_string());
        Ok(())
    })
}

pub(in crate::core::runner) fn materialize_inline_agent_task_json_specs_in_args<T>(
    args: &[String],
    mut sync_inline_json: impl FnMut(AgentTaskInlineJsonSpec<'_>) -> Result<Option<(String, T)>>,
) -> Result<(Vec<String>, Vec<AgentTaskSpecWorkspaceEntry<T>>)> {
    let (args, task_entry) = materialize_inline_json_option(
        args,
        agent_task_subcommand_is(args, &["dispatch", "cook"]),
        "--tasks",
        "agent-task-tasks.json",
        "agent_task_tasks_remapped",
        "lab.sync_remapped_agent_task_tasks",
        |spec| sync_inline_json(spec),
    )?;
    let (args, plan_entry) = materialize_inline_json_option(
        &args,
        agent_task_subcommand_is(&args, &["run-plan"]),
        "--plan",
        "agent-task-plan.json",
        "agent_task_plan_remapped",
        "lab.sync_remapped_agent_task_plan",
        |spec| sync_inline_json(spec),
    )?;

    let workspace_entries = task_entry.into_iter().chain(plan_entry).collect();
    Ok((args, workspace_entries))
}

pub(in crate::core::runner) fn remap_agent_task_plan_in_args(
    args: &[String],
    mappings: &[LabPathRemap],
    source_path: &Path,
) -> Result<Vec<String>> {
    let ordered = order_mappings_by_specificity(mappings);

    try_rewrite_flag_value_args(args, |arg, iter, out| {
        if arg == "--plan" {
            out.push(arg.to_string());
            if let Some(spec) = iter.next() {
                out.push(remap_agent_task_plan_spec(spec, &ordered, source_path)?);
            }
            return Ok(());
        }
        if let Some(spec) = arg.strip_prefix("--plan=") {
            out.push(format!(
                "--plan={}",
                remap_agent_task_plan_spec(spec, &ordered, source_path)?
            ));
            return Ok(());
        }
        out.push(arg.to_string());
        Ok(())
    })
}

pub(in crate::core::runner) fn inline_agent_task_prompt_files_in_args(
    args: &[String],
    source_path: &Path,
) -> Result<Vec<String>> {
    let envelope = ExecutionEnvelope::from_args(args);
    envelope.rewrite_agent_task_text_values(|spec, flag| {
        read_agent_task_text_spec_to_inline(spec, source_path, flag)
    })
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

fn remap_agent_task_fanout_input_spec(
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
            Some(format!("parse agent-task fanout run-plan --input {spec}")),
            None,
        )
    })?;
    let original_value = value.clone();
    remap_paths_in_value(&mut value, mappings);
    rewrite_fanout_cook_workspaces(&mut value, &original_value, mappings);
    serde_json::to_string(&value).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("serialize remapped agent-task fanout input".to_string()),
        )
    })
}

fn rewrite_fanout_cook_workspaces(
    value: &mut Value,
    original_value: &Value,
    mappings: &[&LabPathRemap],
) {
    let original_cooks = original_value
        .get("cooks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let Some(cooks) = value.get_mut("cooks").and_then(Value::as_array_mut) else {
        return;
    };
    for (index, cook) in cooks.iter_mut().enumerate() {
        let Some(object) = cook.as_object_mut() else {
            continue;
        };
        let original_object = original_cooks.get(index).and_then(Value::as_object);
        let mut materializations = Vec::new();
        for field in ["cwd", "workspace"] {
            let Some(controller_value) = original_object
                .and_then(|object| object.get(field))
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                continue;
            };
            let resolved_controller_path = crate::core::worktree::resolve(&controller_value)
                .ok()
                .map(|record| record.worktree_path)
                .unwrap_or_else(|| controller_value.clone());
            let Some(runner_path) = rewrite_path_with_mappings(&resolved_controller_path, mappings)
            else {
                continue;
            };
            object.insert(field.to_string(), Value::String(runner_path.clone()));
            materializations.push(serde_json::json!({
                "field": field,
                "controller_path": resolved_controller_path,
                "runner_path": runner_path,
                "branch": object.get("head").and_then(Value::as_str).or_else(|| object.get("base").and_then(Value::as_str)),
                "ref": object.get("head").and_then(Value::as_str).or_else(|| object.get("base").and_then(Value::as_str)),
                "sync_status": "materialized",
            }));
        }
        if !materializations.is_empty() {
            object.insert(
                "workspace_materialization".to_string(),
                Value::Array(materializations),
            );
        }
    }
}

fn rewrite_path_with_mappings(path: &str, mappings: &[&LabPathRemap]) -> Option<String> {
    mappings.iter().find_map(|mapping| {
        if path == mapping.local {
            return Some(mapping.remote.clone());
        }
        path.strip_prefix(&format!("{}/", mapping.local))
            .map(|suffix| format!("{}/{suffix}", mapping.remote))
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

fn agent_task_subcommand_is(args: &[String], subcommands: &[&str]) -> bool {
    subcommand_index(args, "agent-task")
        .and_then(|index| args.get(index + 1))
        .is_some_and(|arg| subcommands.iter().any(|subcommand| arg == subcommand))
}

fn subcommand_index(args: &[String], command: &str) -> Option<usize> {
    args.iter().position(|arg| arg == command)
}

/// Scans `args` for a single `--flag <json>` / `--flag=<json>` occurrence and,
/// when `in_context` and the supplied `sync` closure produce a remapped spec,
/// rewrites that argument to point at the synced workspace file. Parsing stops
/// at the `--` passthrough boundary; this is argv materialization only and does
/// not interpret anything past `--`.
fn materialize_inline_json_option<T>(
    args: &[String],
    in_context: bool,
    flag: &'static str,
    filename: &'static str,
    role: &'static str,
    step_id: &'static str,
    mut sync: impl FnMut(AgentTaskInlineJsonSpec<'_>) -> Result<Option<(String, T)>>,
) -> Result<(Vec<String>, Option<AgentTaskSpecWorkspaceEntry<T>>)> {
    if !in_context {
        return Ok((args.to_vec(), None));
    }

    let flag_eq = format!("{flag}=");
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
        if arg == flag {
            out.push(arg.clone());
            if let Some(spec) = iter.next() {
                if let Some((remapped_spec, entry)) =
                    sync_inline_json_spec(spec, filename, role, &mut sync)?
                {
                    out.push(remapped_spec);
                    out.extend(iter.cloned());
                    return Ok((out, Some(AgentTaskSpecWorkspaceEntry { step_id, entry })));
                }
                out.push(spec.clone());
            }
            continue;
        }
        if let Some(spec) = arg.strip_prefix(&flag_eq) {
            if let Some((remapped_spec, entry)) =
                sync_inline_json_spec(spec, filename, role, &mut sync)?
            {
                out.push(format!("{flag}={remapped_spec}"));
                out.extend(iter.cloned());
                return Ok((out, Some(AgentTaskSpecWorkspaceEntry { step_id, entry })));
            }
        }
        out.push(arg.clone());
    }

    Ok((out, None))
}

fn sync_inline_json_spec<T>(
    spec: &str,
    filename: &'static str,
    role: &'static str,
    sync: &mut impl FnMut(AgentTaskInlineJsonSpec<'_>) -> Result<Option<(String, T)>>,
) -> Result<Option<(String, T)>> {
    if spec == "-" || spec.starts_with('@') || !looks_like_inline_json(spec) {
        return Ok(None);
    }
    sync(AgentTaskInlineJsonSpec {
        spec,
        filename,
        role,
    })
}

fn looks_like_inline_json(spec: &str) -> bool {
    let trimmed = spec.trim_start();
    trimmed.starts_with('{') || trimmed.starts_with('[')
}
