use super::super::*;
use homeboy::runner::ExtensionParityProbe;
use types::RunnerDoctorStatus;

#[test]
fn extension_parity_check_reports_missing_extension_with_remediation() {
    let check = extension_parity::check_from_probe(
        "remote",
        "/home/user/.local/bin/homeboy",
        Some("/home/user/Developer/component"),
        "rust",
        false,
        "first\nsecond\nthird\nfourth",
        "",
    );

    assert_eq!(check.id, "extension.parity");
    assert_eq!(check.status, RunnerDoctorStatus::Error);
    assert!(check.message.contains("rust"));
    assert!(check
        .remediation
        .as_deref()
        .expect("remediation")
        .contains("extension install <source> --id rust"));
    assert_eq!(
        check.details.get("cwd").map(String::as_str),
        Some("/home/user/Developer/component")
    );
    assert_eq!(
        check.details.get("diagnostics").map(String::as_str),
        Some("second\nthird\nfourth")
    );
}

#[test]
fn extension_parity_check_extracts_nested_json_error_message() {
    let check = extension_parity::check_from_probe(
        "remote",
        "homeboy",
        None,
        "rust",
        false,
        "",
        r#"{"success":false,"error":{"message":"Extension 'rust' not found"}}"#,
    );

    assert_eq!(
        check.details.get("diagnostics").map(String::as_str),
        Some("Extension 'rust' not found")
    );
}

#[test]
fn extension_parity_check_reports_resolved_extension() {
    let check = extension_parity::check_from_probe(
        "remote",
        "homeboy",
        None,
        "rust",
        true,
        "",
        "extension details",
    );

    assert_eq!(check.id, "extension.parity");
    assert_eq!(check.status, RunnerDoctorStatus::Ok);
    assert!(check.remediation.is_none());
    assert_eq!(
        check.details.get("extension_id").map(String::as_str),
        Some("rust")
    );
}

#[test]
fn extension_parity_check_reports_copied_extension_as_actionable_ok() {
    let check = extension_parity::check_from_probe(
        "remote",
        "homeboy",
        None,
        "rust",
        true,
        "",
        r#"{"success":true,"data":{"extension":{"id":"rust","linked":false,"path":"/runner/extensions/rust","source_revision":"abc123"}}}"#,
    );

    assert_eq!(check.id, "extension.parity");
    assert_eq!(check.status, RunnerDoctorStatus::Ok);
    assert!(check.message.contains("copied install"));
    assert_eq!(
        check.details.get("linked").map(String::as_str),
        Some("false")
    );
    assert_eq!(
        check.details.get("source_revision").map(String::as_str),
        Some("abc123")
    );
    assert!(check
        .remediation
        .as_deref()
        .is_some_and(|value| value.contains("extension diff-installed rust")));
}

#[test]
fn stale_extension_parity_skips_provider_readiness() {
    let mut checks = Vec::new();
    let mut readiness_ran = false;
    let stale = extension_parity::check_from_parity_probe(
        "remote",
        "homeboy",
        None,
        "fixture-extension",
        ExtensionParityProbe::NeedsMaterialization {
            error: homeboy::core::Error::validation_invalid_argument(
                "runner_extension",
                "Runner 'lab' has stale extension parity for 'fixture-extension' before command execution",
                Some("fixture-extension".to_string()),
                Some(vec![
                    "Local extension source_revision: abc123".to_string(),
                    "Relink or update the extension on the runner before dispatch: homeboy extension relink fixture-extension <source>".to_string(),
                ]),
            ),
        },
    );

    extension_parity::append_after_extension_parity(&mut checks, vec![stale], || {
        readiness_ran = true;
        vec![checks::ok(
            "provider.readiness",
            "should not run".to_string(),
            None,
        )]
    });

    assert!(!readiness_ran);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].id, "extension.parity");
    assert_eq!(checks[0].status, RunnerDoctorStatus::Error);
    assert!(checks[0]
        .remediation
        .as_deref()
        .is_some_and(|value| value.contains("extension relink fixture-extension")));
}

#[test]
fn current_extension_parity_runs_provider_readiness() {
    let mut checks = Vec::new();
    let mut readiness_ran = false;
    let current = extension_parity::check_from_parity_probe(
        "remote",
        "homeboy",
        None,
        "fixture-extension",
        ExtensionParityProbe::Current,
    );

    extension_parity::append_after_extension_parity(&mut checks, vec![current], || {
        readiness_ran = true;
        vec![checks::ok("provider.readiness", "ready".to_string(), None)]
    });

    assert!(readiness_ran);
    assert_eq!(checks[0].id, "extension.parity");
    assert_eq!(checks[0].status, RunnerDoctorStatus::Ok);
    assert_eq!(checks[1].id, "provider.readiness");
}

#[test]
fn lab_offload_required_extensions_merge_explicit_and_provider_ids() {
    let mut selected =
        node_provider_with_extension("selected.provider", "test", "fixture-extension");
    selected.runner_readiness = vec![serde_json::from_value(serde_json::json!({
        "id": "fixture.readiness",
        "label": "Fixture readiness",
        "required_extensions": ["readiness-extension"]
    }))
    .expect("readiness parses")];
    let other = node_provider_with_extension("other.provider", "other", "other-extension");

    assert_eq!(
        probes::lab_offload_extension_dependencies(
            &[
                " explicit-extension ".to_string(),
                "fixture-extension".to_string()
            ],
            &[selected, other],
            Some("test"),
            Some("selected.provider"),
        ),
        vec![
            probes::LabOffloadExtensionDependency {
                extension_id: "explicit-extension".to_string(),
                provider_id: None,
            },
            probes::LabOffloadExtensionDependency {
                extension_id: "fixture-extension".to_string(),
                provider_id: None,
            },
            probes::LabOffloadExtensionDependency {
                extension_id: "readiness-extension".to_string(),
                provider_id: Some("selected.provider".to_string()),
            }
        ]
    );
}

fn node_provider_with_extension(
    id: &str,
    backend: &str,
    extension_id: &str,
) -> homeboy::agents::agent_tasks::provider::AgentTaskExecutorProvider {
    let mut provider: homeboy::agents::agent_tasks::provider::AgentTaskExecutorProvider =
        serde_json::from_value(serde_json::json!({
            "id": id,
            "backend": backend,
            "invocation": { "argv": ["node", "executor.cjs"] }
        }))
        .expect("provider parses");
    provider.extension_id = Some(extension_id.to_string());
    provider
}

#[test]
fn normalizes_requested_extensions_before_parity_checks() {
    assert_eq!(
        normalized_extension_ids(&[
            " rust ".to_string(),
            "".to_string(),
            "fixture-a".to_string(),
            "rust".to_string(),
        ]),
        vec!["fixture-a".to_string(), "rust".to_string()]
    );
}
