use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::core::error::{Error, Result};
use crate::core::extension::{
    self, CiJobFidelity, CiJobSpec, CiLocalContext, CiProfileSpec, ExtensionManifest,
};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CiInventory {
    pub extension_id: String,
    pub profiles: Vec<CiProfileSummary>,
    pub jobs: Vec<CiJobSummary>,
    pub discovered_jobs: Vec<DiscoveredCiJob>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CiProfileSummary {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub jobs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CiJobSummary {
    pub id: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub check_names: Vec<String>,
    pub command: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(flatten)]
    pub local_context: CiLocalContext,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DiscoveredCiJob {
    pub id: String,
    pub provider: String,
    pub label: String,
    #[serde(flatten)]
    pub local_context: CiLocalContext,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
}

pub fn list_for_extension(source_path: &Path, extension_id: &str) -> Result<CiInventory> {
    let extension = extension::load_extension(extension_id)?;
    Ok(list_from_manifest(source_path, extension_id, &extension))
}

pub fn list_from_manifest(
    source_path: &Path,
    extension_id: &str,
    extension: &ExtensionManifest,
) -> CiInventory {
    let (profiles, jobs) = extension
        .ci
        .as_ref()
        .map(|ci| (profile_summaries(&ci.profiles), job_summaries(&ci.jobs)))
        .unwrap_or_default();

    CiInventory {
        extension_id: extension_id.to_string(),
        profiles,
        jobs,
        discovered_jobs: discover_ci_jobs(source_path),
    }
}

pub fn select_extension_id(extension_ids: &[String]) -> Result<String> {
    match extension_ids {
        [extension_id] => Ok(extension_id.clone()),
        [] => Err(Error::validation_missing_argument(vec![
            "--extension <ID> or component extensions".to_string(),
        ])),
        _ => Err(Error::validation_invalid_argument(
            "extension",
            "ci list needs exactly one extension when multiple component extensions are configured",
            Some(extension_ids.join(", ")),
            Some(vec![
                "Pass --extension <ID> to choose the CI profile source.".to_string(),
            ]),
        )),
    }
}

fn profile_summaries(profiles: &BTreeMap<String, CiProfileSpec>) -> Vec<CiProfileSummary> {
    profiles
        .iter()
        .map(|(id, profile)| CiProfileSummary {
            id: id.clone(),
            label: profile.label.clone(),
            jobs: profile.jobs.clone(),
        })
        .collect()
}

fn job_summaries(jobs: &BTreeMap<String, CiJobSpec>) -> Vec<CiJobSummary> {
    jobs.iter()
        .map(|(id, job)| CiJobSummary {
            id: id.clone(),
            check_names: job.check_names.clone(),
            command: job.command.clone(),
            args: job.args.clone(),
            env: job.env.clone(),
            local_context: job.local_context.clone(),
            provider: job.provider.clone(),
            workflow: job.workflow.clone(),
        })
        .collect()
}

fn discover_ci_jobs(source_path: &Path) -> Vec<DiscoveredCiJob> {
    let mut jobs = Vec::new();
    jobs.extend(discover_package_scripts(source_path));
    jobs.extend(discover_github_workflows(source_path));
    jobs.extend(discover_buildkite_pipelines(source_path));
    jobs
}

fn discover_package_scripts(source_path: &Path) -> Vec<DiscoveredCiJob> {
    let package_json = source_path.join("package.json");
    let Ok(raw) = fs::read_to_string(package_json) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let Some(scripts) = value.get("scripts").and_then(|scripts| scripts.as_object()) else {
        return Vec::new();
    };

    scripts
        .keys()
        .map(|script| DiscoveredCiJob {
            id: format!("package-script:{script}"),
            provider: "package-scripts".to_string(),
            label: script.clone(),
            local_context: unknown_discovered_context(
                "Discovered from package metadata; no Homeboy profile maps this to a local CI-equivalent command yet.",
            ),
            workflow: None,
        })
        .collect()
}

fn discover_github_workflows(source_path: &Path) -> Vec<DiscoveredCiJob> {
    discover_named_files(
        source_path.join(".github/workflows"),
        "github-actions",
        "workflow",
        &["yml", "yaml"],
    )
}

fn discover_buildkite_pipelines(source_path: &Path) -> Vec<DiscoveredCiJob> {
    discover_named_files(
        source_path.join(".buildkite"),
        "buildkite",
        "pipeline",
        &["yml", "yaml"],
    )
}

fn discover_named_files(
    dir: PathBuf,
    provider: &str,
    kind: &str,
    extensions: &[&str],
) -> Vec<DiscoveredCiJob> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut jobs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        if !extensions.contains(&extension) {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        jobs.push(DiscoveredCiJob {
            id: format!("{provider}:{file_name}"),
            provider: provider.to_string(),
            label: file_name.to_string(),
            local_context: unknown_discovered_context(&format!(
                "Discovered {kind} file only; Homeboy does not infer runnable local equivalence from arbitrary CI YAML."
            )),
            workflow: Some(file_name.to_string()),
        });
    }
    jobs.sort_by(|a, b| a.id.cmp(&b.id));
    jobs
}

fn unknown_discovered_context(limitation: &str) -> CiLocalContext {
    CiLocalContext {
        fidelity: CiJobFidelity::Unknown,
        limitations: vec![limitation.to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extension::{CiCapability, ExtensionManifest};
    use tempfile::TempDir;

    fn manifest_with_ci() -> ExtensionManifest {
        serde_json::from_value(serde_json::json!({
            "name": "Node.js",
            "version": "1.0.0",
            "ci": {
                "profiles": {
                    "pr": { "label": "Pull request", "jobs": ["lint"] }
                },
                "jobs": {
                    "lint": {
                        "check_names": ["Lint and typecheck"],
                        "command": "lint",
                        "args": ["--fix=false"],
                        "env": { "CI": "1" },
                        "fidelity": "local-equivalent",
                        "provider": "github-actions",
                        "workflow": "ci.yml"
                    }
                }
            }
        }))
        .expect("parse manifest")
    }

    #[test]
    fn list_from_manifest_returns_declared_profiles_and_jobs() {
        let tmp = TempDir::new().expect("temp dir");
        let inventory = list_from_manifest(tmp.path(), "nodejs", &manifest_with_ci());

        assert_eq!(inventory.extension_id, "nodejs");
        assert_eq!(inventory.profiles[0].id, "pr");
        assert_eq!(inventory.jobs[0].id, "lint");
        assert_eq!(
            inventory.jobs[0].local_context.fidelity,
            CiJobFidelity::LocalEquivalent
        );
    }

    #[test]
    fn test_list_for_extension() {
        crate::test_support::with_isolated_home(|home| {
            let mut extension = manifest_with_ci();
            extension.id = "nodejs".to_string();
            crate::core::extension::save_manifest(&extension).expect("save extension");

            let inventory = list_for_extension(home.path(), "nodejs").expect("list inventory");

            assert_eq!(inventory.extension_id, "nodejs");
            assert_eq!(inventory.profiles[0].id, "pr");
        });
    }

    #[test]
    fn discovery_lists_package_scripts_and_ci_files_as_unknown() {
        let tmp = TempDir::new().expect("temp dir");
        fs::write(
            tmp.path().join("package.json"),
            r#"{ "scripts": { "test": "vitest", "lint": "eslint ." } }"#,
        )
        .expect("write package");
        fs::create_dir_all(tmp.path().join(".github/workflows")).expect("mkdir workflows");
        fs::write(tmp.path().join(".github/workflows/ci.yml"), "name: CI").expect("write ci");

        let discovered = discover_ci_jobs(tmp.path());

        assert!(discovered.iter().any(|job| job.id == "package-script:test"));
        assert!(discovered
            .iter()
            .any(|job| job.id == "github-actions:ci.yml"));
        assert!(discovered
            .iter()
            .all(|job| job.local_context.fidelity == CiJobFidelity::Unknown));
    }

    #[test]
    fn select_extension_id_requires_one_extension() {
        assert_eq!(
            select_extension_id(&["nodejs".to_string()]).unwrap(),
            "nodejs"
        );
        assert!(select_extension_id(&[]).is_err());
        assert!(select_extension_id(&["nodejs".to_string(), "rust".to_string()]).is_err());
    }

    #[test]
    fn empty_ci_manifest_defaults() {
        let ci = CiCapability::default();

        assert!(ci.profiles.is_empty());
        assert!(ci.jobs.is_empty());
    }
}
