//! Rig and stack discovery within a rig package.
//!
//! Cohesive group extracted from the install lifecycle: walking a rig package
//! to enumerate its rig and stack specs, and selecting the requested rig. Kept
//! in a sibling module so the install root stays under the structural
//! item-count threshold (#5241).

use homeboy_core::error::{Error, ErrorCode, Result};
use homeboy_extension as extension;
use homeboy_stack::stack;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredRig {
    pub id: String,
    pub description: String,
    pub rig_path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredStack {
    pub id: String,
    pub description: String,
    pub stack_path: PathBuf,
}

pub fn discover_rigs(package_path: &Path) -> Result<Vec<DiscoveredRig>> {
    discover_rigs_matching(package_path, None)
}

pub(crate) fn discover_rigs_for_install(
    package_path: &Path,
    id: Option<&str>,
    all: bool,
) -> Result<Vec<DiscoveredRig>> {
    if all {
        return discover_rigs_with_preflight(package_path);
    }
    let Some(id) = id else {
        return discover_rigs(package_path);
    };
    discover_rigs_matching(package_path, Some(&extension::slugify_id(id)?))
}

/// Discover every package member before returning a schema failure so `--all`
/// can report every incompatible rig without changing installed state.
fn discover_rigs_with_preflight(package_path: &Path) -> Result<Vec<DiscoveredRig>> {
    let candidates = discover_rig_paths(package_path, None)?;
    let mut rigs = Vec::new();
    let mut outcomes = Vec::new();

    for (path, fallback_name) in candidates {
        let fallback_id = fallback_name
            .as_deref()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        match discovered_from_path(&path, fallback_name.as_deref(), package_path) {
            Ok(rig) => {
                outcomes.push(serde_json::json!({
                    "id": rig.id,
                    "spec_path": rig.rig_path,
                    "status": "preflight_passed",
                }));
                rigs.push(rig);
            }
            Err(error) => outcomes.push(serde_json::json!({
                "id": fallback_id,
                "spec_path": path,
                "status": if error.message.contains("schema is not compatible") {
                    "incompatible"
                } else {
                    "failed"
                },
                "schema_failure_path": error.details.get("field").cloned().unwrap_or_else(|| serde_json::json!("rig_spec")),
                "error": error.message,
                "active_homeboy_version": homeboy_product_identity::product_version(),
                "upgrade_guidance": "Run `homeboy upgrade` to install a binary that supports this rig schema, then retry.",
            })),
        }
    }

    if outcomes
        .iter()
        .any(|outcome| outcome["status"] != "preflight_passed")
    {
        return Err(Error::new(
            ErrorCode::ValidationMultipleErrors,
            "Rig package preflight failed; no rigs were installed",
            serde_json::json!({
                "atomic": true,
                "active_homeboy_version": homeboy_product_identity::product_version(),
                "outcomes": outcomes,
            }),
        ));
    }

    rigs.sort_by(|a, b| a.id.cmp(&b.id));
    rigs.dedup_by(|a, b| a.id == b.id);
    if rigs.is_empty() {
        return no_rigs_found(package_path);
    }
    Ok(rigs)
}

fn discover_rigs_matching(
    package_path: &Path,
    only_id: Option<&str>,
) -> Result<Vec<DiscoveredRig>> {
    let mut rigs = discover_rig_paths(package_path, only_id)?
        .into_iter()
        .map(|(path, fallback_name)| {
            discovered_from_path(&path, fallback_name.as_deref(), package_path)
        })
        .collect::<Result<Vec<_>>>()?;

    rigs.sort_by(|a, b| a.id.cmp(&b.id));
    rigs.dedup_by(|a, b| a.id == b.id);

    if rigs.is_empty() {
        if only_id.is_some() {
            return Ok(rigs);
        }
        return no_rigs_found(package_path);
    }

    Ok(rigs)
}

fn discover_rig_paths(
    package_path: &Path,
    only_id: Option<&str>,
) -> Result<Vec<(PathBuf, Option<std::ffi::OsString>)>> {
    let mut rigs = Vec::new();
    let single = package_path.join("rig.json");
    if single.is_file() {
        rigs.push((single, package_path.file_name().map(ToOwned::to_owned)));
    }

    let rigs_dir = package_path.join("rigs");
    if rigs_dir.is_dir() {
        for entry in fs::read_dir(&rigs_dir)
            .map_err(|e| Error::internal_io(e.to_string(), Some("read rigs dir".into())))?
        {
            let entry = entry.map_err(|e| {
                Error::internal_io(e.to_string(), Some("read rig dir entry".into()))
            })?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let rig_path = path.join("rig.json");
            if rig_path.is_file() {
                if let Some(only_id) = only_id {
                    let candidate = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(extension::slugify_id)
                        .transpose()?;
                    if candidate.as_deref() != Some(only_id) {
                        continue;
                    }
                }
                rigs.push((rig_path, path.file_name().map(ToOwned::to_owned)));
            }
        }
    }

    rigs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(rigs)
}

fn no_rigs_found(package_path: &Path) -> Result<Vec<DiscoveredRig>> {
    Err(Error::validation_invalid_argument(
            "source",
            format!(
                "No rig specs found at {} (expected rig.json or rigs/<id>/rig.json)",
                package_path.display()
            ),
            Some(package_path.to_string_lossy().to_string()),
            Some(vec![
                format!("Expected one of: {}/rig.json", package_path.display()),
                format!(
                    "Expected one of: {}/rigs/<id>/rig.json",
                    package_path.display()
                ),
                "Install a package-shaped source with: homeboy rig install <package-path> --id <rig-id>".to_string(),
                "For command-local package discovery, run Homeboy from inside a checkout that contains rigs/<id>/rig.json.".to_string(),
            ]),
        ))
}

pub fn discover_stacks(package_path: &Path) -> Result<Vec<DiscoveredStack>> {
    let stacks_dir = package_path.join("stacks");
    if !stacks_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut stacks = Vec::new();
    for entry in fs::read_dir(&stacks_dir)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read stacks dir".into())))?
    {
        let entry = entry
            .map_err(|e| Error::internal_io(e.to_string(), Some("read stack entry".into())))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        stacks.push(discovered_stack_from_path(&path)?);
    }

    stacks.sort_by(|a, b| a.id.cmp(&b.id));
    stacks.dedup_by(|a, b| a.id == b.id);
    Ok(stacks)
}

fn discovered_stack_from_path(path: &Path) -> Result<DiscoveredStack> {
    let content = fs::read_to_string(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read stack spec".into())))?;
    let mut spec: stack::StackSpec = serde_json::from_str(&content).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some(format!("parse stack spec {}", path.display())),
            Some(content.chars().take(200).collect()),
        )
    })?;
    if spec.id.is_empty() {
        spec.id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "stack_id",
                    "Stack spec has no id and no filename fallback",
                    None,
                    None,
                )
            })?
            .to_string();
    }
    Ok(DiscoveredStack {
        id: spec.id,
        description: spec.description,
        stack_path: path.to_path_buf(),
    })
}

fn discovered_from_path(
    path: &Path,
    fallback_name: Option<&std::ffi::OsStr>,
    source_root: &Path,
) -> Result<DiscoveredRig> {
    let fallback_id = fallback_name
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();
    let discovered = DiscoveredRig {
        id: fallback_id.clone(),
        description: String::new(),
        rig_path: path.to_path_buf(),
    };
    // Validate dependency declarations for runner materialization, but keep
    // template inheritance bounded to the selected package root.
    super::install::local_package_source_root_for_dependencies(source_root, &[discovered])?;
    let mut spec = parse_discovered_rig_spec(path, source_root)?;
    if spec.id.is_empty() {
        spec.id = if fallback_id.is_empty() {
            return Err(Error::validation_invalid_argument(
                "rig_id",
                "Rig spec has no id and no directory name fallback",
                None,
                None,
            ));
        } else {
            fallback_id
        };
    }
    Ok(DiscoveredRig {
        id: extension::slugify_id(&spec.id)?,
        description: spec.description,
        rig_path: path.to_path_buf(),
    })
}

/// Parse a discovered rig spec for identity/schema validation.
///
/// `extends` children are partial specs by design — they may legitimately omit
/// required fields that a base template supplies, and they may carry
/// post-materialization-only constructs such as the array-merge directives
/// (`{ "$append": [...] }` / `{ "$merge_by": "label", "entries": [...] }`).
/// Validating the raw child against the full schema would reject those, so when
/// a spec declares `extends` we materialize it first (merging templates and
/// stripping directives) and validate the resolved spec.
fn parse_discovered_rig_spec(path: &Path, source_root: &Path) -> Result<super::RigSpec> {
    let value = super::install::materialize_rig_spec(path, source_root)?;

    serde_json::from_value(value).map_err(|e| {
        Error::validation_invalid_argument(
            "rig_spec",
            format!(
                "Rig spec schema is not compatible with this Homeboy binary: {}",
                e
            ),
            Some(path.to_string_lossy().to_string()),
            None,
        )
        .with_hint("Upgrade Homeboy, or update the rig spec to fields supported by this binary")
    })
}

pub(crate) fn select_rigs(
    rigs: Vec<DiscoveredRig>,
    id: Option<&str>,
    all: bool,
    source: &str,
) -> Result<Vec<DiscoveredRig>> {
    if all {
        return Ok(rigs);
    }
    if let Some(id) = id {
        let id = extension::slugify_id(id)?;
        let found: Vec<_> = rigs.into_iter().filter(|rig| rig.id == id).collect();
        if found.is_empty() {
            return Err(Error::validation_invalid_argument(
                "id",
                format!("Rig '{}' not found in package", id),
                Some(id),
                None,
            ));
        }
        return Ok(found);
    }
    if rigs.len() == 1 {
        return Ok(rigs);
    }
    let available = rigs.iter().map(|rig| rig.id.clone()).collect::<Vec<_>>();
    Err(Error::validation_invalid_argument(
        "id",
        format!(
            "Package contains multiple rigs; pass --id <rig> or --all. Available: {}",
            available.join(", ")
        ),
        Some(source.to_string()),
        Some(available),
    ))
}
