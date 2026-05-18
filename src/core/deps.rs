use crate::core::component::{self, Component};
use crate::core::{Error, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod stack;

pub use stack::{
    stack_apply, stack_plan, stack_plan_from_components, stack_status, DependencyStackApplyResult,
    DependencyStackApplyStep, DependencyStackCommandResult, DependencyStackEdgeStatus,
    DependencyStackPlan, DependencyStackPlanStep, DependencyStackStatus,
};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyPackage {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_section: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constraint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked_reference: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyStatus {
    pub component_id: String,
    pub component_path: String,
    pub package_manager: String,
    pub packages: Vec<DependencyPackage>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyUpdateResult {
    pub component_id: String,
    pub component_path: String,
    pub package_manager: String,
    pub package: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_constraint: Option<String>,
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<DependencyPackage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<DependencyPackage>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerAction {
    Require { constraint: String },
    Update,
}

pub fn composer_command_args(package: &str, action: &ComposerAction) -> Vec<String> {
    match action {
        ComposerAction::Require { constraint } => vec![
            "require".to_string(),
            format!("{package}:{constraint}"),
            "--with-dependencies".to_string(),
            "--no-interaction".to_string(),
        ],
        ComposerAction::Update => vec![
            "update".to_string(),
            package.to_string(),
            "--with-dependencies".to_string(),
            "--no-interaction".to_string(),
        ],
    }
}

pub fn status(
    component_id: Option<&str>,
    path_override: Option<&str>,
    package_filter: Option<&str>,
) -> Result<DependencyStatus> {
    let (component, path) = resolve_component_path(component_id, path_override)?;
    composer_status(&component, &path, package_filter)
}

pub fn update(
    component_id: Option<&str>,
    path_override: Option<&str>,
    package: &str,
    constraint: Option<&str>,
) -> Result<DependencyUpdateResult> {
    let (component, path) = resolve_component_path(component_id, path_override)?;
    ensure_composer_component(&path)?;

    let before = package_snapshot(&path, package)?;
    let action = match constraint {
        Some(constraint) => ComposerAction::Require {
            constraint: constraint.to_string(),
        },
        None => ComposerAction::Update,
    };
    let args = composer_command_args(package, &action);
    let output = Command::new("composer")
        .args(&args)
        .current_dir(&path)
        .output()
        .map_err(|e| Error::internal_io(e.to_string(), Some("run composer".to_string())))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "composer",
            format!(
                "Composer command failed with status {}: {}",
                output.status,
                first_non_empty_line(&stderr)
                    .or_else(|| first_non_empty_line(&stdout))
                    .unwrap_or("no output")
            ),
            None,
            Some(vec![format!(
                "Run manually in {}: composer {}",
                path.display(),
                args.join(" ")
            )]),
        ));
    }

    let after = package_snapshot(&path, package)?;

    Ok(DependencyUpdateResult {
        component_id: component.id,
        component_path: path.display().to_string(),
        package_manager: "composer".to_string(),
        package: package.to_string(),
        requested_constraint: constraint.map(str::to_string),
        command: std::iter::once("composer".to_string())
            .chain(args)
            .collect(),
        before,
        after,
        stdout,
        stderr,
    })
}

fn resolve_component_path(
    component_id: Option<&str>,
    path_override: Option<&str>,
) -> Result<(Component, PathBuf)> {
    let component = component::resolve_effective(component_id, path_override, None)?;
    let path = PathBuf::from(shellexpand::tilde(&component.local_path).as_ref());

    if !path.exists() {
        return Err(Error::validation_invalid_argument(
            "component_path",
            format!(
                "Component '{}' path does not exist: {}",
                component.id,
                path.display()
            ),
            Some(component.id.clone()),
            None,
        ));
    }

    Ok((component, path))
}

fn ensure_composer_component(path: &Path) -> Result<()> {
    if path.join("composer.json").is_file() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "package_manager",
        format!("No supported dependency manifest found in {}", path.display()),
        None,
        Some(vec![
            "Composer MVP requires composer.json at the component root".to_string(),
            "npm, Cargo, and other package managers are intentionally out of scope for this command".to_string(),
        ]),
    ))
}

fn composer_status(
    component: &Component,
    path: &Path,
    package_filter: Option<&str>,
) -> Result<DependencyStatus> {
    ensure_composer_component(path)?;
    let packages = read_composer_packages(path, package_filter)?;

    Ok(DependencyStatus {
        component_id: component.id.clone(),
        component_path: path.display().to_string(),
        package_manager: "composer".to_string(),
        packages,
    })
}

fn package_snapshot(path: &Path, package: &str) -> Result<Option<DependencyPackage>> {
    Ok(read_composer_packages(path, Some(package))?
        .into_iter()
        .next())
}

fn read_composer_packages(
    path: &Path,
    package_filter: Option<&str>,
) -> Result<Vec<DependencyPackage>> {
    let manifest = read_json_file(&path.join("composer.json"))?;
    let lock = read_optional_json_file(&path.join("composer.lock"))?;
    let mut direct = BTreeMap::new();

    collect_manifest_section(&manifest, "require", &mut direct);
    collect_manifest_section(&manifest, "require-dev", &mut direct);

    let locked = lock
        .as_ref()
        .map(collect_locked_packages)
        .unwrap_or_default();

    let mut names: BTreeSet<String> = direct.keys().cloned().collect();
    names.extend(locked.keys().cloned());

    let packages = names
        .into_iter()
        .filter(|name| package_filter.map(|filter| filter == name).unwrap_or(true))
        .map(|name| {
            let (manifest_section, constraint) = direct
                .get(&name)
                .cloned()
                .map(|(section, constraint)| (Some(section), Some(constraint)))
                .unwrap_or((None, None));
            let locked = locked.get(&name);
            DependencyPackage {
                name,
                manifest_section,
                constraint,
                locked_version: locked.and_then(|p| p.version.clone()),
                locked_reference: locked.and_then(|p| p.reference.clone()),
            }
        })
        .collect();

    Ok(packages)
}

fn collect_manifest_section(
    manifest: &Value,
    section: &str,
    direct: &mut BTreeMap<String, (String, String)>,
) {
    let Some(entries) = manifest.get(section).and_then(Value::as_object) else {
        return;
    };

    for (name, constraint) in entries {
        if name == "php" || name.starts_with("ext-") {
            continue;
        }
        if let Some(constraint) = constraint.as_str() {
            direct.insert(name.clone(), (section.to_string(), constraint.to_string()));
        }
    }
}

#[derive(Debug, Clone, Default)]
struct LockedPackage {
    version: Option<String>,
    reference: Option<String>,
}

fn collect_locked_packages(lock: &Value) -> BTreeMap<String, LockedPackage> {
    let mut packages = BTreeMap::new();

    for section in ["packages", "packages-dev"] {
        let Some(entries) = lock.get(section).and_then(Value::as_array) else {
            continue;
        };

        for entry in entries {
            let Some(name) = entry.get("name").and_then(Value::as_str) else {
                continue;
            };
            let version = entry
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_string);
            let reference = entry
                .get("source")
                .and_then(|source| source.get("reference"))
                .or_else(|| entry.get("dist").and_then(|dist| dist.get("reference")))
                .and_then(Value::as_str)
                .map(str::to_string);

            packages.insert(name.to_string(), LockedPackage { version, reference });
        }
    }

    packages
}

fn read_json_file(path: &Path) -> Result<Value> {
    let raw = fs::read_to_string(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&raw)
        .map_err(|e| Error::validation_invalid_json(e, Some(path.display().to_string()), Some(raw)))
}

fn read_optional_json_file(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    read_json_file(path).map(Some)
}

fn first_non_empty_line(output: &str) -> Option<&str> {
    output.lines().find(|line| !line.trim().is_empty())
}
