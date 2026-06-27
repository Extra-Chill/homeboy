use crate::core::component::Component;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::reconcile::{
    discover_reconcile_candidates, reconcile_status, standalone_registration_exists,
    unique_candidate,
};
use super::ComponentLocalPathDiagnostic;

pub fn local_path_diagnostic(component: &Component) -> ComponentLocalPathDiagnostic {
    local_path_diagnostic_for(&component.id, &component.local_path)
}

pub(super) fn local_path_diagnostic_for(
    id: &str,
    local_path: &str,
) -> ComponentLocalPathDiagnostic {
    let path = Path::new(local_path);
    let exists = path.exists();
    let git_root_path = detect_git_root(path);
    let is_git_checkout = git_root_path.is_some();
    let is_temp_checkout = is_temp_checkout_path(path);
    let status = reconcile_status(path, is_git_checkout, is_temp_checkout);
    let discovered_candidates = if status == "ok" {
        Vec::new()
    } else {
        discover_reconcile_candidates(id, path)
    };
    let unique_candidate = unique_candidate(discovered_candidates.clone());
    let warning = if status == "ok" {
        None
    } else {
        Some(match status.as_str() {
            "temp_checkout" => format!(
                "Component '{id}' local_path points at a temporary/opencode checkout: {local_path}"
            ),
            "missing" => format!("Component '{id}' local_path does not exist: {local_path}"),
            "non_git" => format!("Component '{id}' local_path is not a git checkout: {local_path}"),
            "missing_local_path" => format!("Component '{id}' has no local_path"),
            _ => format!("Component '{id}' local_path needs attention: {local_path}"),
        })
    };
    let repair_command = if let Some(candidate) = unique_candidate {
        if standalone_registration_exists(id) {
            Some(format!(
                "homeboy component reconcile {id} --apply # updates local_path to {}",
                shell_quote_for_hint(&candidate)
            ))
        } else {
            Some(format!(
                "homeboy component set {id} --local-path {}",
                shell_quote_for_hint(&candidate)
            ))
        }
    } else if status != "ok" {
        Some(format!(
            "homeboy component set {id} --local-path <stable-checkout-path>"
        ))
    } else {
        None
    };

    ComponentLocalPathDiagnostic {
        component_id: id.to_string(),
        local_path: local_path.to_string(),
        exists,
        is_git_checkout,
        is_temp_checkout,
        status,
        git_root: git_root_path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string()),
        branch: git_root_path
            .as_ref()
            .and_then(|path| git_output(path, &["rev-parse", "--abbrev-ref", "HEAD"])),
        head: git_root_path
            .as_ref()
            .and_then(|path| git_output(path, &["rev-parse", "--short", "HEAD"])),
        remote_url: git_root_path
            .as_ref()
            .and_then(|path| git_output(path, &["config", "--get", "remote.origin.url"])),
        upstream: git_root_path.as_ref().and_then(|path| {
            git_output(
                path,
                &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
            )
        }),
        discovered_candidates,
        warning,
        repair_command,
    }
}

pub(super) fn stable_workspace_roots(path: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        roots.push(PathBuf::from(home).join("Developer"));
    }
    if let Some(root) = stable_root_before_temp_marker(path) {
        roots.push(root);
    }
    roots.into_iter().filter(|root| root.is_dir()).collect()
}

fn stable_root_before_temp_marker(path: &Path) -> Option<PathBuf> {
    let segments = crate::core::paths::path_component_strings(path);
    let mut root = PathBuf::new();
    for segment in segments {
        if segment == "opencode" || segment == "tmp" || segment == "Temp" || segment == "T" {
            return if root.as_os_str().is_empty() {
                None
            } else {
                Some(root)
            };
        }
        root.push(&segment);
    }
    None
}

pub(super) fn is_temp_checkout_path(path: &Path) -> bool {
    let rendered = path.to_string_lossy();
    rendered.contains("/opencode/")
        || rendered.contains("/tmp/opencode/")
        || rendered.contains("/Temp/")
        || rendered.contains("/T/opencode/")
}

pub(super) fn detect_git_root(path: &Path) -> Option<PathBuf> {
    if path.join(".git").exists() {
        return Some(path.to_path_buf());
    }

    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        None
    } else {
        Some(PathBuf::from(raw))
    }
}

fn git_output(path: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn shell_quote_for_hint(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
