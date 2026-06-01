//! Remote execution preflight detector.
//!
//! Flags remote execution dispatch sites that can hand local paths or artifacts
//! to another execution environment without an explicit translation/mirror
//! contract.

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;
use crate::core::component::audit::RemoteExecutionSafetyConfig;

pub(in crate::core::code_audit) fn run(
    fingerprints: &[&FileFingerprint],
    config: &RemoteExecutionSafetyConfig,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let policy = RemoteExecutionSafetyPolicy::new(config);

    for fp in fingerprints {
        if super::walker::is_test_path(&fp.relative_path)
            || fp.relative_path == "src/core/code_audit/detectors/remote_execution_preflight.rs"
            || !is_remote_dispatch_file(&fp.content, &policy)
        {
            continue;
        }

        if forwards_args_without_path_preflight(&fp.content, &policy) {
            findings.push(finding(
                fp,
                "Remote execution dispatch builds command argv from caller args without an explicit path-translation preflight.",
                "Route argv through a rewrite/preflight function that strips local-only wrapper flags and translates or rejects local filesystem paths before remote dispatch.",
            ));
        }

        if captures_artifacts_without_snapshot(&fp.content, &policy) {
            findings.push(finding(
                fp,
                "Remote execution dispatch can request artifact capture without a source snapshot or mirror contract.",
                "Attach a source snapshot or equivalent mirror verification contract before reporting remote artifacts as run evidence.",
            ));
        }

        if dispatches_without_capability_preflight(&fp.content, &policy) {
            findings.push(finding(
                fp,
                "Remote execution dispatch starts without validating remote capability parity first.",
                "Check remote execution capabilities before dispatch so missing tools, components, or environment requirements fail before remote execution starts.",
            ));
        }

        if dispatches_extension_without_parity_preflight(&fp.content, &policy) {
            findings.push(finding(
                fp,
                "Remote execution dispatch accepts an extension selector without validating remote extension parity before execution.",
                "Add a pre-dispatch extension parity check so missing remote-side extension support fails before command execution.",
            ));
        }

        if reports_remote_artifact_without_access_check(&fp.content, &policy) {
            findings.push(finding(
                fp,
                "Remote artifact reporting does not prove the reported artifact path is locally accessible or retrievable.",
                "Verify mirrored artifact paths with local metadata or a retrievable remote-artifact token before exposing them as run evidence.",
            ));
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings.dedup_by(|a, b| a.file == b.file && a.description == b.description);
    findings
}

struct RemoteExecutionSafetyPolicy<'a> {
    dispatch_markers: Vec<&'a str>,
    path_translation_markers: Vec<&'a str>,
    capability_preflight_markers: Vec<&'a str>,
    artifact_capture_markers: Vec<&'a str>,
    artifact_snapshot_markers: Vec<&'a str>,
    extension_parity_markers: Vec<&'a str>,
    extension_selector_markers: Vec<&'a str>,
    artifact_report_markers: Vec<&'a str>,
    artifact_access_markers: Vec<&'a str>,
}

impl<'a> RemoteExecutionSafetyPolicy<'a> {
    fn new(config: &'a RemoteExecutionSafetyConfig) -> Self {
        Self {
            dispatch_markers: configured(&config.dispatch_markers),
            path_translation_markers: configured(&config.path_translation_markers),
            capability_preflight_markers: configured(&config.capability_preflight_markers),
            artifact_capture_markers: configured(&config.artifact_capture_markers),
            artifact_snapshot_markers: configured(&config.artifact_snapshot_markers),
            extension_parity_markers: configured(&config.extension_parity_markers),
            extension_selector_markers: configured(&config.extension_selector_markers),
            artifact_report_markers: configured(&config.artifact_report_markers),
            artifact_access_markers: configured(&config.artifact_access_markers),
        }
    }
}

fn configured(configured: &[String]) -> Vec<&str> {
    configured.iter().map(String::as_str).collect()
}

fn is_remote_dispatch_file(content: &str, policy: &RemoteExecutionSafetyPolicy) -> bool {
    contains_any(content, &policy.dispatch_markers)
        || contains_any(content, &policy.artifact_report_markers)
}

fn forwards_args_without_path_preflight(
    content: &str,
    policy: &RemoteExecutionSafetyPolicy,
) -> bool {
    let forwards_caller_args = contains_any(
        content,
        &[
            "normalized_args",
            "std::env::args",
            "args.iter()",
            "command.extend(args",
        ],
    );
    let has_path_preflight = contains_any(content, &policy.path_translation_markers);

    forwards_caller_args && !has_path_preflight
}

fn captures_artifacts_without_snapshot(
    content: &str,
    policy: &RemoteExecutionSafetyPolicy,
) -> bool {
    contains_any(content, &policy.artifact_capture_markers)
        && !contains_any(content, &policy.artifact_snapshot_markers)
}

fn dispatches_without_capability_preflight(
    content: &str,
    policy: &RemoteExecutionSafetyPolicy,
) -> bool {
    let dispatches_remote_work = contains_any(content, &policy.dispatch_markers);
    let has_capability_preflight = contains_any(content, &policy.capability_preflight_markers);

    dispatches_remote_work && !has_capability_preflight
}

fn dispatches_extension_without_parity_preflight(
    content: &str,
    policy: &RemoteExecutionSafetyPolicy,
) -> bool {
    let accepts_extension = contains_any(content, &policy.extension_selector_markers);
    let has_parity_preflight = contains_any(content, &policy.extension_parity_markers);

    accepts_extension && !has_parity_preflight
}

fn reports_remote_artifact_without_access_check(
    content: &str,
    policy: &RemoteExecutionSafetyPolicy,
) -> bool {
    let reports_remote_artifact = contains_any(content, &policy.artifact_report_markers);
    let verifies_access = contains_any(content, &policy.artifact_access_markers);

    reports_remote_artifact && !verifies_access
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn finding(fp: &FileFingerprint, description: &str, suggestion: &str) -> Finding {
    Finding {
        convention: "remote_execution_preflight".to_string(),
        severity: Severity::Warning,
        file: fp.relative_path.clone(),
        description: description.to_string(),
        suggestion: suggestion.to_string(),
        kind: AuditFinding::RemoteExecutionPreflight,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::code_audit::conventions::Language;

    fn fingerprint(path: &str, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Rust,
            content: content.to_string(),
            ..Default::default()
        }
    }

    fn policy_config() -> RemoteExecutionSafetyConfig {
        RemoteExecutionSafetyConfig {
            dispatch_markers: vec![
                "execute_remote_work".to_string(),
                "RemoteWorkOptions".to_string(),
            ],
            path_translation_markers: vec!["translate_remote_args".to_string()],
            capability_preflight_markers: vec![
                "remote_capability_plan".to_string(),
                "evaluate_remote_capabilities".to_string(),
            ],
            artifact_capture_markers: vec!["capture_change: true".to_string()],
            artifact_snapshot_markers: vec!["source_snapshot".to_string()],
            extension_selector_markers: vec!["extension_selector".to_string()],
            extension_parity_markers: vec!["required_extensions".to_string()],
            artifact_report_markers: vec!["change_artifact_path".to_string()],
            artifact_access_markers: vec!["is_retrievable_remote_artifact".to_string()],
        }
    }

    #[test]
    fn marker_names_come_from_config_not_detector_defaults() {
        let fp = fingerprint(
            "src/main.rs",
            r#"
            fn run(command: Vec<String>) {
                execute_remote_work("remote", RemoteWorkOptions { command, capture_change: false });
            }
            "#,
        );

        assert!(run(&[&fp], &policy_config()).is_empty());

        let config = RemoteExecutionSafetyConfig {
            dispatch_markers: vec![
                "execute_remote_work".to_string(),
                "RemoteWorkOptions".to_string(),
            ],
            ..Default::default()
        };

        let findings = run(&[&fp], &config);

        assert!(findings
            .iter()
            .any(|finding| finding.kind == AuditFinding::RemoteExecutionPreflight));
    }

    #[test]
    fn flags_arg_forwarding_without_path_preflight() {
        let fp = fingerprint(
            "src/main.rs",
            r#"
            fn run(args: &[String]) {
                let mut command = vec!["tool".to_string()];
                command.extend(args.iter().cloned());
                execute_remote_work("remote", RemoteWorkOptions { command, capture_change: false });
            }
            "#,
        );

        let findings = run(&[&fp], &policy_config());

        assert!(findings.iter().any(|finding| {
            finding.kind == AuditFinding::RemoteExecutionPreflight
                && finding
                    .suggestion
                    .contains("strips local-only wrapper flags")
        }));
    }

    #[test]
    fn accepts_configured_path_rewrite_and_artifact_snapshot() {
        let fp = fingerprint(
            "src/main.rs",
            r#"
            fn run(normalized_args: &[String], remote_path: &str) {
                let plan = remote_capability_plan(command, source_path).unwrap();
                evaluate_remote_capabilities(target_id, &plan, &capabilities, mode);
                let mut command = vec!["tool".to_string()];
                command.extend(translate_remote_args(normalized_args, remote_path));
                execute_remote_work("remote", RemoteWorkOptions {
                    command,
                    capture_change: true,
                    source_snapshot: Some(source_snapshot),
                });
            }
            "#,
        );

        assert!(run(&[&fp], &policy_config()).is_empty());
    }

    #[test]
    fn flags_remote_dispatch_without_capability_preflight() {
        let fp = fingerprint(
            "src/main.rs",
            r#"
            fn run(command: Vec<String>) {
                execute_remote_work("remote", RemoteWorkOptions {
                    command,
                    capture_change: false,
                    source_snapshot: None,
                });
            }
            "#,
        );

        let findings = run(&[&fp], &policy_config());

        assert!(findings
            .iter()
            .any(|finding| finding.description.contains("capability parity")));
    }

    #[test]
    fn accepts_remote_dispatch_with_capability_preflight() {
        let fp = fingerprint(
            "src/main.rs",
            r#"
            fn run(command: Vec<String>) {
                let plan = remote_capability_plan(command_kind, source_path).unwrap();
                evaluate_remote_capabilities(target_id, &plan, &capabilities, mode);
                execute_remote_work("remote", RemoteWorkOptions {
                    command,
                    capture_change: false,
                    source_snapshot: None,
                });
            }
            "#,
        );

        let findings = run(&[&fp], &policy_config());

        assert!(!findings
            .iter()
            .any(|finding| finding.description.contains("capability parity")));
    }

    #[test]
    fn flags_artifact_capture_without_snapshot_contract() {
        let fp = fingerprint(
            "src/core/remote.rs",
            r#"
            fn run(command: Vec<String>) {
                execute_remote_work("remote", RemoteWorkOptions { command, capture_change: true });
            }
            "#,
        );

        let findings = run(&[&fp], &policy_config());

        assert!(findings
            .iter()
            .any(|finding| finding.description.contains("artifact capture")));
    }

    #[test]
    fn flags_extension_dispatch_without_parity_preflight() {
        let fp = fingerprint(
            "src/main.rs",
            r#"
            fn run(command: Vec<String>, extension_selector: &str) {
                execute_remote_work("remote", RemoteWorkOptions { command, capture_change: false });
            }
            "#,
        );

        let findings = run(&[&fp], &policy_config());

        assert!(findings
            .iter()
            .any(|finding| finding.description.contains("extension selector")));
    }

    #[test]
    fn flags_remote_artifact_reporting_without_access_check() {
        let fp = fingerprint(
            "src/core/remote/evidence.rs",
            r#"
            fn mirrored_patch_result(patch: Value, artifact: ArtifactRecord) -> Value {
                let mut patched = patch.clone();
                patched["change_artifact_path"] = Value::String(artifact.path);
                patched
            }
            "#,
        );

        let findings = run(&[&fp], &policy_config());

        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("locally accessible"));
    }

    #[test]
    fn accepts_remote_artifact_reporting_with_access_check() {
        let fp = fingerprint(
            "src/core/remote/evidence.rs",
            r#"
            fn mirrored_patch_result(patch: Value, artifact: ArtifactRecord) -> Value {
                let accessible = is_retrievable_remote_artifact(&artifact.path);
                if accessible {
                    let mut patched = patch.clone();
                    patched["change_artifact_path"] = Value::String(artifact.path);
                    return patched;
                }
                patch
            }
            "#,
        );

        assert!(run(&[&fp], &policy_config()).is_empty());
    }
}
