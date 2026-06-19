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

use super::path_remap::{remap_paths_in_value, LabPathRemap};

struct ArgTransformSpec<'a, C> {
    value_transforms: &'a [ArgValueTransform<'a, C>],
}

struct ArgValueTransform<'a, C> {
    flag: &'a str,
    transform: fn(&str, &C, &str) -> Result<String>,
}

impl<C> ArgTransformSpec<'_, C> {
    fn transform_args(&self, args: &[String], context: &C) -> Result<Vec<String>> {
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

            if let Some(transform) = self.value_transforms.iter().find(|item| item.flag == arg) {
                out.push(arg.clone());
                if let Some(value) = iter.next() {
                    out.push((transform.transform)(value, context, transform.flag)?);
                }
                continue;
            }

            let mut transformed = false;
            for transform in self.value_transforms {
                if let Some(value) = arg
                    .strip_prefix(transform.flag)
                    .and_then(|rest| rest.strip_prefix('='))
                {
                    out.push(format!(
                        "{}={}",
                        transform.flag,
                        (transform.transform)(value, context, transform.flag)?
                    ));
                    transformed = true;
                    break;
                }
            }
            if !transformed {
                out.push(arg.clone());
            }
        }
        Ok(out)
    }
}

pub(in crate::core::runner) fn remap_agent_task_plan_in_args(
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

pub(in crate::core::runner) fn inline_agent_task_prompt_files_in_args(
    args: &[String],
    source_path: &Path,
) -> Result<Vec<String>> {
    const AGENT_TASK_TEXT_FILE_TRANSFORMS: &[ArgValueTransform<'static, PathBuf>] = &[
        ArgValueTransform {
            flag: "--prompt",
            transform: inline_agent_task_text_file_arg,
        },
        ArgValueTransform {
            flag: "--task",
            transform: inline_agent_task_text_file_arg,
        },
        ArgValueTransform {
            flag: "--tasks",
            transform: inline_agent_task_text_file_arg,
        },
    ];

    ArgTransformSpec {
        value_transforms: AGENT_TASK_TEXT_FILE_TRANSFORMS,
    }
    .transform_args(args, &source_path.to_path_buf())
}

fn inline_agent_task_text_file_arg(
    spec: &str,
    source_path: &PathBuf,
    flag: &str,
) -> Result<String> {
    read_agent_task_text_spec_to_inline(spec, source_path, flag)
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
