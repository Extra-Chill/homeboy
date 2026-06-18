use super::*;
use homeboy::core::agent_tasks::provider::{
    AgentTaskProviderEnvPathReadiness, AgentTaskProviderRunnerReadiness,
};
use std::collections::BTreeMap;
use types::{HomeboyProbe, RunnerDoctorStatus};

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
fn doctor_options_default_to_general_read_only_scope() {
    let options = RunnerDoctorOptions::default();

    assert_eq!(options.scope, RunnerDoctorScope::General);
    assert!(!options.repair);
}

#[test]
fn doctor_output_omits_empty_repairs() {
    let (report, _) = run("local").expect("local doctor report");
    let value = serde_json::to_value(report).expect("serialize report");

    assert!(value.get("repairs").is_none());
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
fn required_homeboy_tools_capture_versions() {
    assert_eq!(
        probes::required_tool_version_args("homeboy"),
        &["--version"]
    );
    assert_eq!(
        probes::required_tool_version_args("/home/user/.cargo/bin/homeboy"),
        &["--version"]
    );
    assert!(probes::required_tool_version_args("git").is_empty());
}

#[test]
fn provider_readiness_renderer_uses_fake_provider_contract() {
    let contract = AgentTaskProviderRunnerReadiness {
        id: "lab.fake_runtime.cache".to_string(),
        label: "Fake runtime cache".to_string(),
        secret_env: Vec::new(),
        env_path: Some(AgentTaskProviderEnvPathReadiness {
            env: vec!["FAKE_RUNTIME_BIN".to_string()],
            revision: Some(true),
            canonical_path: None,
            extra: BTreeMap::new(),
        }),
        executable: None,
        remediation: Some("Refresh the fake runtime cache".to_string()),
        extra: BTreeMap::new(),
    };

    let check = probes::provider_env_path_readiness_check_from_probe(
        &contract,
        Some("/opt/fake-runtime/bin".to_string()),
        true,
        Some("abc123".to_string()),
        None,
    );

    assert_eq!(check.id, "lab.fake_runtime.cache");
    assert_eq!(check.status, RunnerDoctorStatus::Ok);
    assert!(check.message.contains("Fake runtime cache"));
    assert_eq!(
        check.details.get("env").map(String::as_str),
        Some("FAKE_RUNTIME_BIN")
    );
    assert_eq!(
        check.details.get("revision").map(String::as_str),
        Some("abc123")
    );
}

#[test]
fn provider_readiness_warns_on_non_canonical_checkout() {
    let contract = AgentTaskProviderRunnerReadiness {
        id: "lab.fake_runtime.cache".to_string(),
        label: "Fake runtime cache".to_string(),
        secret_env: Vec::new(),
        env_path: Some(AgentTaskProviderEnvPathReadiness {
            env: vec!["FAKE_RUNTIME_BIN".to_string()],
            revision: Some(true),
            canonical_path: Some("/home/runner/.cache/homeboy/source".to_string()),
            extra: BTreeMap::new(),
        }),
        executable: None,
        remediation: Some("Refresh the managed source checkout".to_string()),
        extra: BTreeMap::new(),
    };

    let check = probes::provider_env_path_readiness_check_from_probe(
        &contract,
        Some("/home/runner/Developer/stale-checkout/dist/index.js".to_string()),
        true,
        None,
        Some("/home/runner/.cache/homeboy/source".to_string()),
    );

    assert_eq!(check.status, RunnerDoctorStatus::Warning);
    assert!(check.message.contains("non-canonical checkout"));
    assert_eq!(
        check.details.get("canonical_path").map(String::as_str),
        Some("/home/runner/.cache/homeboy/source")
    );
    assert_eq!(
        check.remediation.as_deref(),
        contract.remediation.as_deref()
    );
}

#[test]
fn provider_readiness_ok_when_path_within_canonical_root() {
    let contract = AgentTaskProviderRunnerReadiness {
        id: "lab.fake_runtime.cache".to_string(),
        label: "Fake runtime cache".to_string(),
        secret_env: Vec::new(),
        env_path: Some(AgentTaskProviderEnvPathReadiness {
            env: vec!["FAKE_RUNTIME_BIN".to_string()],
            revision: None,
            canonical_path: Some("/home/runner/.cache/homeboy/source".to_string()),
            extra: BTreeMap::new(),
        }),
        executable: None,
        remediation: None,
        extra: BTreeMap::new(),
    };

    let check = probes::provider_env_path_readiness_check_from_probe(
        &contract,
        Some("/home/runner/.cache/homeboy/source/dist/index.js".to_string()),
        true,
        None,
        Some("/home/runner/.cache/homeboy/source".to_string()),
    );

    assert_eq!(check.status, RunnerDoctorStatus::Ok);
}

#[test]
fn path_within_canonical_root_is_segment_aware() {
    assert!(probes::path_within_canonical_root("/a/source", "/a/source"));
    assert!(probes::path_within_canonical_root(
        "/a/source/dist",
        "/a/source"
    ));
    assert!(probes::path_within_canonical_root(
        "/a/source/",
        "/a/source"
    ));
    // Prefix collision must not count as containment.
    assert!(!probes::path_within_canonical_root("/a/sour", "/a/source"));
    assert!(!probes::path_within_canonical_root(
        "/a/source-stale/dist",
        "/a/source"
    ));
    // Empty root is treated as "no canonical constraint".
    assert!(probes::path_within_canonical_root("/anywhere", ""));
}

#[test]
fn lab_homeboy_path_shadow_warns_when_bare_homeboy_is_older() {
    let mut details = BTreeMap::new();
    details.insert(
        "configured_command".to_string(),
        "/home/user/.cargo/bin/homeboy".to_string(),
    );
    details.insert(
        "configured_path".to_string(),
        "/home/user/.cargo/bin/homeboy".to_string(),
    );
    details.insert("configured_version".to_string(), "0.229.9".to_string());
    details.insert(
        "bare_path".to_string(),
        "/home/user/.local/bin/homeboy".to_string(),
    );
    details.insert("bare_version".to_string(), "0.228.22".to_string());

    let check = probes::homeboy_path_shadow_check(
        "homeboy-lab",
        "lab-server",
        "/home/user/.cargo/bin/homeboy",
        "0.229.9",
        &HomeboyProbe {
            version: "0.229.9".to_string(),
            path: Some("/home/user/.cargo/bin/homeboy".to_string()),
        },
        &probes::RemoteHomeboyCandidateProbe {
            path: Some("/home/user/.local/bin/homeboy".to_string()),
            version: Some("0.228.22".to_string()),
        },
        details,
    )
    .expect("stale bare homeboy warning");

    assert_eq!(check.id, "lab.homeboy.path_shadow");
    assert_eq!(check.status, RunnerDoctorStatus::Warning);
    assert!(check.message.contains("0.229.9"));
    assert!(check.message.contains("0.228.22"));
    assert_eq!(
        check.details.get("configured_path").map(String::as_str),
        Some("/home/user/.cargo/bin/homeboy")
    );
    assert_eq!(
        check.details.get("bare_path").map(String::as_str),
        Some("/home/user/.local/bin/homeboy")
    );
    assert!(check
        .remediation
        .as_deref()
        .is_some_and(|value| value.contains("Fix PATH ordering")));
}

#[test]
fn lab_homeboy_path_shadow_warns_when_bare_homeboy_resolves_different_path() {
    let mut details = BTreeMap::new();
    details.insert(
        "configured_command".to_string(),
        "/home/user/.cargo/bin/homeboy".to_string(),
    );
    details.insert(
        "configured_path".to_string(),
        "/home/user/.cargo/bin/homeboy".to_string(),
    );
    details.insert("configured_version".to_string(), "0.229.9".to_string());
    details.insert(
        "bare_path".to_string(),
        "/home/user/.local/bin/homeboy".to_string(),
    );
    details.insert("bare_version".to_string(), "0.229.9".to_string());

    let check = probes::homeboy_path_shadow_check(
        "homeboy-lab",
        "lab-server",
        "/home/user/.cargo/bin/homeboy",
        "0.229.9",
        &HomeboyProbe {
            version: "0.229.9".to_string(),
            path: Some("/home/user/.cargo/bin/homeboy".to_string()),
        },
        &probes::RemoteHomeboyCandidateProbe {
            path: Some("/home/user/.local/bin/homeboy".to_string()),
            version: Some("0.229.9".to_string()),
        },
        details,
    )
    .expect("different bare homeboy path warning");

    assert_eq!(check.id, "lab.homeboy.path_shadow");
    assert_eq!(check.status, RunnerDoctorStatus::Warning);
    assert!(check.message.contains("/home/user/.cargo/bin/homeboy"));
    assert!(check.message.contains("/home/user/.local/bin/homeboy"));
    assert_eq!(
        check.details.get("configured_path").map(String::as_str),
        Some("/home/user/.cargo/bin/homeboy")
    );
    assert_eq!(
        check.details.get("bare_path").map(String::as_str),
        Some("/home/user/.local/bin/homeboy")
    );
    assert!(check
        .remediation
        .as_deref()
        .is_some_and(|value| value.contains("configured homeboy_path and bare `homeboy`")));
}

#[test]
fn lab_homeboy_path_shadow_accepts_matching_bare_homeboy() {
    let check = probes::homeboy_path_shadow_check(
        "homeboy-lab",
        "lab-server",
        "/home/user/.cargo/bin/homeboy",
        "0.229.9",
        &HomeboyProbe {
            version: "0.229.9".to_string(),
            path: Some("/home/user/.cargo/bin/homeboy".to_string()),
        },
        &probes::RemoteHomeboyCandidateProbe {
            path: Some("/home/user/.cargo/bin/homeboy".to_string()),
            version: Some("0.229.9".to_string()),
        },
        BTreeMap::new(),
    );

    assert!(check.is_none());
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
            ..Default::default()
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
        "/home/user/Developer",
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
        remote::default_artifact_root_for_home("/Users/user/"),
        Some("/Users/user/.local/share/homeboy/artifacts".to_string())
    );
}

#[test]
fn remote_default_artifact_root_rejects_empty_home() {
    assert_eq!(remote::default_artifact_root_for_home("  "), None);
}
