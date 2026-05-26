use std::path::Path;

use crate::core::code_audit::conventions::AuditFinding;
use crate::core::code_audit::findings::{Finding, Severity};
use crate::core::component::ArtifactPortabilityConfig;
use crate::core::observation::{ObservationStore, RunListFilter};

pub(crate) fn run(component_id: &str) -> Vec<Finding> {
    run_with_config(component_id, &ArtifactPortabilityConfig::default())
}

pub(crate) fn run_with_config(
    component_id: &str,
    config: &ArtifactPortabilityConfig,
) -> Vec<Finding> {
    let Ok(store) = ObservationStore::open_initialized() else {
        return Vec::new();
    };
    let Ok(runs) = store.list_runs(RunListFilter {
        kind: None,
        component_id: Some(component_id.to_string()),
        status: None,
        rig_id: None,
        limit: Some(1000),
    }) else {
        return Vec::new();
    };
    let artifact_root = crate::core::artifact_root().ok();
    let path_policy = config.with_generic_defaults();
    let mut findings = Vec::new();

    for run in runs {
        let Ok(artifacts) = store.list_artifacts(&run.id) else {
            continue;
        };
        for artifact in artifacts {
            if artifact.artifact_type != "file" && artifact.artifact_type != "directory" {
                continue;
            }
            if artifact_path_is_portable(&artifact.path, artifact_root.as_deref(), &path_policy) {
                continue;
            }
            findings.push(Finding {
                convention: "artifact_portability".to_string(),
                severity: Severity::Warning,
                file: format!("observation:{}", run.id),
                description: format!(
                    "Artifact {} ({}) records non-portable path {}",
                    artifact.id, artifact.kind, artifact.path
                ),
                suggestion: "Record copied artifact-store paths or portable artifact tokens instead of runtime temp paths".to_string(),
                kind: AuditFinding::NonPortableArtifactPath,
            });
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

fn artifact_path_is_portable(
    path: &str,
    artifact_root: Option<&Path>,
    config: &ArtifactPortabilityConfig,
) -> bool {
    if path.starts_with("runner-artifact://") || path.starts_with("metadata-only:") {
        return true;
    }
    let path_ref = Path::new(path);
    if let Some(root) = artifact_root {
        if path_ref.starts_with(root) {
            return true;
        }
    }
    !looks_like_runtime_temp_path(path, config)
}

fn looks_like_runtime_temp_path(path: &str, config: &ArtifactPortabilityConfig) -> bool {
    config
        .non_portable_path_prefixes
        .iter()
        .any(|prefix| path.starts_with(prefix))
        || config
            .non_portable_path_contains
            .iter()
            .any(|marker| path.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::observation::{ArtifactRecord, NewRunRecord};
    use crate::test_support::with_isolated_home;

    #[test]
    fn flags_runtime_temp_artifact_paths() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let cwd = std::env::current_dir().expect("cwd");
            let run_record = store
                .start_run(
                    NewRunRecord::builder("observe")
                        .component_id("demo")
                        .command("homeboy observe demo")
                        .cwd_path(&cwd)
                        .build(),
                )
                .expect("run");
            store
                .import_artifact(&ArtifactRecord {
                    id: "artifact-1".to_string(),
                    run_id: run_record.id,
                    kind: "trace-results".to_string(),
                    artifact_type: "file".to_string(),
                    path: "/tmp/homeboy-run-abc/trace.json".to_string(),
                    url: None,
                    sha256: None,
                    size_bytes: None,
                    mime: None,
                    created_at: chrono::Utc::now().to_rfc3339(),
                })
                .expect("artifact");

            let findings = super::run("demo");

            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].kind, AuditFinding::NonPortableArtifactPath);
            assert!(findings[0]
                .description
                .contains("/tmp/homeboy-run-abc/trace.json"));
        });
    }

    #[test]
    fn accepts_artifact_store_paths() {
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            crate::core::set_artifact_root_override(Some(artifact_root.clone()));

            assert!(artifact_path_is_portable(
                &artifact_root.join("run/artifact.json").to_string_lossy(),
                Some(&artifact_root),
                &ArtifactPortabilityConfig::default().with_generic_defaults()
            ));
        });
    }

    #[test]
    fn accepts_project_specific_path_markers_from_config() {
        let config = ArtifactPortabilityConfig {
            non_portable_path_contains: vec!["/project-run-".to_string()],
            ..Default::default()
        }
        .with_generic_defaults();

        assert!(!artifact_path_is_portable(
            "/workspace/project-run-abc/trace.json",
            None,
            &config
        ));
        assert!(artifact_path_is_portable(
            "/workspace/other-run-abc/trace.json",
            None,
            &config
        ));
    }

    #[test]
    fn homeboy_config_declares_homeboy_run_marker() {
        let raw = std::fs::read_to_string("homeboy.json").expect("homeboy config");
        let component: crate::core::component::Component =
            serde_json::from_str(&raw).expect("component config");
        let audit = component.audit.expect("audit config");

        assert!(audit
            .artifact_portability
            .non_portable_path_contains
            .contains(&"/homeboy-run-".to_string()));
    }
}
