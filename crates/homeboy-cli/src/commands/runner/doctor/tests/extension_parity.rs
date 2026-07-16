use super::super::*;
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
