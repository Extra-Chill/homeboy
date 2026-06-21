//! Controller spec dispatch-defaults policy.
//!
//! Owns the controller execution policy that derives `cwd`/`repo` dispatch
//! defaults for a repo-authored loop spec from its source path and the
//! surrounding git checkout. This logic used to live in the CLI adapter; it is
//! a reusable core service so callers (CLI, daemon, future automation) all apply
//! the same defaults before initializing or resuming a controller.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::core::git;

use super::AgentTaskRepoLoopSpec;

/// Apply controller dispatch defaults to `spec`, resolving the dispatch root
/// from the spec source path and the current working directory.
pub fn apply_spec_dispatch_defaults(spec: &mut AgentTaskRepoLoopSpec, spec_arg: &str) {
    apply_spec_dispatch_defaults_with_cwd(spec, spec_arg, || std::env::current_dir().ok());
}

/// Apply controller dispatch defaults to `spec`, resolving the dispatch root
/// with a caller-provided current-directory source (used by tests).
pub fn apply_spec_dispatch_defaults_with_cwd(
    spec: &mut AgentTaskRepoLoopSpec,
    spec_arg: &str,
    current_dir: impl FnOnce() -> Option<PathBuf>,
) {
    let Some(root) = dispatch_root_for_spec(spec_arg, current_dir) else {
        return;
    };
    apply_dispatch_defaults_for_root(spec, root);
}

fn dispatch_root_for_spec(
    spec_arg: &str,
    current_dir: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    let current_dir = current_dir();
    let Some(spec_path) = spec_file_path(spec_arg) else {
        return current_dir.and_then(|path| dispatch_root_for_current_dir(&path));
    };
    if let Some(root) = git_root_for_spec(&spec_path) {
        return Some(root);
    }
    current_dir.and_then(|path| dispatch_root_for_current_dir(&path))
}

fn dispatch_root_for_current_dir(path: &Path) -> Option<PathBuf> {
    git_root_for_path(path).or_else(|| {
        path.is_dir()
            .then(|| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
    })
}

fn apply_dispatch_defaults_for_root(spec: &mut AgentTaskRepoLoopSpec, root: PathBuf) {
    let repo = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();
    if repo.is_empty() {
        return;
    }

    let metadata = match &mut spec.metadata {
        Value::Object(metadata) => metadata,
        _ => {
            spec.metadata = serde_json::json!({});
            spec.metadata.as_object_mut().expect("metadata object")
        }
    };
    let defaults = metadata
        .entry("dispatch_defaults".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let Value::Object(defaults) = defaults else {
        return;
    };
    let should_set_cwd = defaults
        .get("cwd")
        .and_then(Value::as_str)
        .map(|cwd| !Path::new(cwd).is_dir())
        .unwrap_or(true);
    if should_set_cwd {
        defaults.insert("cwd".to_string(), Value::String(root.display().to_string()));
    }
    defaults
        .entry("repo".to_string())
        .or_insert_with(|| Value::String(repo));
}

fn spec_file_path(spec_arg: &str) -> Option<PathBuf> {
    if spec_arg == "-" || spec_arg.trim_start().starts_with('{') {
        return None;
    }
    let path = spec_arg.strip_prefix('@').unwrap_or(spec_arg);
    let path = PathBuf::from(path);
    path.is_file().then_some(path)
}

fn git_root_for_spec(spec_path: &Path) -> Option<PathBuf> {
    let dir = spec_path.parent()?;
    git_root_for_path(dir)
}

fn git_root_for_path(path: &Path) -> Option<PathBuf> {
    git::repo_root(path)
}
