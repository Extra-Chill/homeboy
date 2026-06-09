use super::*;
use types::RunnerDoctorStatus;

#[test]
fn local_alias_report_has_stable_top_level_shape() {
    let (report, exit_code) = run("local").expect("local doctor report");
    assert_eq!(exit_code, 0);
    let value = serde_json::to_value(report).expect("serialize report");
    assert_eq!(value["command"], "runner.doctor");
    assert_eq!(value["runner_id"], "local");
    assert!(value.get("status").is_some());
    assert!(value.get("capabilities").is_some());
    assert!(value.get("resources").is_some());
    assert!(value
        .get("checks")
        .and_then(|checks| checks.as_array())
        .is_some());
}

#[test]
fn overall_status_promotes_errors_over_warnings() {
    let checks = vec![
        checks::warning("optional", "optional missing".to_string(), None),
        checks::error(
            "required",
            "required missing".to_string(),
            None,
            BTreeMap::new(),
        ),
    ];
    assert_eq!(checks::overall_status(&checks), RunnerDoctorStatus::Error);
}

#[test]
fn extension_parity_check_reports_missing_extension_with_remediation() {
    let check = extension_parity::check_from_probe(
        "remote",
        "/home/chubes/.local/bin/homeboy",
        Some("/home/chubes/Developer/component"),
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
        Some("/home/chubes/Developer/component")
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

#[test]
fn normalizes_required_tools_before_preflight_checks() {
    assert_eq!(
        normalized_required_tools(&[
            " zip ".to_string(),
            "".to_string(),
            "tar".to_string(),
            "zip".to_string(),
        ]),
        vec!["tar".to_string(), "zip".to_string()]
    );
}

#[test]
fn required_tool_check_errors_with_actionable_remediation() {
    let check = checks::required_tool_check(
        "zip",
        &types::ToolProbe {
            available: false,
            path: None,
            version: None,
            error: Some("not found on PATH".to_string()),
        },
    );

    assert_eq!(check.id, "tool.required.zip");
    assert_eq!(check.status, RunnerDoctorStatus::Error);
    assert!(check.message.contains("zip"));
    assert_eq!(
        check.details.get("command").map(String::as_str),
        Some("zip")
    );
    assert!(check
        .remediation
        .as_deref()
        .is_some_and(|value| value.contains("Install zip on the runner")));
}

#[test]
fn headed_browser_check_warns_without_display_or_xvfb() {
    let check = checks::headed_browser_check(false, false, false);

    assert_eq!(check.id, "browser.headed_ready");
    assert_eq!(check.status, RunnerDoctorStatus::Warning);
    assert_eq!(
        check.details.get("display_ready").map(String::as_str),
        Some("false")
    );
    assert_eq!(
        check.details.get("xvfb_ready").map(String::as_str),
        Some("false")
    );
    assert!(check
        .remediation
        .as_deref()
        .is_some_and(|value| value.contains("headless/Ozone")));
}

#[test]
fn headed_browser_ready_accepts_display_or_xvfb() {
    assert!(probes::headed_browser_ready(true, false));
    assert!(probes::headed_browser_ready(false, true));
    assert!(!probes::headed_browser_ready(false, false));
}

#[test]
fn local_doctor_honors_required_tool_errors() {
    let (report, exit_code) = run_with_options(
        "local",
        RunnerDoctorOptions {
            path: None,
            extensions: Vec::new(),
            required_tools: vec!["homeboy-definitely-missing-tool".to_string()],
        },
    )
    .expect("local doctor report");

    assert_eq!(exit_code, 0);
    assert_eq!(report.status, RunnerDoctorStatus::Error);
    assert!(report.checks.iter().any(|check| {
        check.id == "tool.required.homeboy-definitely-missing-tool"
            && check.status == RunnerDoctorStatus::Error
    }));
}

#[test]
fn homeboy_version_skew_check_is_absent_for_equal_versions() {
    assert!(checks::homeboy_version_skew_check("0.198.7", "0.198.7", "lab", "lab").is_none());
}

#[test]
fn homeboy_version_skew_check_warns_for_different_versions() {
    let check = checks::homeboy_version_skew_check("0.198.7", "0.197.7", "lab", "lab")
        .expect("version skew warning");

    assert_eq!(check.id, "homeboy.version_skew");
    assert_eq!(check.status, RunnerDoctorStatus::Warning);
    assert!(check.message.contains("0.198.7"));
    assert!(check.message.contains("0.197.7"));
    assert_eq!(
        check.details.get("local_version").map(String::as_str),
        Some("0.198.7")
    );
    assert_eq!(
        check.details.get("remote_version").map(String::as_str),
        Some("0.197.7")
    );
    assert!(check
        .remediation
        .as_deref()
        .is_some_and(|value| value.contains("homeboy ssh lab -- homeboy upgrade")));
}

#[test]
fn daemon_exec_probe_reports_structured_failure() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut buffer = [0; 4096];
        let _ = std::io::Read::read(&mut stream, &mut buffer).expect("read request");
        let body = serde_json::json!({
            "success": false,
            "data": {
                "error": "validation.invalid_argument",
                "message": "Invalid argument 'runner': stale daemon session"
            },
            "error": {
                "error": "validation.invalid_argument",
                "message": "Invalid argument 'runner': stale daemon session"
            }
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(), body
        );
        std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write response");
    });

    let check = probes::daemon_exec_check(
        "homeboy-lab",
        "/home/chubes/Developer",
        &format!("http://{addr}"),
    );

    assert_eq!(check.id, "daemon.exec");
    assert_eq!(check.status, RunnerDoctorStatus::Error);
    assert!(check.message.contains("failed the lightweight exec probe"));
    assert!(check
        .details
        .get("response")
        .expect("response detail")
        .contains("validation.invalid_argument"));
    assert!(check
        .remediation
        .expect("remediation")
        .contains("homeboy runner connect homeboy-lab"));
}

#[test]
fn ssh_target_uses_runner_env_for_remote_probes() {
    crate::test_support::with_isolated_home(|_| {
        server::create(
            r#"{
                "id":"lab",
                "host":"localhost",
                "user":"tester",
                "env":{"PATH":"/server/bin:$PATH"}
            }"#,
            false,
        )
        .expect("create server");
        runner::create(
            r#"{
                "id":"lab",
                "kind":"ssh",
                "server_id":"lab",
                "workspace_root":"/tmp",
                "env":{"PATH":"/runner/bin:$PATH"}
            }"#,
            false,
        )
        .expect("create runner");

        let target = target::resolve("lab").expect("resolve runner target");
        let target::RunnerTarget::Ssh { client, .. } = target else {
            panic!("expected ssh target");
        };

        assert_eq!(
            client.env.get("PATH").map(String::as_str),
            Some("/runner/bin:$PATH")
        );
    });
}

#[test]
fn remote_default_artifact_root_expands_under_home() {
    assert_eq!(
        remote::default_artifact_root_for_home("/home/runner"),
        Some("/home/runner/.local/share/homeboy/artifacts".to_string())
    );
}

#[test]
fn remote_default_artifact_root_normalizes_trailing_home_slash() {
    assert_eq!(
        remote::default_artifact_root_for_home("/Users/chubes/"),
        Some("/Users/chubes/.local/share/homeboy/artifacts".to_string())
    );
}

#[test]
fn remote_default_artifact_root_rejects_empty_home() {
    assert_eq!(remote::default_artifact_root_for_home("  "), None);
}
