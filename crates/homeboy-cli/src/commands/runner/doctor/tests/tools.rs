use super::super::*;
use common::PathLookup;
use std::ffi::OsString;
use types::RunnerDoctorStatus;

#[cfg(unix)]
fn write_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, "#!/bin/sh\nexit 0\n").expect("write tool");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod tool");
}

/// The probe must answer from the filesystem and `PATH`, never from a shell.
///
/// #14374: the old probe shelled out to `sh -lc "command -v <tool>"`, so a single
/// broken line in `~/.profile` made dash exit before `command -v` ran and every
/// tool on a healthy host reported as absent. A directory that only this test
/// knows about cannot be reached through any login profile, so resolving from it
/// pins the shell-free lookup.
#[test]
#[cfg(unix)]
fn resolves_tools_from_path_without_a_shell() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tool = dir.path().join("homeboy-probe-fixture");
    write_executable(&tool);

    assert_eq!(
        common::resolve_in_path(
            "homeboy-probe-fixture",
            Some(&OsString::from(dir.path().as_os_str()))
        ),
        PathLookup::Found(common::display_path(&tool))
    );
}

#[test]
#[cfg(unix)]
fn path_lookup_skips_non_executable_and_non_file_candidates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path_var = OsString::from(dir.path().as_os_str());

    fs::write(dir.path().join("not-executable"), "plain data").expect("write data file");
    fs::create_dir(dir.path().join("a-directory")).expect("create directory");

    assert_eq!(
        common::resolve_in_path("not-executable", Some(&path_var)),
        PathLookup::NotFound
    );
    assert_eq!(
        common::resolve_in_path("a-directory", Some(&path_var)),
        PathLookup::NotFound
    );
}

#[test]
#[cfg(unix)]
fn path_lookup_searches_entries_in_order() {
    let first = tempfile::tempdir().expect("first dir");
    let second = tempfile::tempdir().expect("second dir");
    write_executable(&first.path().join("homeboy-probe-fixture"));
    write_executable(&second.path().join("homeboy-probe-fixture"));

    let path_var = env::join_paths([first.path(), second.path()]).expect("join paths");

    assert_eq!(
        common::resolve_in_path("homeboy-probe-fixture", Some(&path_var)),
        PathLookup::Found(common::display_path(
            first.path().join("homeboy-probe-fixture")
        ))
    );
}

/// A name containing a separator is a path to test, matching `command -v`.
#[test]
#[cfg(unix)]
fn path_lookup_treats_separated_names_as_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tool = dir.path().join("homeboy-probe-fixture");
    write_executable(&tool);

    let absolute = common::display_path(&tool);
    assert_eq!(
        common::resolve_in_path(&absolute, None),
        PathLookup::Found(absolute.clone())
    );
    assert_eq!(
        common::resolve_in_path(&common::display_path(dir.path().join("absent")), None),
        PathLookup::NotFound
    );
}

/// An unusable `PATH` is a failed lookup, not proof the tool is missing.
#[test]
fn unusable_path_reports_probe_failure_instead_of_absence() {
    assert_eq!(
        common::resolve_in_path("git", None),
        PathLookup::Unavailable("PATH is not set for the doctor process".to_string())
    );
    assert_eq!(
        common::resolve_in_path("git", Some(&OsString::from(""))),
        PathLookup::Unavailable("PATH is empty for the doctor process".to_string())
    );
}

#[test]
fn probe_failure_is_reported_separately_from_absence() {
    let missing = types::ToolProbe::not_found();
    assert!(!missing.probe_failed);
    assert_eq!(missing.error.as_deref(), Some(types::TOOL_NOT_FOUND_ERROR));

    let failed = types::ToolProbe::probe_failed("tool probe shell failed: bad profile".to_string());
    assert!(!failed.available);
    assert!(failed.probe_failed);

    let check = checks::required_tool_check("git", &failed);
    assert_eq!(check.status, RunnerDoctorStatus::Error);
    assert!(
        check.message.contains("could not be probed"),
        "probe failure must not claim the tool is missing: {}",
        check.message
    );
    assert!(check.message.contains("bad profile"));
    assert_eq!(
        check.details.get("probe_error").map(String::as_str),
        Some("tool probe shell failed: bad profile")
    );
    assert_eq!(
        check.remediation.as_deref(),
        Some(checks::PROBE_FAILURE_REMEDIATION)
    );
}

/// The tools doctor probes are the ones doctor itself already executes, so a
/// probe that disagrees with a direct spawn is reporting a lie.
#[test]
fn local_tool_probe_agrees_with_direct_execution() {
    let probe = probes::local_tool_probe("git", &[]);
    let spawnable = Command::new("git")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);

    assert_eq!(
        probe.available, spawnable,
        "probe reported available={} while direct execution reported {spawnable}: {:?}",
        probe.available, probe.error
    );
}

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
    let check = checks::required_tool_check("zip", &types::ToolProbe::not_found());

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

    assert_eq!(exit_code, 1);
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
