//! Runner/offload preflight detector.
//!
//! Flags remote runner dispatch sites that can hand local paths or artifacts to
//! a runner without an explicit translation/mirror contract.

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

pub(in crate::core::code_audit) fn run(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let mut findings = Vec::new();

    for fp in fingerprints {
        if super::walker::is_test_path(&fp.relative_path) || !is_remote_dispatch_file(&fp.content) {
            continue;
        }

        if forwards_args_without_path_preflight(&fp.content) {
            findings.push(finding(
                fp,
                "Remote runner dispatch builds command argv from caller args without an explicit path-translation preflight.",
                "Route argv through a rewrite/preflight function that strips local-only wrapper flags and translates or rejects local filesystem paths before runner dispatch.",
            ));
        }

        if captures_artifacts_without_snapshot(&fp.content) {
            findings.push(finding(
                fp,
                "Remote runner dispatch can request artifact capture without a source snapshot or mirror contract.",
                "Attach source_snapshot or an equivalent mirror verification contract before reporting remote artifacts as run evidence.",
            ));
        }

        if dispatches_extension_without_parity_preflight(&fp.content) {
            findings.push(finding(
                fp,
                "Remote runner dispatch accepts an extension selector without validating runner extension parity before execution.",
                "Add a pre-dispatch extension parity check so missing runner-side extension support fails before command execution.",
            ));
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings.dedup_by(|a, b| a.file == b.file && a.description == b.description);
    findings
}

fn is_remote_dispatch_file(content: &str) -> bool {
    content.contains("RunnerExecOptions")
        || content.contains("runner::exec")
        || content.contains("core::runner::exec")
}

fn forwards_args_without_path_preflight(content: &str) -> bool {
    let forwards_caller_args = contains_any(
        content,
        &[
            "normalized_args",
            "std::env::args",
            "args.iter()",
            "command.extend(args",
        ],
    );
    let has_path_preflight = contains_any(
        content,
        &[
            "rewrite_lab_offload_args",
            "translate_remote_path",
            "remote_path",
            "sync_workspace",
            "strip_local_output",
        ],
    );

    forwards_caller_args && !has_path_preflight
}

fn captures_artifacts_without_snapshot(content: &str) -> bool {
    content.contains("capture_patch: true") && !content.contains("source_snapshot")
}

fn dispatches_extension_without_parity_preflight(content: &str) -> bool {
    let accepts_extension = contains_any(content, &["--extension", "extension_id", "extension"]);
    let has_parity_preflight = contains_any(
        content,
        &[
            "extension_parity",
            "required_extensions",
            "runner_extension",
            "validate_runner_extension",
        ],
    );

    accepts_extension && !has_parity_preflight
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn finding(fp: &FileFingerprint, description: &str, suggestion: &str) -> Finding {
    Finding {
        convention: "runner_offload_preflight".to_string(),
        severity: Severity::Warning,
        file: fp.relative_path.clone(),
        description: description.to_string(),
        suggestion: suggestion.to_string(),
        kind: AuditFinding::RunnerOffloadPreflight,
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

    #[test]
    fn flags_arg_forwarding_without_path_preflight() {
        let fp = fingerprint(
            "src/main.rs",
            r#"
            fn run(args: &[String]) {
                let mut command = vec!["homeboy".to_string()];
                command.extend(args.iter().cloned());
                runner::exec("lab", RunnerExecOptions { command, capture_patch: false });
            }
            "#,
        );

        let findings = run(&[&fp]);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::RunnerOffloadPreflight);
        assert!(findings[0]
            .suggestion
            .contains("strips local-only wrapper flags"));
    }

    #[test]
    fn accepts_explicit_path_rewrite_and_artifact_snapshot() {
        let fp = fingerprint(
            "src/main.rs",
            r#"
            fn run(normalized_args: &[String], remote_path: &str) {
                let mut command = vec!["homeboy".to_string()];
                command.extend(rewrite_lab_offload_args(normalized_args, remote_path));
                runner::exec("lab", RunnerExecOptions {
                    command,
                    capture_patch: true,
                    source_snapshot: Some(source_snapshot),
                });
            }
            "#,
        );

        assert!(run(&[&fp]).is_empty());
    }

    #[test]
    fn flags_artifact_capture_without_snapshot_contract() {
        let fp = fingerprint(
            "src/core/runner.rs",
            r#"
            fn run(command: Vec<String>) {
                runner::exec("lab", RunnerExecOptions { command, capture_patch: true });
            }
            "#,
        );

        let findings = run(&[&fp]);

        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("artifact capture"));
    }

    #[test]
    fn flags_extension_dispatch_without_parity_preflight() {
        let fp = fingerprint(
            "src/main.rs",
            r#"
            fn run(command: Vec<String>, extension_id: &str) {
                runner::exec("lab", RunnerExecOptions { command, capture_patch: false });
            }
            "#,
        );

        let findings = run(&[&fp]);

        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("extension selector"));
    }
}
