//! Registered-context detection for the default `status` view.
//!
//! Determines whether the current (or git-root) directory maps to a registered
//! component/project checkout, so `homeboy status` can fast-return an
//! actionable "unregistered context" hint instead of scanning every configured
//! component.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use homeboy::core::git;

use super::types::UnregisteredContextStatusOutput;

pub(super) fn unregistered_cwd_status_output() -> Option<UnregisteredContextStatusOutput> {
    let cwd = std::env::current_dir().ok()?;
    let git_root = git::get_git_root(&cwd.to_string_lossy())
        .ok()
        .map(PathBuf::from);
    let candidates = [Some(cwd.as_path()), git_root.as_deref()];

    if candidates
        .into_iter()
        .flatten()
        .any(path_is_registered_context)
    {
        return None;
    }

    let git_root_string = git_root
        .as_ref()
        .map(|path| path.to_string_lossy().to_string());
    let suggestion = if let Some(ref git_root) = git_root_string {
        format!(
            "Repo not attached. Prefer: `homeboy project components attach-path <project-id> {}`",
            git_root
        )
    } else {
        "Repo not attached. Prefer: `homeboy project components attach-path <project-id> <path>`"
            .to_string()
    };

    Some(UnregisteredContextStatusOutput {
        command: "status",
        status: "unregistered_context",
        cwd: cwd.to_string_lossy().to_string(),
        git_root: git_root_string,
        suggestion,
        action: "Run `homeboy status --all` to inspect every configured component, or attach this checkout to a project/component first.",
    })
}

fn path_is_registered_context(path: &Path) -> bool {
    registered_local_paths().into_iter().any(|registered| {
        path_is_at_or_inside(&registered, path) || path_is_at_or_inside(path, &registered)
    })
}

fn registered_local_paths() -> Vec<PathBuf> {
    let Ok(home) = std::env::var("HOME") else {
        return Vec::new();
    };
    let config_root = PathBuf::from(home).join(".config").join("homeboy");
    [config_root.join("components"), config_root.join("projects")]
        .into_iter()
        .flat_map(json_files_under)
        .filter_map(|path| fs::read_to_string(path).ok())
        .filter_map(|raw| serde_json::from_str::<Value>(&raw).ok())
        .flat_map(|value| {
            let mut paths = Vec::new();
            collect_local_paths(&value, &mut paths);
            paths
        })
        .collect()
}

fn json_files_under(root: PathBuf) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_json_files(&root, &mut files);
    files
}

fn collect_json_files(path: &Path, files: &mut Vec<PathBuf>) {
    if path.is_file() {
        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            files.push(path.to_path_buf());
        }
        return;
    }

    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        collect_json_files(&entry.path(), files);
    }
}

fn collect_local_paths(value: &Value, paths: &mut Vec<PathBuf>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if matches!(key.as_str(), "local_path" | "localPath") {
                    if let Some(path) = value.as_str().filter(|path| !path.trim().is_empty()) {
                        paths.push(homeboy::core::expand_tilde_path(path));
                    }
                }
                collect_local_paths(value, paths);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_local_paths(item, paths);
            }
        }
        _ => {}
    }
}

fn path_is_at_or_inside(parent: &Path, path: &Path) -> bool {
    match (parent.canonicalize().ok(), path.canonicalize().ok()) {
        (Some(parent), Some(path)) => path == parent || path.starts_with(parent),
        _ => false,
    }
}
