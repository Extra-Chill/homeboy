use super::super::*;
use types::RunnerDoctorStatus;

#[test]
fn shell_path_expr_expands_runner_home_relative_paths() {
    assert_eq!(
        common::shell_path_expr("~/.cache/homeboy/source"),
        "\"${HOME}\"/'.cache/homeboy/source'"
    );
    assert_eq!(common::shell_path_expr("~"), "\"${HOME}\"");
    assert_eq!(common::shell_path_expr("/tmp/source"), "'/tmp/source'");
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
fn declared_runtime_diagnostic_tools_are_grouped_by_source() {
    let manifest = serde_json::from_value(serde_json::json!({
        "schema": homeboy::core::agent_runtime_manifest::AGENT_RUNTIME_MANIFEST_SCHEMA,
        "id": "nodejs",
        "extension_id": "nodejs",
        "agent_task_executors": [],
        "materialization": {
            "diagnostics": {
                "tools": [
                    {
                        "tool": "node",
                        "managed_cache_source": "runtime",
                        "managed_cache_binary": "node",
                        "effective_binary_rule": "PATH",
                        "diagnostic_script": "node --version"
                    },
                    {
                        "tool": "npm",
                        "managed_cache_source": "runtime",
                        "managed_cache_binary": "npm",
                        "effective_binary_rule": "PATH",
                        "diagnostic_script": "npm --version"
                    },
                    {
                        "tool": "gh",
                        "managed_cache_source": "workflow",
                        "managed_cache_binary": "gh",
                        "effective_binary_rule": "PATH",
                        "diagnostic_script": "gh --version"
                    }
                ]
            }
        }
    }))
    .expect("runtime manifest");

    let specs = probes::declared_tool_specs_by_source_from_manifests(&[manifest]);
    let tools = specs.get("nodejs/nodejs").expect("source tools");
    let ids = tools
        .iter()
        .map(|spec| spec.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(ids, vec!["gh", "node", "npm"]);
    assert!(tools
        .iter()
        .all(|spec| spec.check_id.starts_with("tool.declared.nodejs/nodejs.")));
}

#[test]
fn declared_extension_diagnostic_tools_are_grouped_by_extension() {
    let extension = serde_json::from_value(serde_json::json!({
        "id": "wordpress",
        "name": "WordPress",
        "version": "1.0.0",
        "diagnostics": {
            "tools": [
                {
                    "id": "php",
                    "version_args": ["--version"],
                    "remediation": "Install PHP and ensure it is on PATH"
                },
                {
                    "id": "composer",
                    "version_args": ["--version"],
                    "remediation": "Install Composer and ensure it is on PATH"
                }
            ]
        }
    }))
    .expect("extension manifest");

    let specs = probes::declared_extension_tool_specs_by_source(&[extension]);
    let tools = specs.get("wordpress").expect("extension tools");
    let ids = tools
        .iter()
        .map(|spec| spec.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(ids, vec!["composer", "php"]);
    assert!(tools
        .iter()
        .all(|spec| spec.check_id.starts_with("tool.declared.wordpress.")));
}
