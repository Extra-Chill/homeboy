use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::error::{Error, Result};
use crate::core::extension::{
    self, CiJobFidelity, CiJobMapping, CiJobSpec, CiLocalContext, CiProfileSpec, ExtensionManifest,
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
    #[serde(flatten)]
    pub mapping: CiJobMapping,
    pub command: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(flatten)]
    pub local_context: CiLocalContext,
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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CiRunSelection {
    Job(String),
    Profile(String),
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CiRunOutput {
    pub extension_id: String,
    pub selection: CiRunSelection,
    pub jobs: Vec<CiJobRunOutput>,
    pub success: bool,
    pub exit_code: i32,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CiJobRunOutput {
    pub id: String,
    pub ci_context: CiContext,
    pub command: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    pub success: bool,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CiResolvedJob {
    pub extension_id: String,
    pub id: String,
    pub spec: CiJobSpec,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CiContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub job_id: String,
    #[serde(flatten)]
    pub mapping: CiJobMapping,
    #[serde(flatten)]
    pub local_context: CiLocalContext,
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

pub fn run_for_extension(
    source_path: &Path,
    extension_id: &str,
    selection: CiRunSelection,
) -> Result<CiRunOutput> {
    let extension = extension::load_extension(extension_id)?;
    run_from_manifest(source_path, extension_id, &extension, selection)
}

pub fn resolve_job_for_extension(extension_id: &str, job_id: &str) -> Result<CiResolvedJob> {
    let extension = extension::load_extension(extension_id)?;
    let ci = extension.ci.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "extension",
            format!("extension '{extension_id}' does not declare CI jobs"),
            Some(extension_id.to_string()),
            None,
        )
    })?;
    let spec = ci.jobs.get(job_id).ok_or_else(|| {
        Error::validation_invalid_argument(
            "job",
            format!("CI job '{job_id}' is not declared by extension '{extension_id}'"),
            Some(job_id.to_string()),
            Some(ci.jobs.keys().cloned().collect()),
        )
    })?;

    Ok(CiResolvedJob {
        extension_id: extension_id.to_string(),
        id: job_id.to_string(),
        spec: spec.clone(),
    })
}

pub fn resolve_profile_jobs_for_extension(
    extension_id: &str,
    profile_id: &str,
) -> Result<Vec<CiResolvedJob>> {
    let extension = extension::load_extension(extension_id)?;
    let ci = extension.ci.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "extension",
            format!("extension '{extension_id}' does not declare CI jobs"),
            Some(extension_id.to_string()),
            None,
        )
    })?;
    let profile = ci.profiles.get(profile_id).ok_or_else(|| {
        Error::validation_invalid_argument(
            "profile",
            format!("CI profile '{profile_id}' is not declared by extension '{extension_id}'"),
            Some(profile_id.to_string()),
            Some(ci.profiles.keys().cloned().collect()),
        )
    })?;

    profile
        .jobs
        .iter()
        .map(|job_id| {
            let spec = ci.jobs.get(job_id).ok_or_else(|| {
                Error::validation_invalid_argument(
                    "job",
                    format!("CI job '{job_id}' is not declared by extension '{extension_id}'"),
                    Some(job_id.clone()),
                    Some(ci.jobs.keys().cloned().collect()),
                )
            })?;
            Ok(CiResolvedJob {
                extension_id: extension_id.to_string(),
                id: job_id.clone(),
                spec: spec.clone(),
            })
        })
        .collect()
}

pub fn validate_single_profile_command(
    profile_id: &str,
    jobs: &[CiResolvedJob],
    expected_command: &str,
) -> Result<()> {
    if jobs.len() != 1 {
        return Err(Error::validation_invalid_argument(
            "ci-profile",
            format!(
                "CI profile '{profile_id}' declares {} jobs; this command requires exactly one '{expected_command}' job",
                jobs.len()
            ),
            Some(profile_id.to_string()),
            None,
        ));
    }

    validate_job_command(&jobs[0], expected_command)
}

pub fn validate_job_command(job: &CiResolvedJob, expected_command: &str) -> Result<()> {
    if job.spec.command == expected_command {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "ci-job",
        format!(
            "CI job '{}' declares command '{}', but this command only runs '{}' jobs",
            job.id, job.spec.command, expected_command
        ),
        Some(job.id.clone()),
        Some(vec![expected_command.to_string()]),
    ))
}

pub fn ci_job_env(job: Option<&CiResolvedJob>) -> Vec<(String, String)> {
    job.map(|job| {
        job.spec
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    })
    .unwrap_or_default()
}

pub fn ci_context_for_job(job: Option<&CiResolvedJob>, profile: Option<&str>) -> Option<CiContext> {
    job.map(|job| ci_context(&job.id, &job.spec, profile))
}

pub fn run_from_manifest(
    source_path: &Path,
    extension_id: &str,
    extension: &ExtensionManifest,
    selection: CiRunSelection,
) -> Result<CiRunOutput> {
    let ci = extension.ci.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "extension",
            format!("extension '{extension_id}' does not declare CI jobs"),
            Some(extension_id.to_string()),
            None,
        )
    })?;

    let profile_id = match &selection {
        CiRunSelection::Profile(profile_id) => Some(profile_id.clone()),
        CiRunSelection::Job(_) => None,
    };

    let job_ids = match &selection {
        CiRunSelection::Job(job_id) => vec![job_id.clone()],
        CiRunSelection::Profile(profile_id) => ci
            .profiles
            .get(profile_id)
            .map(|profile| profile.jobs.clone())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "profile",
                    format!(
                        "CI profile '{profile_id}' is not declared by extension '{extension_id}'"
                    ),
                    Some(profile_id.clone()),
                    Some(ci.profiles.keys().cloned().collect()),
                )
            })?,
    };

    if job_ids.is_empty() {
        return Err(Error::validation_invalid_argument(
            "profile",
            "CI profile does not declare any jobs",
            match &selection {
                CiRunSelection::Profile(profile_id) => Some(profile_id.clone()),
                CiRunSelection::Job(job_id) => Some(job_id.clone()),
            },
            None,
        ));
    }

    let mut jobs = Vec::new();
    for job_id in job_ids {
        let job = ci.jobs.get(&job_id).ok_or_else(|| {
            Error::validation_invalid_argument(
                "job",
                format!("CI job '{job_id}' is not declared by extension '{extension_id}'"),
                Some(job_id.clone()),
                Some(ci.jobs.keys().cloned().collect()),
            )
        })?;
        jobs.push(run_job(source_path, &job_id, job, profile_id.as_deref())?);
    }

    let success = jobs.iter().all(|job| job.success);
    Ok(CiRunOutput {
        extension_id: extension_id.to_string(),
        selection,
        exit_code: if success { 0 } else { 1 },
        success,
        jobs,
    })
}

fn run_job(
    source_path: &Path,
    job_id: &str,
    job: &CiJobSpec,
    profile: Option<&str>,
) -> Result<CiJobRunOutput> {
    let output = Command::new(&job.command)
        .args(&job.args)
        .envs(&job.env)
        .current_dir(source_path)
        .output()
        .map_err(|error| {
            Error::internal_io(
                format!("Failed to run CI job '{job_id}': {error}"),
                Some("ci.run".to_string()),
            )
        })?;

    Ok(CiJobRunOutput {
        id: job_id.to_string(),
        ci_context: ci_context(job_id, job, profile),
        command: job.command.clone(),
        args: job.args.clone(),
        success: output.status.success(),
        exit_code: output.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn ci_context(job_id: &str, job: &CiJobSpec, profile: Option<&str>) -> CiContext {
    CiContext {
        profile: profile.map(ToString::to_string),
        job_id: job_id.to_string(),
        mapping: job.mapping.clone(),
        local_context: job.local_context.clone(),
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
            mapping: job.mapping.clone(),
            command: job.command.clone(),
            args: job.args.clone(),
            env: job.env.clone(),
            local_context: job.local_context.clone(),
        })
        .collect()
}

fn discover_ci_jobs(source_path: &Path) -> Vec<DiscoveredCiJob> {
    let mut jobs = Vec::new();
    jobs.extend(discover_github_workflows(source_path));
    jobs.extend(discover_buildkite_pipelines(source_path));
    jobs
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
        let inventory = list_from_manifest(tmp.path(), "fixture-ci", &manifest_with_ci());

        assert_eq!(inventory.extension_id, "fixture-ci");
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
            extension.id = "fixture-ci".to_string();
            crate::core::extension::save_manifest(&extension).expect("save extension");

            let inventory = list_for_extension(home.path(), "fixture-ci").expect("list inventory");

            assert_eq!(inventory.extension_id, "fixture-ci");
            assert_eq!(inventory.profiles[0].id, "pr");
        });
    }

    #[test]
    fn discovery_lists_ci_files_as_unknown() {
        let tmp = TempDir::new().expect("temp dir");
        fs::create_dir_all(tmp.path().join(".github/workflows")).expect("mkdir workflows");
        fs::write(tmp.path().join(".github/workflows/ci.yml"), "name: CI").expect("write ci");

        let discovered = discover_ci_jobs(tmp.path());

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
            select_extension_id(&["fixture-ci".to_string()]).unwrap(),
            "fixture-ci"
        );
        assert!(select_extension_id(&[]).is_err());
        assert!(select_extension_id(&["fixture-a".to_string(), "fixture-b".to_string()]).is_err());
    }

    #[test]
    fn empty_ci_manifest_defaults() {
        let ci = CiCapability::default();

        assert!(ci.profiles.is_empty());
        assert!(ci.jobs.is_empty());
    }

    #[test]
    fn run_from_manifest_executes_declared_job() {
        let tmp = TempDir::new().expect("temp dir");
        let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": "Test",
            "version": "1.0.0",
            "ci": {
                "jobs": {
                    "smoke": {
                        "command": "sh",
                        "args": ["-c", "printf '%s' \"$CI_MESSAGE\""],
                        "env": { "CI_MESSAGE": "ok" },
                        "fidelity": "local-equivalent"
                    }
                }
            }
        }))
        .expect("parse manifest");

        let output = run_from_manifest(
            tmp.path(),
            "test",
            &manifest,
            CiRunSelection::Job("smoke".to_string()),
        )
        .expect("run job");

        assert!(output.success);
        assert_eq!(output.exit_code, 0);
        assert_eq!(output.jobs[0].stdout, "ok");
    }

    #[test]
    fn test_run_for_extension() {
        crate::test_support::with_isolated_home(|_home| {
            let tmp = TempDir::new().expect("temp dir");
            let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
                "id": "test",
                "name": "Test",
                "version": "1.0.0",
                "ci": {
                    "jobs": {
                        "smoke": { "command": "sh", "args": ["-c", "printf smoke"] }
                    }
                }
            }))
            .expect("parse manifest");
            crate::core::extension::save_manifest(&manifest).expect("save extension");

            let output =
                run_for_extension(tmp.path(), "test", CiRunSelection::Job("smoke".to_string()))
                    .expect("run extension job");

            assert!(output.success);
            assert_eq!(output.jobs[0].stdout, "smoke");
        });
    }

    #[test]
    fn test_resolve_job_for_extension() {
        crate::test_support::with_isolated_home(|_home| {
            let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
                "id": "test",
                "name": "Test",
                "version": "1.0.0",
                "ci": {
                    "jobs": {
                        "unit": {
                            "command": "test",
                            "args": ["--unit"],
                            "env": { "CI": "1" }
                        }
                    }
                }
            }))
            .expect("parse manifest");
            crate::core::extension::save_manifest(&manifest).expect("save extension");

            let job = resolve_job_for_extension("test", "unit").expect("resolve CI job");

            assert_eq!(job.extension_id, "test");
            assert_eq!(job.id, "unit");
            assert_eq!(job.spec.command, "test");
            assert_eq!(job.spec.args, vec!["--unit"]);
            assert_eq!(job.spec.env.get("CI").map(String::as_str), Some("1"));
        });
    }

    #[test]
    fn test_resolve_profile_jobs_for_extension() {
        crate::test_support::with_isolated_home(|_home| {
            let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
                "id": "test",
                "name": "Test",
                "version": "1.0.0",
                "ci": {
                    "profiles": {
                        "perf": { "jobs": ["bench"] }
                    },
                    "jobs": {
                        "bench": { "command": "bench", "args": ["--fast"] }
                    }
                }
            }))
            .expect("parse manifest");
            crate::core::extension::save_manifest(&manifest).expect("save extension");

            let jobs = resolve_profile_jobs_for_extension("test", "perf")
                .expect("resolve CI profile jobs");

            assert_eq!(jobs.len(), 1);
            assert_eq!(jobs[0].id, "bench");
            assert_eq!(jobs[0].spec.args, vec!["--fast"]);
        });
    }

    #[test]
    fn test_validate_single_profile_command() {
        let job = CiResolvedJob {
            extension_id: "test".to_string(),
            id: "bench".to_string(),
            spec: CiJobSpec {
                command: "bench".to_string(),
                ..CiJobSpec::default()
            },
        };

        assert!(validate_single_profile_command("perf", &[job.clone()], "bench").is_ok());
        assert!(validate_single_profile_command("perf", &[job], "test").is_err());
        assert!(validate_single_profile_command("perf", &[], "bench").is_err());
    }

    #[test]
    fn test_validate_job_command() {
        let job = CiResolvedJob {
            extension_id: "test".to_string(),
            id: "unit".to_string(),
            spec: CiJobSpec {
                command: "test".to_string(),
                ..CiJobSpec::default()
            },
        };

        assert!(validate_job_command(&job, "test").is_ok());
        assert!(validate_job_command(&job, "lint").is_err());
    }

    #[test]
    fn test_ci_job_env() {
        let job = CiResolvedJob {
            extension_id: "test".to_string(),
            id: "lint".to_string(),
            spec: CiJobSpec {
                command: "lint".to_string(),
                env: BTreeMap::from([("CI".to_string(), "1".to_string())]),
                ..CiJobSpec::default()
            },
        };

        assert_eq!(
            ci_job_env(Some(&job)),
            vec![("CI".to_string(), "1".to_string())]
        );
        assert!(ci_job_env(None).is_empty());
    }

    #[test]
    fn test_ci_context_for_job() {
        let job = CiResolvedJob {
            extension_id: "test".to_string(),
            id: "unit".to_string(),
            spec: CiJobSpec {
                mapping: CiJobMapping {
                    check_names: vec!["Unit tests".to_string()],
                    provider: Some("github-actions".to_string()),
                    workflow: Some("ci.yml".to_string()),
                },
                command: "test".to_string(),
                local_context: CiLocalContext {
                    fidelity: CiJobFidelity::LocalPartial,
                    limitations: vec!["matrix shard not reproduced".to_string()],
                },
                ..CiJobSpec::default()
            },
        };

        let context = ci_context_for_job(Some(&job), Some("pr")).expect("context");

        assert_eq!(context.profile.as_deref(), Some("pr"));
        assert_eq!(context.job_id, "unit");
        assert_eq!(context.mapping.check_names, vec!["Unit tests"]);
        assert_eq!(context.mapping.provider.as_deref(), Some("github-actions"));
        assert_eq!(context.mapping.workflow.as_deref(), Some("ci.yml"));
        assert_eq!(context.local_context.fidelity, CiJobFidelity::LocalPartial);
        assert_eq!(context.local_context.limitations.len(), 1);
    }

    #[test]
    fn run_from_manifest_executes_profile_jobs() {
        let tmp = TempDir::new().expect("temp dir");
        let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": "Test",
            "version": "1.0.0",
            "ci": {
                "profiles": {
                    "pr": { "jobs": ["first", "second"] }
                },
                "jobs": {
                    "first": { "command": "sh", "args": ["-c", "printf first"] },
                    "second": { "command": "sh", "args": ["-c", "printf second"] }
                }
            }
        }))
        .expect("parse manifest");

        let output = run_from_manifest(
            tmp.path(),
            "test",
            &manifest,
            CiRunSelection::Profile("pr".to_string()),
        )
        .expect("run profile");

        assert!(output.success);
        assert_eq!(output.jobs.len(), 2);
        assert_eq!(output.jobs[0].ci_context.profile.as_deref(), Some("pr"));
        assert_eq!(output.jobs[0].ci_context.job_id, "first");
        assert_eq!(output.jobs[0].stdout, "first");
        assert_eq!(output.jobs[1].stdout, "second");
    }

    #[test]
    fn run_from_manifest_reports_failed_job_as_output() {
        let tmp = TempDir::new().expect("temp dir");
        let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": "Test",
            "version": "1.0.0",
            "ci": {
                "jobs": {
                    "fail": { "command": "sh", "args": ["-c", "printf nope >&2; exit 7"] }
                }
            }
        }))
        .expect("parse manifest");

        let output = run_from_manifest(
            tmp.path(),
            "test",
            &manifest,
            CiRunSelection::Job("fail".to_string()),
        )
        .expect("run job");

        assert!(!output.success);
        assert_eq!(output.exit_code, 1);
        assert_eq!(output.jobs[0].exit_code, 7);
        assert_eq!(output.jobs[0].stderr, "nope");
    }
}
