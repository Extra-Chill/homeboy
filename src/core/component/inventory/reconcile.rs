use crate::core::component::discover_from_portable;
use crate::core::error::{Error, Result};
use std::collections::BTreeSet;
use std::path::Path;

use super::path_diagnostics::{detect_git_root, is_temp_checkout_path, local_path_diagnostic_for};
use super::ComponentReconcileReport;

pub fn reconcile_standalone_registration(
    id: &str,
    apply: bool,
) -> Result<ComponentReconcileReport> {
    let dir = crate::core::paths::components()?;
    let registration_path = dir.join(format!("{}.json", id));
    let content = std::fs::read_to_string(&registration_path).map_err(|e| {
        Error::validation_invalid_argument(
            "component_id",
            format!("No standalone registration found for component '{id}': {e}"),
            Some(id.to_string()),
            Some(vec![
                "Run `homeboy component list` to inspect registered components".to_string(),
                "Run `homeboy component create --local-path <path>` to register a checkout"
                    .to_string(),
            ]),
        )
    })?;
    let mut json: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some(format!("parse {}", registration_path.display())),
            Some(content.chars().take(200).collect()),
        )
    })?;
    let registered_local_path = json
        .get("local_path")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let diagnostic = local_path_diagnostic_for(id, &registered_local_path);
    let mut status = diagnostic.status.clone();
    let mut discovered_local_path = if status == "ok" {
        None
    } else {
        unique_candidate(diagnostic.discovered_candidates.clone())
    };

    // A relative `local_path` is rejected by `release` even when it resolves
    // relative to the current directory. Flag it and repair to an absolute path.
    // (#6938)
    if crate::core::component::local_path_is_relative(&registered_local_path) {
        status = "relative_local_path".to_string();
        if discovered_local_path.is_none() {
            if let Ok(absolute) =
                crate::core::component::normalize_component_local_path(&registered_local_path)
            {
                if std::path::Path::new(&absolute).exists() {
                    discovered_local_path = Some(absolute);
                }
            }
        }
    }
    let repair = discovered_local_path
        .as_ref()
        .map(|path| format!("Update local_path to {path}"));
    let mut applied = false;

    if apply {
        let discovered = discovered_local_path.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "apply",
                "No safe repair path was discovered",
                Some(id.to_string()),
                Some(vec![
                    "Run without --apply to inspect the current registry state".to_string(),
                ]),
            )
        })?;
        if let Some(obj) = json.as_object_mut() {
            obj.insert(
                "local_path".to_string(),
                serde_json::Value::String(discovered.clone()),
            );
        }
        crate::core::component::portable::validate_component_remote_urls(&json)?;
        let updated = crate::core::config::to_string_pretty(&json)?;
        crate::core::engine::local_files::write_file_atomic(
            &registration_path,
            &updated,
            &format!(
                "write standalone registration {}",
                registration_path.display()
            ),
        )?;
        applied = true;
    }

    Ok(ComponentReconcileReport {
        component_id: id.to_string(),
        registration_path: registration_path.display().to_string(),
        registered_local_path,
        status,
        discovered_local_path,
        repair,
        applied,
    })
}

pub(super) fn reconcile_status(
    path: &Path,
    is_git_checkout: bool,
    is_temp_checkout: bool,
) -> String {
    if path.as_os_str().is_empty() {
        return "missing_local_path".to_string();
    }
    if !path.exists() {
        return "missing".to_string();
    }
    if !is_git_checkout {
        return "non_git".to_string();
    }
    if is_temp_checkout {
        return "temp_checkout".to_string();
    }
    "ok".to_string()
}

pub(super) fn discover_reconcile_candidates(id: &str, registered_path: &Path) -> Vec<String> {
    let mut roots = BTreeSet::new();
    if let Some(parent) = registered_path.parent() {
        roots.insert(parent.to_path_buf());
    }
    for root in super::path_diagnostics::stable_workspace_roots(registered_path) {
        roots.insert(root);
    }

    let mut candidates = BTreeSet::new();
    for root in roots {
        discover_candidate_children(id, &root, &mut candidates);
    }

    candidates.into_iter().collect()
}

fn discover_candidate_children(id: &str, root: &Path, candidates: &mut BTreeSet<String>) {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || detect_git_root(&path).is_none() || is_temp_checkout_path(&path) {
            continue;
        }
        let Some(component) = discover_from_portable(&path) else {
            continue;
        };
        if component.id == id {
            candidates.insert(path.to_string_lossy().to_string());
        }
    }
}

pub(super) fn unique_candidate(candidates: Vec<String>) -> Option<String> {
    if candidates.len() == 1 {
        candidates.into_iter().next()
    } else {
        None
    }
}

pub(super) fn standalone_registration_exists(id: &str) -> bool {
    crate::core::paths::components()
        .map(|dir| dir.join(format!("{id}.json")).exists())
        .unwrap_or(false)
}
