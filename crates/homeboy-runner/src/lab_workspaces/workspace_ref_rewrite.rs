use std::path::{Path, PathBuf};

use homeboy_core::{worktree, Error, Result};

use super::*;

pub(super) fn push_path_setting_value(raw: &str, values: &mut Vec<String>) {
    let Some((key, value)) = raw.split_once('=') else {
        return;
    };
    if key.trim().is_empty() || value.trim().is_empty() {
        return;
    }
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(value) {
        collect_path_setting_json_values(&json, values);
    } else {
        values.push(value.to_string());
    }
}

pub(super) fn collect_path_setting_json_values(
    value: &serde_json::Value,
    values: &mut Vec<String>,
) {
    match value {
        serde_json::Value::String(text) => values.push(text.to_string()),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_path_setting_json_values(item, values);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                collect_path_setting_json_values(item, values);
            }
        }
        _ => {}
    }
}

pub(super) fn rewrite_path_setting_workspace_refs_in_args(
    args: &[String],
    resolutions: &mut Vec<WorkspaceRefResolution>,
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
        if arg == "--setting" {
            out.push(arg.clone());
            if let Some(raw) = iter.next() {
                out.push(rewrite_path_setting_workspace_ref_pair(
                    raw,
                    false,
                    resolutions,
                )?);
            }
            continue;
        }
        if arg == "--setting-json" {
            out.push(arg.clone());
            if let Some(raw) = iter.next() {
                out.push(rewrite_path_setting_workspace_ref_pair(
                    raw,
                    true,
                    resolutions,
                )?);
            }
            continue;
        }
        if let Some(raw) = arg.strip_prefix("--setting=") {
            out.push(format!(
                "--setting={}",
                rewrite_path_setting_workspace_ref_pair(raw, false, resolutions)?
            ));
            continue;
        }
        if let Some(raw) = arg.strip_prefix("--setting-json=") {
            out.push(format!(
                "--setting-json={}",
                rewrite_path_setting_workspace_ref_pair(raw, true, resolutions)?
            ));
            continue;
        }
        out.push(arg.clone());
    }
    Ok(out)
}

pub(super) fn rewrite_path_setting_workspace_ref_pair(
    raw: &str,
    is_json: bool,
    resolutions: &mut Vec<WorkspaceRefResolution>,
) -> Result<String> {
    let Some((key, value)) = raw.split_once('=') else {
        return Ok(raw.to_string());
    };
    if is_json {
        let mut json: serde_json::Value = match serde_json::from_str(value) {
            Ok(value) => value,
            Err(_) => {
                let rewritten = rewrite_workspace_ref_value(value, resolutions)?;
                return Ok(format!("{key}={rewritten}"));
            }
        };
        rewrite_workspace_refs_in_json_value(&mut json, resolutions)?;
        return serde_json::to_string(&json)
            .map(|value| format!("{key}={value}"))
            .map_err(|err| Error::internal_json(err.to_string(), Some(key.to_string())));
    }

    let rewritten = rewrite_workspace_ref_value(value, resolutions)?;
    Ok(format!("{key}={rewritten}"))
}

pub(super) fn rewrite_workspace_refs_in_json_value(
    value: &mut serde_json::Value,
    resolutions: &mut Vec<WorkspaceRefResolution>,
) -> Result<()> {
    match value {
        serde_json::Value::String(text) => {
            if let Some(rewritten) = maybe_resolve_workspace_ref(text, resolutions)? {
                *text = rewritten;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                rewrite_workspace_refs_in_json_value(item, resolutions)?;
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values_mut() {
                rewrite_workspace_refs_in_json_value(item, resolutions)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn rewrite_workspace_ref_value(
    value: &str,
    resolutions: &mut Vec<WorkspaceRefResolution>,
) -> Result<String> {
    maybe_resolve_workspace_ref(value, resolutions)
        .map(|resolved| resolved.unwrap_or_else(|| value.to_string()))
}

pub(super) fn maybe_resolve_workspace_ref(
    value: &str,
    resolutions: &mut Vec<WorkspaceRefResolution>,
) -> Result<Option<String>> {
    let Some((handle, subpath)) = parse_workspace_ref(value) else {
        return Ok(None);
    };
    let record = worktree::resolve_workspace_ref(&handle).map_err(|_| {
        Error::validation_invalid_argument(
            "workspace_ref",
            format!("Lab offload workspace ref `{value}` does not match a known workspace handle"),
            Some(value.to_string()),
            Some(vec![
                "Create a Homeboy task worktree or adopt an existing path with `homeboy worktree adopt <handle> <path>`.".to_string(),
            ]),
        )
    })?;
    if record.state() != &TaskWorktreeState::Active {
        return Err(Error::validation_invalid_argument(
            "workspace_ref",
            format!(
                "Lab offload workspace ref `{value}` points at a stale {}",
                record.source_kind()
            ),
            Some(record.handle().to_string()),
            Some(vec![
                "Use an active workspace handle or adopt an existing path before Lab offload."
                    .to_string(),
            ]),
        ));
    }
    let workspace_path = PathBuf::from(record.path());
    let mut resolved = workspace_path.clone();
    if let Some(subpath) = subpath.as_deref() {
        resolved.push(subpath);
    }
    if !resolved.exists() {
        return Err(Error::validation_invalid_argument(
            "workspace_ref",
            format!("Lab offload workspace ref `{value}` resolved to a missing controller path"),
            Some(resolved.display().to_string()),
            Some(vec![
                "Use an existing optional subpath under the referenced Homeboy worktree."
                    .to_string(),
            ]),
        ));
    }
    resolutions.push(WorkspaceRefResolution {
        raw_ref: value.to_string(),
        handle,
        subpath,
        source_kind: record.source_kind().to_string(),
        source_provenance: record.provenance().cloned(),
        workspace_path,
        resolved_path: resolved.clone(),
    });
    Ok(Some(resolved.display().to_string()))
}

pub(super) fn parse_workspace_ref(value: &str) -> Option<(String, Option<String>)> {
    let rest = value.strip_prefix("@workspace:")?;
    let rest = rest.trim();
    if rest.is_empty() || rest.contains("://") {
        return None;
    }
    let (handle, subpath) = rest
        .split_once('/')
        .map(|(handle, subpath)| (handle, Some(subpath)))
        .unwrap_or((rest, None));
    if handle.trim().is_empty() || subpath.is_some_and(|value| value.trim().is_empty()) {
        return None;
    }
    Some((handle.to_string(), subpath.map(str::to_string)))
}

pub(super) fn subcommand_index(args: &[String], subcommand: &str) -> Option<usize> {
    args.iter().position(|arg| arg == subcommand)
}
