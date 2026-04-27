//! Stack spec — the JSON schema on disk at `~/.config/homeboy/stacks/{id}.json`.
//!
//! A **stack spec** is a declarative description of a combined-fixes
//! branch: an upstream `base` plus an ordered list of `prs` cherry-picked
//! on top, materialized into a `target` branch via `homeboy stack apply`.
//!
//! Mirrors the rig spec layout (one JSON file per stack, ID derived from
//! filename if absent in JSON). Supports `~` and `${env.VAR}` expansion in
//! the `component_path` field via the same token-expansion helper used by
//! rig specs, with a narrower token vocabulary.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::error::{Error, Result};
use crate::expand;
use crate::paths;

/// A stack: the spec for one combined-fixes branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackSpec {
    /// Stack identifier. Populated from filename if empty in JSON.
    #[serde(default)]
    pub id: String,

    /// Human-readable description shown in `stack list` / `stack show`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,

    /// Component identifier — informational. Used in display and as the
    /// (eventual) link key from a rig `ComponentSpec.stack` field.
    pub component: String,

    /// Local checkout path. Supports `~` and `${env.VAR}` expansion.
    /// Stack specs are self-contained: the path is declared inline rather
    /// than resolved through the global component registry, so a stack is
    /// usable on a fresh machine after a single git clone + JSON copy.
    pub component_path: String,

    /// Upstream ref the target is rebuilt from.
    pub base: GitRef,

    /// The combined-fixes branch the stack materializes.
    pub target: GitRef,

    /// PRs cherry-picked onto `target` in order.
    #[serde(default)]
    pub prs: Vec<StackPrEntry>,
}

/// A `<remote>/<branch>` pair. Split into two fields so callers can fetch
/// + checkout without having to re-parse a slash-joined string.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRef {
    pub remote: String,
    pub branch: String,
}

impl GitRef {
    /// Render as `<remote>/<branch>` for display + git ref construction.
    pub fn display(&self) -> String {
        format!("{}/{}", self.remote, self.branch)
    }
}

/// One PR entry in a stack's `prs` array.
///
/// Phase 2 will add `squash: bool` and `merge: bool` per-PR flags; the
/// struct shape is intentionally left open via `#[serde(default)]` on
/// optional fields so future additions don't break older specs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackPrEntry {
    /// `<owner>/<repo>` coordinate, e.g. `Automattic/studio`.
    pub repo: String,
    /// PR number on `repo`.
    pub number: u64,
    /// Optional human-readable note shown in `stack show` / `stack status`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Expand `~` and `${env.VAR}` in a stack spec field. Kept tiny on purpose:
/// stack specs only have one field that needs expansion (`component_path`).
pub fn expand_path(input: &str) -> String {
    expand::expand_with_tilde(input, |token| {
        token
            .strip_prefix("env.")
            .map(|name| std::env::var(name).unwrap_or_default())
    })
}

/// Resolve a stack's component checkout path and ensure it exists.
pub(crate) fn resolve_existing_component_path(spec: &StackSpec) -> Result<String> {
    let path = expand_path(&spec.component_path);
    if Path::new(&path).exists() {
        return Ok(path);
    }

    Err(Error::validation_invalid_argument(
        "component_path",
        format!(
            "Component path '{}' does not exist (stack '{}')",
            path, spec.id
        ),
        None,
        Some(vec![format!(
            "Edit ~/.config/homeboy/stacks/{}.json or clone the checkout",
            spec.id
        )]),
    ))
}

/// Load a stack spec by ID from `~/.config/homeboy/stacks/{id}.json`.
pub fn load(id: &str) -> Result<StackSpec> {
    let path = paths::stack_config(id)?;
    if !path.exists() {
        let suggestions = list_ids().unwrap_or_default();
        return Err(Error::stack_not_found(id, suggestions));
    }
    let content = fs::read_to_string(&path).map_err(|e| {
        Error::internal_unexpected(format!("Failed to read stack {}: {}", path.display(), e))
    })?;
    let mut spec: StackSpec = serde_json::from_str(&content).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some(format!("parse stack spec {}", path.display())),
            Some(content.chars().take(200).collect()),
        )
    })?;
    if spec.id.is_empty() {
        spec.id = id.to_string();
    }
    Ok(spec)
}

/// List all stack specs in `~/.config/homeboy/stacks/`.
pub fn list() -> Result<Vec<StackSpec>> {
    let dir = paths::stacks()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut stacks = Vec::new();
    for entry in fs::read_dir(&dir)
        .map_err(|e| Error::internal_unexpected(format!("Failed to list stacks: {}", e)))?
    {
        let entry = entry.map_err(|e| {
            Error::internal_unexpected(format!("Failed to read stack entry: {}", e))
        })?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if let Ok(spec) = load(&stem) {
            stacks.push(spec);
        }
    }
    stacks.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(stacks)
}

/// Return sorted stack IDs (cheaper than load+collect when you only need IDs,
/// e.g. for error suggestions).
pub fn list_ids() -> Result<Vec<String>> {
    let dir = paths::stacks()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in fs::read_dir(&dir)
        .map_err(|e| Error::internal_unexpected(format!("Failed to list stacks: {}", e)))?
    {
        let entry = entry.map_err(|e| {
            Error::internal_unexpected(format!("Failed to read stack entry: {}", e))
        })?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            ids.push(stem.to_string());
        }
    }
    ids.sort();
    Ok(ids)
}

/// Whether a stack spec with this ID already exists on disk.
pub fn exists(id: &str) -> Result<bool> {
    Ok(paths::stack_config(id)?.exists())
}

/// Write a spec to disk. Creates the stacks directory if missing. Pretty-printed
/// so humans can edit by hand.
pub fn save(spec: &StackSpec) -> Result<()> {
    let dir = paths::stacks()?;
    fs::create_dir_all(&dir).map_err(|e| {
        Error::internal_unexpected(format!(
            "Failed to create stacks dir {}: {}",
            dir.display(),
            e
        ))
    })?;
    let path = paths::stack_config(&spec.id)?;
    let json = serde_json::to_string_pretty(spec).map_err(|e| {
        Error::internal_unexpected(format!("Failed to serialize stack spec: {}", e))
    })?;
    fs::write(&path, format!("{}\n", json)).map_err(|e| {
        Error::internal_unexpected(format!(
            "Failed to write stack spec {}: {}",
            path.display(),
            e
        ))
    })?;
    Ok(())
}

/// Parse a `<remote>/<branch>` CLI string into a [`GitRef`]. Errors with a
/// helpful suggestion if the slash is missing.
pub fn parse_git_ref(value: &str, field: &'static str) -> Result<GitRef> {
    match value.split_once('/') {
        Some((remote, branch)) if !remote.is_empty() && !branch.is_empty() => Ok(GitRef {
            remote: remote.to_string(),
            branch: branch.to_string(),
        }),
        _ => Err(Error::validation_invalid_argument(
            field,
            format!(
                "Expected `<remote>/<branch>`, got `{}` (e.g. `origin/trunk`, `fork/dev/combined-fixes`)",
                value
            ),
            None,
            Some(vec![format!(
                "{} should be like `origin/main` or `fork/dev/combined-fixes`",
                field
            )]),
        )),
    }
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn test_resolve_existing_component_path() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().to_string_lossy().to_string();
        let spec = StackSpec {
            id: "test-stack".to_string(),
            description: String::new(),
            component: "homeboy".to_string(),
            component_path: path.clone(),
            base: GitRef {
                remote: "origin".to_string(),
                branch: "main".to_string(),
            },
            target: GitRef {
                remote: "origin".to_string(),
                branch: "stack".to_string(),
            },
            prs: Vec::new(),
        };

        assert_eq!(resolve_existing_component_path(&spec).unwrap(), path);
    }

    #[test]
    fn resolve_existing_component_path_preserves_error_contract() {
        let spec = StackSpec {
            id: "missing-stack".to_string(),
            description: String::new(),
            component: "homeboy".to_string(),
            component_path: "/definitely/missing/homeboy-stack-checkout".to_string(),
            base: GitRef {
                remote: "origin".to_string(),
                branch: "main".to_string(),
            },
            target: GitRef {
                remote: "origin".to_string(),
                branch: "stack".to_string(),
            },
            prs: Vec::new(),
        };

        let err = resolve_existing_component_path(&spec).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("component_path"));
        assert!(msg.contains(
            "Component path '/definitely/missing/homeboy-stack-checkout' does not exist"
        ));
        assert_eq!(
            err.details
                .get("tried")
                .and_then(|v| v.as_array())
                .map(|v| v[0].as_str()),
            Some(Some(
                "Edit ~/.config/homeboy/stacks/missing-stack.json or clone the checkout"
            ))
        );
    }
}

#[cfg(test)]
#[path = "../../../tests/core/stack/spec_test.rs"]
mod spec_test;
