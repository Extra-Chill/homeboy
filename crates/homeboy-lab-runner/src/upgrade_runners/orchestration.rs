use super::*;
use crate as runner;
use crate::Runner;
use crate::RunnerExecOptions;
use crate::RunnerStatusReport;
use homeboy_core::Result;
use homeboy_lab_runner_contract::RunnerKind;
use homeboy_upgrade::upgrade::version_is_newer;
use homeboy_upgrade::upgrade::ExtensionUpgradeEntry;
use homeboy_upgrade::upgrade::InstallMethod;
use homeboy_upgrade::upgrade::RunnerUpgradeEntry;
use std::path::Path;

pub fn preflight_configured_runners_for_upgrade(
    _method_override: Option<InstallMethod>,
    _source_path: Option<&Path>,
    _explicit_source_path: bool,
    runner_targets: &[String],
    candidate_version: &str,
) -> Result<Vec<RunnerUpgradeEntry>> {
    let runners = runner_upgrade_targets(runner_targets)?;
    let mut failures = Vec::new();
    for runner in &runners {
        if runner.kind != RunnerKind::Ssh {
            continue;
        }

        let homeboy_path = runner
            .settings
            .homeboy_path
            .clone()
            .unwrap_or_else(|| "homeboy".to_string());
        if let Some(detail) = runner_manifest_preflight(runner, &homeboy_path, candidate_version) {
            failures.push(runner_upgrade_failure_entry(
                &runner.id,
                homeboy_path,
                None,
                1,
                detail,
            ));
        }
    }

    Ok(failures)
}

/// Query only the runner's manifest summary. This admission does not install,
/// refresh, or materialize extensions; those operations remain after promotion.
fn runner_manifest_preflight(
    runner: &Runner,
    homeboy_path: &str,
    candidate_version: &str,
) -> Option<String> {
    runner_manifest_preflight_with_executor(
        runner,
        homeboy_path,
        candidate_version,
        &mut runner::exec,
    )
}

fn runner_manifest_preflight_with_executor(
    runner: &Runner,
    homeboy_path: &str,
    candidate_version: &str,
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> Option<String> {
    let inventory_command = vec![
        homeboy_path.to_string(),
        "extension".to_string(),
        "list".to_string(),
        "--skip-ready-check".to_string(),
    ];
    let inventory_options = runner_manifest_query_options(runner, inventory_command);
    let (inventory_output, inventory_exit_code) = match exec(&runner.id, inventory_options) {
        Ok(result) => result,
        Err(error) => {
            return Some(format!(
                "runner manifest preflight could not query installed extension inventory: {}",
                error.message
            ));
        }
    };
    if inventory_exit_code != 0 {
        return Some(format!(
            "runner manifest preflight could not query installed extension inventory; recover with: {homeboy_path} extension list --skip-ready-check"
        ));
    }
    let extension_ids = match runner_extension_inventory(&inventory_output.stdout) {
        Ok(extension_ids) => extension_ids,
        Err(detail) => return Some(detail),
    };

    for extension_id in extension_ids {
        let options = runner_manifest_query_options(
            runner,
            vec![
                homeboy_path.to_string(),
                "extension".to_string(),
                "show".to_string(),
                extension_id.clone(),
                "--skip-ready-check".to_string(),
            ],
        );
        let (output, exit_code) = match exec(&runner.id, options) {
            Ok(result) => result,
            Err(error) => {
                return Some(format!(
                    "runner manifest preflight could not query installed extension `{extension_id}`: {}",
                    error.message
                ));
            }
        };
        if exit_code != 0 {
            return Some(format!("runner manifest preflight failed for installed extension `{extension_id}`; recover with: {homeboy_path} extension show {extension_id} --skip-ready-check"));
        }
        let requires_homeboy =
            match runner_extension_requires_homeboy(&output.stdout, &extension_id) {
                Ok(requires_homeboy) => requires_homeboy,
                Err(detail) => return Some(detail),
            };
        let Some(requires_homeboy) = requires_homeboy else {
            continue;
        };
        match homeboy_extension::evaluate_core_compatibility_for_version(
            Some(&requires_homeboy),
            None,
            candidate_version,
        ) {
            Ok(report) if report.status != "incompatible" => {}
            Ok(_) => return Some(format!("runner extension `{extension_id}` requires homeboy {requires_homeboy}, incompatible with selected controller {candidate_version}; recover with: {homeboy_path} extension update {extension_id}")),
            Err(error) => return Some(format!("runner extension `{extension_id}` has invalid homeboy compatibility declaration: {}; recover with: {homeboy_path} extension show {extension_id}", error.message)),
        }
    }
    None
}

fn runner_manifest_query_options(runner: &Runner, command: Vec<String>) -> RunnerExecOptions {
    let mut options = runner_exec_options(runner, command);
    options.print_handoff = false;
    options.mirror_evidence = false;
    options
}

fn runner_extension_inventory(stdout: &str) -> std::result::Result<Vec<String>, String> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim())
        .map_err(|error| format!("runner extension inventory was not valid JSON: {error}"))?;
    if value.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err("runner extension inventory did not report success".to_string());
    }
    let extensions = value
        .pointer("/data/extensions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "runner extension inventory did not contain data.extensions".to_string())?;

    extensions
        .iter()
        .map(|extension| {
            let id = extension
                .get("id")
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| {
                    "runner extension inventory contained an entry without an id".to_string()
                })?;
            if let Some(error) = extension.get("error") {
                let error = error.as_str().ok_or_else(|| {
                    format!("runner extension `{id}` inventory error was not a string")
                })?;
                let diagnostic = extension
                    .get("diagnostic")
                    .and_then(serde_json::Value::as_str)
                    .map(|diagnostic| format!(": {diagnostic}"))
                    .unwrap_or_default();
                return Err(format!(
                    "runner extension `{id}` manifest is invalid ({error}){diagnostic}"
                ));
            }
            Ok(id.to_string())
        })
        .collect()
}

fn runner_extension_requires_homeboy(
    stdout: &str,
    expected_extension_id: &str,
) -> std::result::Result<Option<String>, String> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|error| {
        format!("runner extension `{expected_extension_id}` manifest output was not valid JSON: {error}")
    })?;
    if value.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(format!(
            "runner extension `{expected_extension_id}` manifest query did not report success"
        ));
    }
    let extension = value
        .pointer("/data/extension")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            format!(
                "runner extension `{expected_extension_id}` manifest query did not contain data.extension"
            )
        })?;
    if extension.get("id").and_then(serde_json::Value::as_str) != Some(expected_extension_id) {
        return Err(format!(
            "runner extension manifest query returned the wrong extension for `{expected_extension_id}`"
        ));
    }
    let compatibility = extension
        .get("core_compatibility")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            format!(
                "runner extension `{expected_extension_id}` manifest query omitted core_compatibility"
            )
        })?;
    match compatibility.get("requires_homeboy") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(requirement)) => Ok(Some(requirement.clone())),
        Some(_) => Err(format!(
            "runner extension `{expected_extension_id}` compatibility requirement was not a string"
        )),
    }
}

#[cfg(test)]
mod manifest_preflight_tests {
    use super::*;
    use crate::{RunnerExecMode, RunnerExecOutput};
    use homeboy_core::server::RunnerSettings;
    use std::collections::HashMap;

    fn ssh_runner() -> Runner {
        Runner {
            id: "lab-a".to_string(),
            kind: RunnerKind::Ssh,
            server_id: Some("lab-a-server".to_string()),
            workspace_root: Some("/home/user/workspace".to_string()),
            settings: RunnerSettings::default(),
            env: HashMap::new(),
            secret_env: HashMap::new(),
            resources: HashMap::new(),
            policy: Default::default(),
        }
    }

    fn successful_output(
        runner_id: &str,
        command: Vec<String>,
        stdout: &str,
    ) -> (RunnerExecOutput, i32) {
        (
            RunnerExecOutput {
                variant: "exec",
                command: "runner.exec",
                runner_id: runner_id.to_string(),
                dry_run: false,
                mode: RunnerExecMode::DiagnosticSsh,
                argv: command,
                remote_cwd: "/home/user/workspace".to_string(),
                exit_code: 0,
                stdout: stdout.to_string(),
                stderr: String::new(),
                source_snapshot: None,
                job: None,
                runner_job: None,
                job_id: None,
                job_events: None,
                mirror_run_id: None,
                patch: None,
                mutation_artifacts: None,
                artifacts: Vec::new(),
                promoted_outputs: Vec::new(),
                structured_summaries: Vec::new(),
                metrics: None,
                capture: None,
                execution_record: None,
                runner_result: None,
                handoff: None,
                diagnostics: None,
            },
            0,
        )
    }

    #[test]
    fn reads_runner_manifest_compatibility_from_extension_show_json() {
        let stdout = r#"{"success":true,"data":{"extension":{"id":"rust","core_compatibility":{"requires_homeboy":">=2.0.0, <3.0.0"}}}}"#;
        assert_eq!(
            runner_extension_requires_homeboy(stdout, "rust")
                .expect("valid manifest query")
                .as_deref(),
            Some(">=2.0.0, <3.0.0")
        );
    }

    #[test]
    fn ignores_extension_show_without_a_compatibility_declaration() {
        let stdout = r#"{"success":true,"data":{"extension":{"id":"example","core_compatibility":{"status":"undeclared"}}}}"#;
        assert!(runner_extension_requires_homeboy(stdout, "example")
            .expect("valid undeclared compatibility")
            .is_none());
    }

    #[test]
    fn controller_only_extension_absent_from_runner_inventory_is_not_queried() {
        let runner = ssh_runner();
        let mut call_count = 0;
        let mut exec = |runner_id: &str, options: RunnerExecOptions| {
            call_count += 1;
            assert_eq!(runner_id, "lab-a");
            assert_eq!(
                options.command,
                ["homeboy", "extension", "list", "--skip-ready-check"]
            );
            assert!(!options.read_only_artifact_access);
            Ok(successful_output(
                runner_id,
                options.command,
                r#"{"success":true,"data":{"extensions":[]}}"#,
            ))
        };

        assert!(
            runner_manifest_preflight_with_executor(&runner, "homeboy", "2.1.0", &mut exec,)
                .is_none()
        );
        assert_eq!(
            call_count, 1,
            "an absent controller extension is never shown"
        );
    }

    #[test]
    fn runner_only_incompatible_extension_fails_closed() {
        let runner = ssh_runner();
        let mut call_count = 0;
        let mut exec = |runner_id: &str, options: RunnerExecOptions| {
            let stdout = match call_count {
                0 => {
                    assert_eq!(
                        options.command,
                        ["homeboy", "extension", "list", "--skip-ready-check"]
                    );
                    r#"{"success":true,"data":{"extensions":[{"id":"runner-only"}]}}"#
                }
                1 => {
                    assert_eq!(
                        options.command,
                        [
                            "homeboy",
                            "extension",
                            "show",
                            "runner-only",
                            "--skip-ready-check"
                        ]
                    );
                    r#"{"success":true,"data":{"extension":{"id":"runner-only","core_compatibility":{"requires_homeboy":">=3.0.0"}}}}"#
                }
                _ => panic!("unexpected manifest query"),
            };
            call_count += 1;
            Ok(successful_output(runner_id, options.command, stdout))
        };

        let rejection =
            runner_manifest_preflight_with_executor(&runner, "homeboy", "2.1.0", &mut exec)
                .expect("runner-only incompatible extension blocks promotion");
        assert!(rejection.contains("incompatible with selected controller 2.1.0"));
        assert_eq!(call_count, 2);
    }

    #[test]
    fn invalid_runner_inventory_manifest_fails_closed() {
        let runner = ssh_runner();
        let mut exec = |runner_id: &str, options: RunnerExecOptions| {
            Ok(successful_output(
                runner_id,
                options.command,
                r#"{"success":true,"data":{"extensions":[{"id":"broken","error":"manifest_invalid","diagnostic":"invalid requires.homeboy"}]}}"#,
            ))
        };

        let rejection =
            runner_manifest_preflight_with_executor(&runner, "homeboy", "2.1.0", &mut exec)
                .expect("invalid installed manifest blocks promotion");
        assert!(rejection.contains("runner extension `broken` manifest is invalid"));
        assert!(rejection.contains("invalid requires.homeboy"));
    }

    #[test]
    fn changed_runner_manifest_is_rejected_by_revalidation_without_source_sync() {
        let runner = ssh_runner();
        let manifests = [
            r#"{"success":true,"data":{"extension":{"id":"rust","core_compatibility":{"requires_homeboy":">=2.0.0"}}}}"#,
            r#"{"success":true,"data":{"extension":{"id":"rust","core_compatibility":{"requires_homeboy":">=3.0.0"}}}}"#,
        ];
        let mut call_count = 0;
        let mut exec = |runner_id: &str, options: RunnerExecOptions| {
            assert_eq!(runner_id, "lab-a");
            let stdout = if call_count % 2 == 0 {
                assert_eq!(
                    options.command,
                    ["homeboy", "extension", "list", "--skip-ready-check"]
                );
                r#"{"success":true,"data":{"extensions":[{"id":"rust"}]}}"#
            } else {
                assert_eq!(
                    options.command,
                    ["homeboy", "extension", "show", "rust", "--skip-ready-check"]
                );
                manifests[call_count / 2]
            };
            assert!(!options.read_only_artifact_access);
            call_count += 1;
            Ok(successful_output(runner_id, options.command, stdout))
        };

        assert!(
            runner_manifest_preflight_with_executor(&runner, "homeboy", "2.1.0", &mut exec,)
                .is_none()
        );
        let rejection =
            runner_manifest_preflight_with_executor(&runner, "homeboy", "2.1.0", &mut exec)
                .expect("changed manifest is rejected during revalidation");
        assert!(rejection.contains("incompatible with selected controller 2.1.0"));
        assert_eq!(call_count, 4);
    }
}

pub(crate) fn upgrade_configured_runners(
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&Path>,
    runner_targets: &[String],
    extension_updates: &[ExtensionUpgradeEntry],
    promotion_lease: Option<&homeboy_core::runtime_promotion::RuntimePromotionLease>,
) -> Result<(Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>)> {
    let runners = runner_upgrade_targets(runner_targets)?;
    if runners.is_empty() {
        return Ok((vec![], vec![]));
    }

    homeboy_core::log_status!(
        "upgrade",
        "Updating {} configured runner(s)...",
        runners.len()
    );
    let upgrade = || {
        Ok(upgrade_runners_with_executor(
            &runners,
            force,
            method_override,
            source_path,
            extension_updates,
            runner::exec,
            runner::status,
        ))
    };
    match promotion_lease {
        Some(lease) => lease.with_local_targets(
            &runners
                .iter()
                .map(|runner| runner.id.clone())
                .collect::<Vec<_>>(),
            upgrade,
        ),
        None => upgrade(),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "The runner-upgrade provider preserves its explicit controller identity and promotion authority inputs."
)]
pub fn upgrade_configured_runners_with_explicit_source_path(
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&Path>,
    explicit_source_path: bool,
    expected_controller_identity: Option<&str>,
    runner_targets: &[String],
    extension_updates: &[ExtensionUpgradeEntry],
    promotion_lease: Option<&homeboy_core::runtime_promotion::RuntimePromotionLease>,
) -> Result<(Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>)> {
    if !explicit_source_path {
        return upgrade_configured_runners(
            force,
            method_override,
            source_path,
            runner_targets,
            extension_updates,
            promotion_lease,
        );
    }

    let runners = runner_upgrade_targets(runner_targets)?;
    if runners.is_empty() {
        return Ok((vec![], vec![]));
    }

    homeboy_core::log_status!(
        "upgrade",
        "Updating {} configured runner(s)...",
        runners.len()
    );
    let upgrade = || {
        Ok(
            upgrade_runners_with_executor_and_source_materializer_with_expected_controller_identity(
                &runners,
                force,
                method_override,
                source_path,
                extension_updates,
                runner::exec,
                runner::status,
                materialize_explicit_runner_source_path,
                expected_controller_identity,
            ),
        )
    };
    match promotion_lease {
        Some(lease) => lease.with_local_targets(
            &runners
                .iter()
                .map(|runner| runner.id.clone())
                .collect::<Vec<_>>(),
            upgrade,
        ),
        None => upgrade(),
    }
}

pub(crate) fn runner_upgrade_targets(runner_targets: &[String]) -> Result<Vec<Runner>> {
    if !runner_targets.is_empty() {
        return runner_targets
            .iter()
            .map(|runner_id| runner::load(runner_id))
            .collect();
    }

    Ok(runner::list()?
        .into_iter()
        .filter(|runner| runner.kind == RunnerKind::Ssh)
        .collect())
}

pub fn upgrade_runners_with_executor(
    runners: &[Runner],
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&Path>,
    extension_updates: &[ExtensionUpgradeEntry],
    mut exec: impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
    status: impl Fn(&str) -> Result<RunnerStatusReport>,
) -> (Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>) {
    upgrade_runners_with_executor_and_source_materializer(
        runners,
        force,
        method_override,
        source_path,
        extension_updates,
        &mut exec,
        status,
        materialize_runner_source_path,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "Compatibility entry point exposes separately injectable upgrade operations for focused tests."
)]
pub fn upgrade_runners_with_executor_and_source_materializer(
    runners: &[Runner],
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&Path>,
    extension_updates: &[ExtensionUpgradeEntry],
    exec: impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
    status: impl Fn(&str) -> Result<RunnerStatusReport>,
    materialize_source_path: impl FnMut(&Runner, &Path) -> Result<String>,
) -> (Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>) {
    upgrade_runners_with_executor_and_source_materializer_with_expected_controller_identity(
        runners,
        force,
        method_override,
        source_path,
        extension_updates,
        exec,
        status,
        materialize_source_path,
        None,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "The internal operation keeps expected controller identity explicit through source materialization."
)]
fn upgrade_runners_with_executor_and_source_materializer_with_expected_controller_identity(
    runners: &[Runner],
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&Path>,
    extension_updates: &[ExtensionUpgradeEntry],
    mut exec: impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
    status: impl Fn(&str) -> Result<RunnerStatusReport>,
    mut materialize_source_path: impl FnMut(&Runner, &Path) -> Result<String>,
    expected_controller_identity: Option<&str>,
) -> (Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>) {
    upgrade_runners_with_executor_source_materializer_and_path_updater_with_expected_controller_identity(
        runners,
        force,
        method_override,
        source_path,
        extension_updates,
        &mut exec,
        status,
        &mut materialize_source_path,
        update_runner_homeboy_path,
        expected_controller_identity,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "The internal upgrade operation retains explicit controller identity and path mutation boundaries."
)]
pub(crate) fn upgrade_runners_with_executor_source_materializer_and_path_updater_with_expected_controller_identity(
    runners: &[Runner],
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&Path>,
    extension_updates: &[ExtensionUpgradeEntry],
    mut exec: impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
    status: impl Fn(&str) -> Result<RunnerStatusReport>,
    mut materialize_source_path: impl FnMut(&Runner, &Path) -> Result<String>,
    mut update_homeboy_path: impl FnMut(&str, &str) -> Result<()>,
    expected_controller_identity: Option<&str>,
) -> (Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>) {
    upgrade_runners_with_executor_source_materializer_path_updater_and_reconnector_with_expected_controller_identity(
        runners,
        force,
        method_override,
        source_path,
        extension_updates,
        &mut exec,
        status,
        reconnect_runner_daemon,
        &mut materialize_source_path,
        &mut update_homeboy_path,
        expected_controller_identity,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "The orchestration boundary keeps every mutating operation explicit for deterministic runner upgrade recovery."
)]
pub(crate) fn upgrade_runners_with_executor_source_materializer_path_updater_and_reconnector_with_expected_controller_identity(
    runners: &[Runner],
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&Path>,
    extension_updates: &[ExtensionUpgradeEntry],
    mut exec: impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
    status: impl Fn(&str) -> Result<RunnerStatusReport>,
    mut reconnect_stale_daemon: impl FnMut(&str) -> Result<(String, Option<String>)>,
    mut materialize_source_path: impl FnMut(&Runner, &Path) -> Result<String>,
    mut update_homeboy_path: impl FnMut(&str, &str) -> Result<()>,
    expected_controller_identity: Option<&str>,
) -> (Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>) {
    let mut updated = Vec::new();
    let mut skipped = Vec::new();

    for runner in runners {
        let entry = upgrade_runner_with_executor(
            runner,
            force,
            method_override,
            source_path,
            extension_updates,
            &mut exec,
            &status,
            &mut reconnect_stale_daemon,
            &mut materialize_source_path,
            &mut update_homeboy_path,
            expected_controller_identity,
        );
        if entry.success {
            homeboy_core::log_status!(
                "upgrade",
                "  {} {}",
                entry.runner_id,
                runner_upgrade_summary(&entry)
            );
            updated.push(entry);
        } else {
            homeboy_core::log_status!("upgrade", "  {} skipped: {}", entry.runner_id, entry.detail);
            skipped.push(entry);
        }
    }

    (updated, skipped)
}

#[expect(
    clippy::too_many_arguments,
    reason = "A single runner upgrade retains explicit diagnostic inputs and operations to preserve callback order."
)]
pub fn upgrade_runner_with_executor(
    runner: &Runner,
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&Path>,
    extension_updates: &[ExtensionUpgradeEntry],
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
    status: &impl Fn(&str) -> Result<RunnerStatusReport>,
    reconnect_stale_daemon: &mut impl FnMut(&str) -> Result<(String, Option<String>)>,
    materialize_source_path: &mut impl FnMut(&Runner, &Path) -> Result<String>,
    update_homeboy_path: &mut impl FnMut(&str, &str) -> Result<()>,
    expected_controller_identity: Option<&str>,
) -> RunnerUpgradeEntry {
    let original_homeboy_path = runner
        .settings
        .homeboy_path
        .clone()
        .unwrap_or_else(|| "homeboy".to_string());
    let previous_version = runner_homeboy_version(runner, &original_homeboy_path, exec)
        .ok()
        .flatten();
    if is_managed_immutable_homeboy_path(runner, &original_homeboy_path) {
        return refresh_managed_immutable_runner(
            runner,
            original_homeboy_path,
            previous_version,
            extension_updates,
            exec,
        );
    }
    let expected_build_identity = expected_controller_identity
        .map(str::to_string)
        .or_else(|| {
            (method_override == Some(InstallMethod::Source))
                .then(|| source_path.and_then(source_checkout_build_identity))
                .flatten()
        });
    let selected_source_revision = source_path.and_then(source_checkout_revision);
    let selected_source_url = source_path.and_then(homeboy_core::git::remote_origin_url);
    let command_source_path = match runner_upgrade_source_path(
        runner,
        method_override,
        source_path,
        materialize_source_path,
    ) {
        Ok(path) => path,
        Err(err) => {
            return runner_upgrade_failure_entry(
                &runner.id,
                original_homeboy_path,
                previous_version,
                1,
                err.message,
            );
        }
    };
    if let Some(err) = prepare_runner_source_checkout_for_upgrade(
        runner,
        method_override,
        command_source_path.as_deref(),
        exec,
    ) {
        return runner_upgrade_failure_entry(
            &runner.id,
            original_homeboy_path,
            previous_version,
            1,
            err,
        );
    }
    let mut upgrade_homeboy_path = original_homeboy_path.clone();
    let mut path_update_detail = None;
    let upgrade = exec(
        &runner.id,
        runner_exec_options(
            runner,
            runner_upgrade_command(
                &upgrade_homeboy_path,
                force,
                method_override,
                command_source_path.as_deref(),
            ),
        ),
    );

    // Both the non-zero-exit and hard-error upgrade arms attempt the same
    // recovery: realign the runner homeboy_path, then retry the upgrade.
    let recovery_outcome = match upgrade {
        Ok((output, exit_code)) if exit_code == 0 => {
            Ok((exit_code, runner_upgrade_detail(&output), None))
        }
        Ok((output, exit_code)) => recover_and_retry_failed_upgrade(
            runner,
            force,
            method_override,
            command_source_path.as_deref(),
            &original_homeboy_path,
            previous_version.as_deref(),
            expected_build_identity.as_deref(),
            selected_source_revision.as_deref(),
            selected_source_url.as_deref(),
            FailedUpgradeOutcome {
                exit_code,
                detail: runner_upgrade_detail(&output),
            },
            update_homeboy_path,
            exec,
        ),
        Err(err) => recover_and_retry_failed_upgrade(
            runner,
            force,
            method_override,
            command_source_path.as_deref(),
            &original_homeboy_path,
            previous_version.as_deref(),
            expected_build_identity.as_deref(),
            selected_source_revision.as_deref(),
            selected_source_url.as_deref(),
            FailedUpgradeOutcome {
                exit_code: 1,
                detail: err.message,
            },
            update_homeboy_path,
            exec,
        ),
    };

    let (exit_code, detail) = match recovery_outcome {
        Ok((exit_code, detail, recovery)) => {
            if let Some(recovery) = recovery {
                upgrade_homeboy_path = recovery.homeboy_path;
                path_update_detail = Some(recovery.detail);
            }
            (exit_code, detail)
        }
        Err(mut entry) => {
            entry.previous_version = previous_version;
            return entry;
        }
    };

    let mut homeboy_path = upgrade_homeboy_path.clone();
    let mut new_version = runner_homeboy_version(runner, &upgrade_homeboy_path, exec)
        .ok()
        .flatten();
    let mut source_path_realigned = false;
    if let Some(realignment) = source_upgrade_homeboy_path_realignment(
        runner,
        &original_homeboy_path,
        method_override,
        command_source_path.as_deref(),
        &upgrade_homeboy_path,
        new_version.as_deref(),
        expected_build_identity.as_deref(),
        exec,
    ) {
        match update_homeboy_path(&runner.id, &realignment.homeboy_path) {
            Ok(()) => {
                homeboy_path = realignment.homeboy_path;
                new_version = Some(realignment.version);
                source_path_realigned = true;
                path_update_detail = Some(realignment.detail);
            }
            Err(err) => {
                path_update_detail = Some(format!(
                    "source-built runner homeboy_path realignment failed: {}",
                    err.message
                ));
            }
        }
    }
    let mut bare_homeboy_version = None;
    let alignment = if !source_path_realigned && is_auto_realignable_homeboy_path(&homeboy_path) {
        bare_homeboy_version = runner_bare_homeboy_version(runner, &upgrade_homeboy_path, exec);
        runner_homeboy_path_alignment(
            &runner.id,
            &homeboy_path,
            new_version.as_deref(),
            bare_homeboy_version.as_deref(),
        )
    } else {
        None
    };
    let mut path_drift = alignment
        .as_ref()
        .and_then(|alignment| alignment.drift.clone());

    if let Some(alignment) = alignment {
        if alignment.update_to.is_none()
            && is_disposable_lab_workspace_homeboy_path(&homeboy_path)
            && matches!(
                (new_version.as_deref(), bare_homeboy_version.as_deref()),
                (Some(configured), Some(bare)) if version_is_newer(configured, bare)
            )
        {
            let repair = repair_stale_bare_homeboy_after_upgrade(
                runner,
                force,
                method_override,
                command_source_path.as_deref(),
                new_version.as_deref().unwrap_or_default(),
                exec,
            );
            bare_homeboy_version = repair.bare_version;
            path_drift = repair.path_drift;
            path_update_detail = Some(repair.detail);
        } else {
            apply_runner_homeboy_path_alignment(
                runner,
                &runner.id,
                alignment,
                &original_homeboy_path,
                bare_homeboy_version.as_deref(),
                &mut homeboy_path,
                &mut new_version,
                &mut path_drift,
                &mut path_update_detail,
                update_homeboy_path,
                expected_build_identity.as_deref(),
                exec,
            );
        }
    }

    let mut source_identity_drift = if path_drift.is_none() {
        runner_build_identity_drift(
            runner,
            &homeboy_path,
            expected_build_identity.as_deref(),
            exec,
        )
    } else {
        None
    };
    if path_drift.is_none() {
        path_drift = source_identity_drift.clone();
    }
    let (extensions_synced, mut extensions_skipped, mut extensions_failed) =
        if let Some(drift) = path_drift.as_deref() {
            (
                Vec::new(),
                defer_runner_extensions_for_binary_drift(extension_updates, drift),
                Vec::new(),
            )
        } else {
            sync_runner_extensions(runner, &homeboy_path, extension_updates, exec)
        };
    if path_drift.is_some() && is_auto_realignable_homeboy_path(&homeboy_path) {
        let refreshed_bare_homeboy_version =
            runner_bare_homeboy_version(runner, &homeboy_path, exec);
        if refreshed_bare_homeboy_version.is_some() {
            bare_homeboy_version = refreshed_bare_homeboy_version;
            match runner_homeboy_path_alignment(
                &runner.id,
                &homeboy_path,
                new_version.as_deref(),
                bare_homeboy_version.as_deref(),
            ) {
                Some(alignment) => {
                    path_drift = alignment.drift.clone();
                    apply_runner_homeboy_path_alignment(
                        runner,
                        &runner.id,
                        alignment,
                        &original_homeboy_path,
                        bare_homeboy_version.as_deref(),
                        &mut homeboy_path,
                        &mut new_version,
                        &mut path_drift,
                        &mut path_update_detail,
                        update_homeboy_path,
                        expected_build_identity.as_deref(),
                        exec,
                    );
                }
                None => {
                    path_drift = None;
                }
            }
        }
    }
    if bare_homeboy_version.is_none() && !source_path_realigned {
        bare_homeboy_version = runner_bare_homeboy_version(runner, &homeboy_path, exec);
    }
    if path_drift.is_none() && !source_path_realigned {
        if let Some(alignment) = runner_homeboy_path_alignment(
            &runner.id,
            &homeboy_path,
            new_version.as_deref(),
            bare_homeboy_version.as_deref(),
        ) {
            path_drift = alignment.drift.clone();
            apply_runner_homeboy_path_alignment(
                runner,
                &runner.id,
                alignment,
                &original_homeboy_path,
                bare_homeboy_version.as_deref(),
                &mut homeboy_path,
                &mut new_version,
                &mut path_drift,
                &mut path_update_detail,
                update_homeboy_path,
                expected_build_identity.as_deref(),
                exec,
            );
        }
    }
    let local_version_drift = runner_local_version_drift(
        &runner.id,
        &homeboy_path,
        previous_version.as_deref(),
        new_version.as_deref(),
    );
    if path_drift.is_none() {
        path_drift = local_version_drift;
    }
    // PATH alignment is semver-only. Re-check the final configured executable
    // so an equal-version bare binary cannot erase source identity divergence.
    source_identity_drift = runner_build_identity_drift(
        runner,
        &homeboy_path,
        expected_build_identity.as_deref(),
        exec,
    );
    if path_drift.is_none() {
        path_drift = source_identity_drift.clone();
    }

    defer_extension_failures_for_path_drift(
        path_drift.as_deref(),
        &mut extensions_skipped,
        &mut extensions_failed,
    );
    let selected_materialized_binary = (path_drift.is_some()
        && !selected_source_url
            .as_deref()
            .is_some_and(source_url_is_runner_reachable))
    .then(|| {
        verified_materialized_source_binary(
            runner,
            command_source_path.as_deref(),
            expected_build_identity.as_deref(),
            exec,
        )
    })
    .flatten();
    let recovery_commands = runner_recovery_commands(
        &runner.id,
        &homeboy_path,
        path_drift.as_ref(),
        new_version.as_deref(),
        bare_homeboy_version.as_deref(),
        selected_source_revision.as_deref(),
        selected_source_url.as_deref(),
        selected_materialized_binary.as_deref(),
        expected_build_identity.as_deref(),
    );
    let mut stale_daemon_repair_detail = None;
    let mut stale_daemon = runner_stale_daemon(runner, status);
    let mut daemon_previous_version = None;
    let mut daemon_new_version = None;
    if stale_daemon.is_some() && path_drift.is_none() {
        let daemon_before = stale_daemon
            .as_ref()
            .map(|daemon| daemon.session_homeboy_version.clone())
            .unwrap_or_else(|| "unknown".to_string());
        daemon_previous_version = Some(daemon_before.clone());
        match reconnect_stale_daemon(&runner.id) {
            Ok((detail, observed_version)) => {
                daemon_new_version = observed_version.clone();
                let daemon_after = observed_version.as_deref().unwrap_or("unknown");
                if matches!(
                    (observed_version.as_deref(), new_version.as_deref()),
                    (Some(observed), Some(expected))
                        if crate::connection::versions_match(observed, expected)
                ) {
                    stale_daemon = None;
                    stale_daemon_repair_detail = Some(format!(
                        "runner daemon identity: {daemon_before} -> {daemon_after}; {detail}"
                    ));
                } else {
                    stale_daemon_repair_detail = Some(format!(
                        "runner daemon reconnect did not converge: expected {}, observed {daemon_after}; {detail}",
                        new_version.as_deref().unwrap_or("the configured runner version")
                    ));
                }
            }
            Err(err) => {
                stale_daemon_repair_detail = Some(format!(
                    "automatic stale runner daemon restart failed: {}",
                    err.message
                ));
            }
        }
    }
    let upgraded = source_path_realigned
        || match (previous_version.as_deref(), new_version.as_deref()) {
            (Some(previous), Some(new)) => new != previous,
            _ => false,
        };
    let success = new_version.is_some()
        && path_drift.is_none()
        && extensions_failed.is_empty()
        && stale_daemon.is_none();
    let detail =
        runner_version_report_detail(detail, previous_version.as_deref(), new_version.as_deref());
    let detail = runner_upgrade_final_detail(
        &runner.id,
        detail,
        &homeboy_path,
        path_update_detail.as_deref(),
        stale_daemon_repair_detail.as_deref(),
        path_drift.as_deref(),
        stale_daemon.as_ref(),
        &extensions_skipped,
        &extensions_failed,
    );

    RunnerUpgradeEntry {
        runner_id: runner.id.clone(),
        homeboy_path,
        success,
        upgraded,
        previous_version,
        new_version,
        bare_homeboy_version,
        path_drift,
        recovery_commands,
        extensions_synced,
        extensions_skipped,
        extensions_failed,
        stale_daemon,
        daemon_previous_version,
        daemon_new_version,
        exit_code,
        detail,
    }
}

fn refresh_managed_immutable_runner(
    runner: &Runner,
    previous_homeboy_path: String,
    previous_version: Option<String>,
    extension_updates: &[ExtensionUpgradeEntry],
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> RunnerUpgradeEntry {
    let recovery_commands = managed_immutable_runner_recovery_commands(&runner.id);
    let Some(controller_commit) = homeboy_product_identity::build_identity().git_commit else {
        return managed_immutable_runner_failure_entry(
            &runner.id,
            previous_homeboy_path,
            previous_version,
            1,
            "managed immutable runner refresh requires the controller's immutable commit identity; no mutable version-tag recovery action was emitted".to_string(),
        );
    };
    let options = crate::HomeboyBinaryRefreshOptions {
        runner_id: runner.id.clone(),
        mode: crate::HomeboyBinaryRefreshMode::Materialize,
        source: None,
        git_ref: Some(controller_commit),
        target_dir: None,
        reconnect: true,
        force: false,
        allow_downgrade: false,
        dry_run: false,
    };
    let (refreshed, exit_code) = match crate::refresh_homeboy_binary(options) {
        Ok(result) => result,
        Err(error) => {
            return managed_immutable_runner_failure_entry(
                &runner.id,
                previous_homeboy_path,
                previous_version,
                1,
                format!(
                    "managed immutable runner refresh failed: {}; recover with {}",
                    error.message,
                    recovery_commands.join(" && ")
                ),
            );
        }
    };
    if !managed_refresh_can_reconcile(
        exit_code,
        refreshed.daemon_refreshed,
        refreshed.failure.is_some(),
    ) {
        return RunnerUpgradeEntry {
            runner_id: runner.id.clone(),
            homeboy_path: refreshed.selected_binary_path,
            success: false,
            upgraded: false,
            previous_version,
            new_version: None,
            bare_homeboy_version: None,
            path_drift: Some("managed immutable runner refresh did not converge".to_string()),
            recovery_commands,
            extensions_synced: Vec::new(),
            extensions_skipped: Vec::new(),
            extensions_failed: Vec::new(),
            stale_daemon: None,
            daemon_previous_version: None,
            daemon_new_version: None,
            exit_code,
            detail: format!(
                "managed immutable runner refresh failed: {:?}",
                refreshed.failure
            ),
        };
    }
    let reconciled = match runner::reconcile_status(&runner.id) {
        Ok(report) => report,
        Err(error) => {
            return managed_immutable_runner_failure_entry(
                &runner.id,
                refreshed.selected_binary_path,
                previous_version,
                1,
                format!(
                    "managed immutable runner refresh completed but reconciliation failed: {}; recover with {}",
                    error.message,
                    recovery_commands.join(" && ")
                ),
            );
        }
    };
    let homeboy_path = refreshed.selected_binary_path;
    let new_version = runner_homeboy_version(runner, &homeboy_path, exec)
        .ok()
        .flatten();
    let (extensions_synced, extensions_skipped, extensions_failed) =
        sync_runner_extensions(runner, &homeboy_path, extension_updates, exec);
    let admission_ready = managed_immutable_admission_ready(&reconciled);
    let stale_daemon = reconciled.stale_daemon.map(runner_daemon_drift_entry);
    let success = new_version.is_some()
        && admission_ready
        && stale_daemon.is_none()
        && extensions_failed.is_empty();
    RunnerUpgradeEntry {
        runner_id: runner.id.clone(),
        homeboy_path: homeboy_path.clone(),
        success,
        upgraded: homeboy_path != previous_homeboy_path,
        previous_version,
        new_version,
        bare_homeboy_version: None,
        path_drift: (!success).then(|| {
            "managed immutable runner did not satisfy binary, daemon, and admission convergence"
                .to_string()
        }),
        recovery_commands: (!success).then_some(recovery_commands).unwrap_or_default(),
        extensions_synced,
        extensions_skipped,
        extensions_failed,
        stale_daemon,
        daemon_previous_version: None,
        daemon_new_version: None,
        exit_code: i32::from(!success),
        detail: format!(
            "managed immutable runner refreshed through controller-owned promotion, daemon rotation, and reconciliation; admission ready: {admission_ready}"
        ),
    }
}

pub(super) fn managed_immutable_admission_ready(status: &crate::RunnerStatusReport) -> bool {
    status.admission_summary(0).accepting_jobs
}

pub(super) fn managed_refresh_can_reconcile(
    exit_code: i32,
    daemon_refreshed: bool,
    refresh_failed: bool,
) -> bool {
    !refresh_failed && (exit_code == 0 || daemon_refreshed)
}

fn managed_immutable_runner_failure_entry(
    runner_id: &str,
    homeboy_path: String,
    previous_version: Option<String>,
    exit_code: i32,
    detail: String,
) -> RunnerUpgradeEntry {
    RunnerUpgradeEntry {
        runner_id: runner_id.to_string(),
        homeboy_path,
        success: false,
        upgraded: false,
        previous_version,
        new_version: None,
        bare_homeboy_version: None,
        path_drift: Some("managed immutable runner refresh did not converge".to_string()),
        recovery_commands: managed_immutable_runner_recovery_commands(runner_id),
        extensions_synced: Vec::new(),
        extensions_skipped: Vec::new(),
        extensions_failed: Vec::new(),
        stale_daemon: None,
        daemon_previous_version: None,
        daemon_new_version: None,
        exit_code,
        detail,
    }
}
