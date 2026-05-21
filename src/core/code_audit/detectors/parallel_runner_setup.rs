//! Parallel runner setup detector.
//!
//! Finds command-family files that independently assemble the same generic
//! execution contract. The detector stays ecosystem-neutral by looking for
//! repeated setup phases and shared contract-call shapes rather than specific
//! tools, runtimes, or package managers.

use std::collections::{BTreeMap, BTreeSet};

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

const MIN_GROUP_SIZE: usize = 2;
const MIN_SHARED_PHASES: usize = 2;
const MIN_SHARED_CONTRACT_CALLS: usize = 2;

pub(in crate::core::code_audit) fn run(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    detect_parallel_runner_setup(fingerprints)
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct RunnerSignature {
    phases: BTreeSet<&'static str>,
    contract_calls: BTreeSet<String>,
}

#[derive(Debug)]
struct Candidate<'a> {
    file: &'a str,
}

fn detect_parallel_runner_setup(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let mut groups: BTreeMap<RunnerSignature, Vec<Candidate>> = BTreeMap::new();

    for fp in fingerprints {
        if super::walker::is_test_path(&fp.relative_path) {
            continue;
        }

        let signature = runner_signature(&fp.content);
        if signature.phases.len() < MIN_SHARED_PHASES
            || signature.contract_calls.len() < MIN_SHARED_CONTRACT_CALLS
            || !signature.phases.contains("execution")
        {
            continue;
        }

        groups
            .entry(signature.clone())
            .or_default()
            .push(Candidate {
                file: &fp.relative_path,
            });
    }

    let mut findings = Vec::new();
    for (signature, mut members) in groups {
        if members.len() < MIN_GROUP_SIZE {
            continue;
        }

        members.sort_by(|a, b| a.file.cmp(b.file));
        let member_files: Vec<&str> = members.iter().map(|member| member.file).collect();
        let anchor = member_files
            .first()
            .copied()
            .unwrap_or("<unknown>")
            .to_string();
        let phases = signature
            .phases
            .iter()
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        let calls = signature
            .contract_calls
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");

        findings.push(Finding {
            convention: "parallel_runner_setup".to_string(),
            severity: Severity::Warning,
            file: anchor,
            description: format!(
                "Parallel runner setup: {} command-family files share phases [{}] and contract calls [{}]. Members: {}.",
                member_files.len(),
                phases,
                calls,
                member_files.join(", ")
            ),
            suggestion: "Extract a runner descriptor or shared builder that owns context resolution, environment construction, artifact setup, execution, result mapping, and error mapping for this contract.".to_string(),
            kind: AuditFinding::ParallelRunnerSetup,
        });
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

fn runner_signature(content: &str) -> RunnerSignature {
    let mut phases = BTreeSet::new();
    let mut contract_calls = BTreeSet::new();

    for call in extract_call_names(content) {
        if let Some(phase) = phase_for_call(&call) {
            phases.insert(phase);
            contract_calls.insert(call);
        }
    }

    RunnerSignature {
        phases,
        contract_calls,
    }
}

fn phase_for_call(call: &str) -> Option<&'static str> {
    if call.contains("context") && contains_any(call, &["resolve", "build", "execution", "runner"])
    {
        return Some("context");
    }
    if contains_any(call, &["env", "environment"])
        && contains_any(
            call,
            &["build", "construct", "prepare", "runner", "command"],
        )
    {
        return Some("environment");
    }
    if contains_any(
        call,
        &["artifact", "output_path", "report_path", "log_path"],
    ) && contains_any(call, &["build", "prepare", "setup", "create", "runner"])
    {
        return Some("artifacts");
    }
    if contains_any(
        call,
        &["execute", "invoke", "spawn", "script", "process", "runner"],
    ) {
        return Some("execution");
    }
    if contains_any(call, &["result", "outcome", "status", "exit", "success"])
        && contains_any(call, &["map", "parse", "runner", "execution"])
    {
        return Some("result");
    }
    if contains_any(call, &["error", "failure", "diagnostic"])
        && contains_any(call, &["map", "parse", "runner", "execution"])
    {
        return Some("error");
    }
    None
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn extract_call_names(content: &str) -> Vec<String> {
    let bytes = content.as_bytes();
    let mut calls = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'(' {
            i += 1;
            continue;
        }

        let mut end = i;
        while end > 0 && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }

        let mut start = end;
        while start > 0 && is_identifier_byte(bytes[start - 1]) {
            start -= 1;
        }

        if start < end {
            let raw = &content[start..end];
            let normalized = normalize_identifier(raw);
            if !normalized.is_empty() && !is_control_keyword(&normalized) {
                calls.push(normalized);
            }
        }

        i += 1;
    }

    calls.sort();
    calls.dedup();
    calls
}

fn normalize_identifier(raw: &str) -> String {
    let mut normalized = String::new();
    for (index, ch) in raw.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                normalized.push('_');
            }
            normalized.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_alphanumeric() || ch == '_' {
            normalized.push(ch.to_ascii_lowercase());
        }
    }
    normalized
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_control_keyword(value: &str) -> bool {
    matches!(
        value,
        "if" | "for" | "while" | "switch" | "match" | "return" | "catch"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::code_audit::conventions::Language;

    fn fingerprint(path: &str, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Unknown,
            content: content.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_run() {
        let alpha = fingerprint(
            "src/families/alpha.rs",
            r#"
            fn dispatch_alpha() {
                let context = resolve_execution_context(input);
                let env = build_command_environment(&context);
                let artifacts = prepare_artifact_paths(&context);
                let output = invoke_extension_script(&context, &env, &artifacts);
                let result = map_runner_result(output);
                map_runner_error(result);
            }
            "#,
        );
        let beta = fingerprint(
            "src/families/beta.rs",
            r#"
            fn dispatch_beta() {
                let context = resolve_execution_context(input);
                let env = build_command_environment(&context);
                let artifacts = prepare_artifact_paths(&context);
                let output = invoke_extension_script(&context, &env, &artifacts);
                let result = map_runner_result(output);
                map_runner_error(result);
            }
            "#,
        );

        let findings = run(&[&alpha, &beta]);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::ParallelRunnerSetup);
        assert!(findings[0].description.contains("alpha.rs"));
        assert!(findings[0].description.contains("beta.rs"));
        assert!(findings[0].suggestion.contains("runner descriptor"));
    }

    #[test]
    fn ignores_different_execution_protocols() {
        let alpha = fingerprint(
            "src/families/alpha.rs",
            r#"
            fn dispatch_alpha() {
                let context = resolve_execution_context(input);
                let env = build_command_environment(&context);
                invoke_extension_script(&context, &env);
            }
            "#,
        );
        let beta = fingerprint(
            "src/families/beta.rs",
            r#"
            fn dispatch_beta() {
                let token = lookup_session_token(input);
                let payload = encode_payload(token);
                send_payload(payload);
            }
            "#,
        );

        assert!(run(&[&alpha, &beta]).is_empty());
    }

    #[test]
    fn requires_shared_phases_and_contract_calls() {
        let alpha = fingerprint(
            "src/families/alpha.rs",
            r#"
            fn dispatch_alpha() {
                invoke_extension_script(input);
                execute_contract(input);
            }
            "#,
        );
        let beta = fingerprint(
            "src/families/beta.rs",
            r#"
            fn dispatch_beta() {
                invoke_extension_script(input);
                execute_contract(input);
            }
            "#,
        );

        assert!(run(&[&alpha, &beta]).is_empty());
    }
}
