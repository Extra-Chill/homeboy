use super::*;
use crate as runner;
use crate::Runner;
use crate::RunnerExecOptions;
use crate::RunnerStatusReport;
use crate::{RunnerBinaryConvergence, RunnerBinaryConvergenceStatus};
use homeboy_core::Result;
use homeboy_product_identity::BuildIdentity;
use homeboy_runner_contract::RunnerKind;
use homeboy_upgrade::upgrade::ExtensionUpgradeEntry;
use homeboy_upgrade::upgrade::RunnerUpgradeEntry;

pub fn preflight_configured_runners_for_upgrade(
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
        if let Some(detail) = runner_live_jobs_preflight(&runner.id, candidate_version) {
            let mut entry = runner_upgrade_failure_entry(&runner.id, homeboy_path, None, 1, detail);
            entry.recovery_commands =
                vec![format!("homeboy upgrade --upgrade-runner {}", runner.id)];
            failures.push(entry);
            continue;
        }
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

/// Refuse a version change while the runner's daemon still runs jobs (#15995).
///
/// A controller on the candidate version can no longer admit results from a
/// daemon on the old one, and the runner cannot rotate that daemon until its
/// jobs drain. Upgrading anyway strands every in-flight job: its result is
/// rejected as stale and the runner refuses rotation until it ends. The daemon's
/// own live job view is the authority; the check fails open when it cannot be
/// read, because the manifest preflight then reports the unreachable runner.
fn runner_live_jobs_preflight(runner_id: &str, candidate_version: &str) -> Option<String> {
    let summary = runner::runner_admission_snapshot(runner_id).ok()?.summary;
    live_jobs_preflight_detail(
        runner_id,
        summary.daemon_build_identity.as_deref(),
        summary.live_daemon_job_count,
        candidate_version,
    )
}

fn live_jobs_preflight_detail(
    runner_id: &str,
    daemon_build_identity: Option<&str>,
    live_jobs: Option<usize>,
    candidate_version: &str,
) -> Option<String> {
    let live_jobs = live_jobs.filter(|count| *count > 0)?;
    let daemon_version = daemon_build_identity.and_then(build_identity_version);
    if daemon_version == Some(candidate_version.trim_start_matches('v')) {
        return None;
    }
    Some(format!(
        "runner `{runner_id}` has {live_jobs} live job(s) on daemon `{}`; upgrading to {candidate_version} would strand them. Wait for them to finish (`homeboy runner job list {runner_id}`) or cancel them (`homeboy runner job cancel {runner_id} <job-id>`), then re-run `homeboy upgrade`",
        daemon_build_identity.unwrap_or("unknown"),
    ))
}

/// `homeboy 0.428.0+19c5f4e5` -> `0.428.0`.
fn build_identity_version(identity: &str) -> Option<&str> {
    identity
        .trim()
        .strip_prefix("homeboy ")?
        .trim_start_matches('v')
        .split('+')
        .next()
        .filter(|version| !version.is_empty())
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
    // Resolved once: every query in this preflight is read-only and must use
    // the same transport the runner currently admits.
    let allow_diagnostic_ssh = diagnostic_ssh_is_permitted(&runner.id);
    let inventory_command = vec![
        homeboy_path.to_string(),
        "extension".to_string(),
        "list".to_string(),
    ];
    let inventory_options =
        runner_manifest_query_options(runner, inventory_command, allow_diagnostic_ssh);
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
            "runner manifest preflight could not query installed extension inventory; recover with: {homeboy_path} extension list"
        ));
    }
    let inventory = match runner_extension_inventory(&inventory_output.stdout) {
        Ok(inventory) => inventory,
        Err(detail) => return Some(detail),
    };
    for extension_id in inventory.candidate_owned {
        homeboy_core::log_status!(
            "upgrade",
            "  {} extension `{}` compatibility deferred to selected candidate",
            runner.id,
            extension_id
        );
    }

    for (extension_id, requires_homeboy) in inventory.readable {
        let Some(requires_homeboy) = requires_homeboy else {
            continue;
        };
        match homeboy_extension_contract::evaluate_core_compatibility_for_version(
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

fn runner_manifest_query_options(
    runner: &Runner,
    command: Vec<String>,
    allow_diagnostic_ssh: bool,
) -> RunnerExecOptions {
    let mut options = runner_exec_options(runner, command);
    options.print_handoff = false;
    options.mirror_evidence = false;
    options.allow_diagnostic_ssh = allow_diagnostic_ssh;
    // This preflight only reads the runner's extension inventory; it has no
    // need for SSH specifically. `allow_diagnostic_ssh` is a *force*, not a
    // permit, so leaving it set made the query demand the one transport a
    // runner with a fresh connected daemon is documented to refuse — failing
    // the preflight and aborting the whole controller upgrade (#13686).
    //
    // The flag cannot simply be cleared: a disconnected SSH runner has no
    // daemon session, so transport selection would fall through to
    // `Unavailable` and lose the path it relies on today. Mirror the runner's
    // own admission rule instead.
    options
}

/// Whether diagnostic SSH is currently the runner's admissible transport.
///
/// Mirrors the execution layer's own rule: SSH is for a disconnected or
/// non-fresh daemon, and a fresh connected daemon must be reached through
/// normal exec so admission stays attributed to it.
///
/// An unreadable status preserves the historical behaviour rather than
/// inventing a refusal: preflight then fails on the exec itself, with the
/// transport error the operator can act on.
fn diagnostic_ssh_is_permitted(runner_id: &str) -> bool {
    match crate::connection::status(runner_id) {
        Ok(status) => diagnostic_ssh_is_permitted_for(&status),
        Err(_) => true,
    }
}

/// The transport decision itself, over an already-resolved status.
fn diagnostic_ssh_is_permitted_for(status: &RunnerStatusReport) -> bool {
    !status.connected || !status.daemon_fresh_for_admission()
}

#[derive(Debug, PartialEq, Eq)]
struct RunnerExtensionInventory {
    /// Readable extensions with their declared `requires.homeboy`, if any.
    readable: Vec<(String, Option<String>)>,
    candidate_owned: Vec<String>,
}

fn runner_extension_inventory(
    stdout: &str,
) -> std::result::Result<RunnerExtensionInventory, String> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim())
        .map_err(|error| format!("runner extension inventory was not valid JSON: {error}"))?;
    if value.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err("runner extension inventory did not report success".to_string());
    }
    let extensions = value
        .pointer("/data/entities")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "runner extension inventory did not contain data.entities".to_string())?;

    let mut inventory = RunnerExtensionInventory {
        readable: Vec::new(),
        candidate_owned: Vec::new(),
    };
    for extension in extensions {
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
            // The selected runner refresh installs the binary and
            // synchronizes extensions only after the new binary is active. A
            // schema the legacy binary cannot deserialize therefore belongs to
            // candidate validation; requiring the legacy reader here creates a
            // permanent bootstrap cycle. Other invalid-manifest categories stay
            // fail-closed because a new binary cannot make malformed or missing
            // input trustworthy.
            if error == "manifest_deserialize_incompatible" {
                inventory.candidate_owned.push(id.to_string());
                continue;
            }
            let diagnostic = extension
                .get("diagnostic")
                .and_then(serde_json::Value::as_str)
                .map(|diagnostic| format!(": {diagnostic}"))
                .unwrap_or_default();
            return Err(format!(
                "runner extension `{id}` manifest is invalid ({error}){diagnostic}"
            ));
        }
        let requires_homeboy = match extension
            .get("core_compatibility")
            .and_then(serde_json::Value::as_object)
            .map(|compatibility| compatibility.get("requires_homeboy"))
        {
            None => {
                return Err(format!(
                    "runner extension `{id}` inventory omitted core_compatibility"
                ))
            }
            Some(None | Some(serde_json::Value::Null)) => None,
            Some(Some(serde_json::Value::String(requirement))) => Some(requirement.clone()),
            Some(Some(_)) => {
                return Err(format!(
                    "runner extension `{id}` compatibility requirement was not a string"
                ))
            }
        };
        inventory.readable.push((id.to_string(), requires_homeboy));
    }
    Ok(inventory)
}

#[cfg(test)]
mod live_jobs_preflight_tests {
    use super::*;

    #[test]
    fn a_version_change_is_refused_while_the_daemon_runs_jobs() {
        let detail = live_jobs_preflight_detail(
            "homeboy-lab",
            Some("homeboy 0.428.0+19c5f4e54f8e"),
            Some(2),
            "0.428.2",
        )
        .expect("live jobs on an outgoing daemon block the upgrade");
        assert!(detail.contains("2 live job(s)"), "{detail}");
        assert!(detail.contains("homeboy 0.428.0+19c5f4e54f8e"), "{detail}");
        assert!(
            detail.contains("homeboy runner job list homeboy-lab"),
            "{detail}"
        );
        assert!(
            detail.contains("homeboy runner job cancel homeboy-lab"),
            "{detail}"
        );
        assert!(
            !detail.contains("refresh-homeboy"),
            "the recovery must not be a rotation the live jobs also block: {detail}"
        );
    }

    #[test]
    fn an_unknown_daemon_version_with_live_jobs_is_refused() {
        assert!(live_jobs_preflight_detail("lab", None, Some(1), "0.428.2").is_some());
    }

    #[test]
    fn an_idle_daemon_or_same_version_repair_is_admitted() {
        let identity = Some("homeboy 0.428.0+19c5f4e5");
        assert_eq!(
            live_jobs_preflight_detail("lab", identity, Some(0), "0.428.2"),
            None
        );
        assert_eq!(
            live_jobs_preflight_detail("lab", identity, None, "0.428.2"),
            None,
            "an unobservable job view is reported by the manifest preflight"
        );
        assert_eq!(
            live_jobs_preflight_detail("lab", identity, Some(3), "0.428.0"),
            None,
            "the daemon already runs the candidate version"
        );
        assert_eq!(
            live_jobs_preflight_detail("lab", identity, Some(3), "v0.428.0"),
            None
        );
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
    fn reads_runner_manifest_compatibility_from_the_extension_inventory() {
        let stdout = r#"{"success":true,"data":{"entities":[{"id":"rust","core_compatibility":{"requires_homeboy":">=2.0.0, <3.0.0"}},{"id":"example","core_compatibility":{"status":"undeclared"}}]}}"#;
        assert_eq!(
            runner_extension_inventory(stdout)
                .expect("valid inventory")
                .readable,
            vec![
                ("rust".to_string(), Some(">=2.0.0, <3.0.0".to_string())),
                ("example".to_string(), None),
            ]
        );
        let missing = r#"{"success":true,"data":{"entities":[{"id":"rust"}]}}"#;
        assert!(runner_extension_inventory(missing)
            .expect_err("an entry without core_compatibility fails closed")
            .contains("omitted core_compatibility"));
    }

    fn status_report(connected: bool) -> RunnerStatusReport {
        RunnerStatusReport {
            runner_id: "lab-a".to_string(),
            connected,
            state: if connected {
                crate::session::RunnerSessionState::Connected
            } else {
                crate::session::RunnerSessionState::Disconnected
            },
            session: None,
            stale_daemon: None,
            configured_job_binary_build_identity: None,
            daemon_freshness: connected.then_some(homeboy_core::daemon::DaemonFreshnessReport {
                fresh: true,
                stale_reason_code: None,
                restartable: false,
                lease_id: Some("lease-test".to_string()),
                pid: Some(42),
                recovery_evidence: Some(homeboy_core::daemon::DaemonRecoveryEvidence::Recoverable),
                ownership_evidence: None,
                adoption_command: None,
                binary_hash: None,
                daemon_version: None,
                daemon_build_identity: None,
                runtime_paths: None,
                active_jobs: 0,
                termination_evidence: None,
                repair_plan: Vec::new(),
            }),
            active_jobs: Vec::new(),
            active_runner_jobs: Vec::new(),
            stale_runner_jobs: Vec::new(),
            active_job_count: 0,
            stale_runner_job_count: 0,
            active_job_state: crate::session::RunnerActiveJobState::NotQueried,
            active_job_source: None,
            active_job_error: None,
            active_job_recovery_evidence: None,
            session_path: "test".to_string(),
        }
    }

    /// The manifest preflight must follow the runner's own admission rule.
    ///
    /// Forcing diagnostic SSH made this read-only query demand the one
    /// transport a runner with a fresh connected daemon refuses, which failed
    /// the preflight and aborted the entire controller upgrade (#13686).
    /// Clearing the flag unconditionally is equally wrong: a disconnected SSH
    /// runner has no daemon session, so transport selection would fall through
    /// to `Unavailable`.
    #[test]
    fn manifest_preflight_prefers_the_daemon_and_keeps_ssh_when_disconnected() {
        let connected_fresh = status_report(true);
        assert!(
            connected_fresh.daemon_fresh_for_admission(),
            "fixture must model a fresh daemon"
        );
        assert!(
            !diagnostic_ssh_is_permitted_for(&connected_fresh),
            "a fresh connected daemon must be reached through normal exec"
        );

        assert!(
            diagnostic_ssh_is_permitted_for(&status_report(false)),
            "a disconnected runner must keep diagnostic SSH"
        );

        // The decision must actually reach the exec options. Asserting only the
        // predicate would pass even if the result were never wired in.
        let runner = ssh_runner();
        let command = vec!["homeboy".to_string(), "extension".to_string()];
        assert!(
            !runner_manifest_query_options(&runner, command.clone(), false).allow_diagnostic_ssh,
            "a refused diagnostic SSH decision must clear the exec flag"
        );
        assert!(
            runner_manifest_query_options(&runner, command, true).allow_diagnostic_ssh,
            "a permitted diagnostic SSH decision must keep the exec flag"
        );
    }

    #[test]
    fn controller_only_extension_absent_from_runner_inventory_is_not_queried() {
        let runner = ssh_runner();
        let mut call_count = 0;
        let mut exec = |runner_id: &str, options: RunnerExecOptions| {
            call_count += 1;
            assert_eq!(runner_id, "lab-a");
            assert_eq!(options.command, ["homeboy", "extension", "list"]);
            assert!(!options.read_only_artifact_access);
            Ok(successful_output(
                runner_id,
                options.command,
                r#"{"success":true,"data":{"entities":[]}}"#,
            ))
        };

        assert!(
            runner_manifest_preflight_with_executor(&runner, "homeboy", "2.1.0", &mut exec,)
                .is_none()
        );
        assert_eq!(call_count, 1, "one inventory query answers the preflight");
    }

    #[test]
    fn runner_only_incompatible_extension_fails_closed() {
        let runner = ssh_runner();
        let mut call_count = 0;
        let mut exec = |runner_id: &str, options: RunnerExecOptions| {
            let stdout = match call_count {
                0 => {
                    assert_eq!(options.command, ["homeboy", "extension", "list"]);
                    r#"{"success":true,"data":{"entities":[{"id":"runner-only","core_compatibility":{"requires_homeboy":">=3.0.0"}}]}}"#
                }
                _ => panic!("the inventory alone answers compatibility"),
            };
            call_count += 1;
            Ok(successful_output(runner_id, options.command, stdout))
        };

        let rejection =
            runner_manifest_preflight_with_executor(&runner, "homeboy", "2.1.0", &mut exec)
                .expect("runner-only incompatible extension blocks promotion");
        assert!(rejection.contains("incompatible with selected controller 2.1.0"));
        assert_eq!(call_count, 1);
    }

    #[test]
    fn invalid_runner_inventory_manifest_fails_closed() {
        let runner = ssh_runner();
        let mut exec = |runner_id: &str, options: RunnerExecOptions| {
            Ok(successful_output(
                runner_id,
                options.command,
                r#"{"success":true,"data":{"entities":[{"id":"broken","error":"manifest_invalid","diagnostic":"invalid requires.homeboy"}]}}"#,
            ))
        };

        let rejection =
            runner_manifest_preflight_with_executor(&runner, "homeboy", "2.1.0", &mut exec)
                .expect("invalid installed manifest blocks promotion");
        assert!(rejection.contains("runner extension `broken` manifest is invalid"));
        assert!(rejection.contains("invalid requires.homeboy"));
    }

    #[test]
    fn legacy_incompatible_manifest_is_deferred_to_the_selected_candidate() {
        let runner = ssh_runner();
        let mut call_count = 0;
        let mut exec = |runner_id: &str, options: RunnerExecOptions| {
            let stdout = match call_count {
                0 => {
                    assert_eq!(options.command, ["homeboy", "extension", "list"]);
                    r#"{"success":true,"data":{"entities":[{"id":"discord","error":"manifest_deserialize_incompatible","diagnostic":"The extension manifest does not match the supported schema."},{"id":"rust","core_compatibility":{"requires_homeboy":">=2.0.0"}}]}}"#
                }
                _ => {
                    panic!("legacy-incompatible extension must not be queried by the legacy binary")
                }
            };
            call_count += 1;
            Ok(successful_output(runner_id, options.command, stdout))
        };

        assert!(
            runner_manifest_preflight_with_executor(&runner, "homeboy", "2.1.0", &mut exec)
                .is_none(),
            "the selected candidate owns compatibility for an unreadable legacy schema"
        );
        assert_eq!(call_count, 1);
    }

    #[test]
    fn changed_runner_manifest_is_rejected_by_revalidation_without_source_sync() {
        let runner = ssh_runner();
        let inventories = [
            r#"{"success":true,"data":{"entities":[{"id":"rust","core_compatibility":{"requires_homeboy":">=2.0.0"}}]}}"#,
            r#"{"success":true,"data":{"entities":[{"id":"rust","core_compatibility":{"requires_homeboy":">=3.0.0"}}]}}"#,
        ];
        let mut call_count = 0;
        let mut exec = |runner_id: &str, options: RunnerExecOptions| {
            assert_eq!(runner_id, "lab-a");
            assert_eq!(options.command, ["homeboy", "extension", "list"]);
            let stdout = inventories[call_count];
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
        assert_eq!(call_count, 2);
    }
}

pub fn upgrade_configured_runners(
    expected_controller_identity: Option<&str>,
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
    // The installed controller identity, not this (possibly pre-upgrade)
    // process's own build, is what configured runners converge to — and what
    // their compatibility is judged against, or a runner that converged to the
    // installed controller reads as skewed from the old one (#15552).
    let upgrade = || {
        let run = || {
            let (updated, skipped) = upgrade_runners(
                &runners,
                extension_updates,
                runner::exec,
                runner::status,
                expected_controller_identity,
            );
            // A runner is converged only when its runner-service units
            // run the converged binary too (#15733).
            converge_service_binaries_for_entries(&runners, updated, skipped)
        };
        Ok(crate::controller_identity::with_converging_controller(
            expected_controller_identity,
            run,
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

pub(crate) fn upgrade_runners(
    runners: &[Runner],
    extension_updates: &[ExtensionUpgradeEntry],
    mut exec: impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
    status: impl Fn(&str) -> Result<RunnerStatusReport>,
    expected_controller_identity: Option<&str>,
) -> (Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>) {
    let target = runner_upgrade_target(expected_controller_identity);
    let mut updated = Vec::new();
    let mut skipped = Vec::new();

    for runner in runners {
        let entry = upgrade_runner_with_executor(
            runner,
            extension_updates,
            &target,
            &mut exec,
            &status,
            &mut converge_configured_runner_binary,
        );
        if entry.deferred {
            homeboy_core::log_status!(
                "upgrade",
                "  {} deferred: {}",
                entry.runner_id,
                entry.detail
            );
            updated.push(entry);
        } else if entry.success {
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

/// The controller release configured runners converge to: the controller
/// this upgrade installed when its identity is known, else this process's
/// build.
///
/// `homeboy upgrade` runs its runner phase in the pre-upgrade process, so
/// this process's own build is the version being replaced (#15552, #16845).
pub(crate) fn runner_upgrade_target(expected_controller_identity: Option<&str>) -> BuildIdentity {
    expected_controller_identity
        .and_then(homeboy_upgrade::upgrade::parse_build_identity_display)
        .unwrap_or_else(homeboy_product_identity::build_identity)
}

/// The runner-binary step's outcome and the binary configured after it.
pub(crate) struct RunnerBinaryStep {
    pub(crate) convergence: RunnerBinaryConvergence,
    pub(crate) homeboy_path: String,
}

/// Production runner-binary step: the same convergence `runner connect`
/// runs, aimed at `target`, then the binary the runner is configured with
/// afterwards (an install repoints it at the new release slot).
fn converge_configured_runner_binary(runner: &Runner, target: &BuildIdentity) -> RunnerBinaryStep {
    let convergence =
        crate::connection::binary_convergence::converge_runner_binary(&runner.id, target);
    let homeboy_path = runner::load(&runner.id)
        .map(|runner| configured_homeboy_path(&runner))
        .unwrap_or_else(|_| configured_homeboy_path(runner));
    RunnerBinaryStep {
        convergence,
        homeboy_path,
    }
}

fn configured_homeboy_path(runner: &Runner) -> String {
    runner
        .settings
        .homeboy_path
        .clone()
        .unwrap_or_else(|| "homeboy".to_string())
}

/// Upgrade one runner: converge its binary on `target`, then — only once the
/// binary is converged or already current — sync extensions and report the
/// daemon drift that remains. Service-unit binaries are reconciled afterwards
/// by [`converge_service_binaries_for_entries`].
pub fn upgrade_runner_with_executor(
    runner: &Runner,
    extension_updates: &[ExtensionUpgradeEntry],
    target: &BuildIdentity,
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
    status: &impl Fn(&str) -> Result<RunnerStatusReport>,
    converge: &mut impl FnMut(&Runner, &BuildIdentity) -> RunnerBinaryStep,
) -> RunnerUpgradeEntry {
    let RunnerBinaryStep {
        convergence,
        homeboy_path,
    } = converge(runner, target);
    let previous_version = convergence
        .from
        .as_deref()
        .and_then(parse_cli_version_output);
    let target_identity = convergence
        .to
        .clone()
        .unwrap_or_else(|| target.display.clone());
    let reason = convergence.detail.clone().unwrap_or_default();
    let not_converged = |exit_code: i32, detail: String, recovery_commands: Vec<String>| {
        let mut entry = runner_upgrade_failure_entry(
            &runner.id,
            homeboy_path.clone(),
            previous_version.clone(),
            exit_code,
            detail,
        );
        entry.recovery_commands = recovery_commands;
        entry
    };
    let upgraded = match convergence.status {
        RunnerBinaryConvergenceStatus::Converged => true,
        RunnerBinaryConvergenceStatus::AlreadyCurrent => false,
        RunnerBinaryConvergenceStatus::PendingActiveJobs => {
            let mut entry = not_converged(
                0,
                format!("runner busy: {reason}"),
                vec![format!(
                    "homeboy upgrade --upgrade-runner {}",
                    shell_arg(&runner.id)
                )],
            );
            entry.success = true;
            entry.deferred = true;
            return entry;
        }
        RunnerBinaryConvergenceStatus::Failed => {
            let mut entry = not_converged(
                1,
                format!("runner binary convergence to {target_identity} failed: {reason}"),
                runner_binary_refresh_recovery_commands(&runner.id, target),
            );
            entry.path_drift = Some(BINARY_NOT_CONVERGED.to_string());
            return entry;
        }
        RunnerBinaryConvergenceStatus::SkippedUnverifiable => {
            let mut entry = not_converged(
                1,
                format!("runner binary could not be verified against {target_identity}: {reason}"),
                runner_binary_refresh_recovery_commands(&runner.id, target),
            );
            entry.path_drift = Some(BINARY_NOT_CONVERGED.to_string());
            return entry;
        }
        RunnerBinaryConvergenceStatus::SkippedRemoteNewer
        | RunnerBinaryConvergenceStatus::SkippedDevBuild => {
            let mut entry = not_converged(
                1,
                format!("runner binary not converged on {target_identity}: {reason}"),
                Vec::new(),
            );
            entry.path_drift = Some(BINARY_NOT_CONVERGED.to_string());
            return entry;
        }
    };

    let (extensions_synced, extensions_skipped, extensions_failed) =
        sync_runner_extensions(runner, &homeboy_path, extension_updates, exec);
    let stale_daemon = status(&runner.id)
        .ok()
        .and_then(|report| report.stale_daemon)
        .map(runner_daemon_drift_entry);
    let success = stale_daemon.is_none() && extensions_failed.is_empty();
    let binary = if upgraded {
        format!(
            "runner binary converged from {} to {target_identity}",
            convergence.from.as_deref().unwrap_or("an unknown build")
        )
    } else {
        format!("runner binary already runs {target_identity}")
    };
    RunnerUpgradeEntry {
        runner_id: runner.id.clone(),
        homeboy_path,
        success,
        upgraded,
        deferred: false,
        previous_version,
        new_version: Some(target.version.trim_start_matches('v').to_string()),
        bare_homeboy_version: None,
        path_drift: stale_daemon
            .is_some()
            .then(|| "runner daemon still runs another homeboy build".to_string()),
        recovery_commands: (!success)
            .then(|| {
                vec![format!(
                    "homeboy upgrade --upgrade-runner {}",
                    shell_arg(&runner.id)
                )]
            })
            .unwrap_or_default(),
        extensions_synced,
        extensions_skipped,
        extensions_failed,
        stale_daemon,
        daemon_previous_version: None,
        daemon_new_version: None,
        service_binaries: Vec::new(),
        exit_code: i32::from(!success),
        detail: binary,
    }
}

const BINARY_NOT_CONVERGED: &str =
    "runner binary did not converge on the target controller release";

#[cfg(test)]
mod upgrade_runner_tests {
    use super::super::tests::{runner_status, ssh_runner};
    use super::*;
    use crate::connection::binary_convergence::{
        converge_runner_binary_with, ObservedRemoteBinary,
    };
    use crate::homeboy_refresh::{install_controller_release_with, ControllerReleaseInstall};

    const INSTALLED: &str = "homeboy 0.401.0+bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn target() -> BuildIdentity {
        runner_upgrade_target(Some(INSTALLED))
    }

    fn remote(version: &str) -> std::result::Result<ObservedRemoteBinary, String> {
        Ok(ObservedRemoteBinary {
            version: version.to_string(),
            build_identity: Some(format!("homeboy {version}+def5678")),
        })
    }

    fn no_jobs() -> std::result::Result<Vec<String>, String> {
        Ok(Vec::new())
    }

    fn no_exec(_: &str, _: RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)> {
        panic!("a runner with no extension updates and no converged binary executes nothing")
    }

    fn upgrade(
        converge: impl FnOnce(&BuildIdentity) -> RunnerBinaryConvergence,
    ) -> RunnerUpgradeEntry {
        let runner = ssh_runner("lab", Some("/homeboy"));
        let mut converge = Some(converge);
        upgrade_runner_with_executor(
            &runner,
            &[],
            &target(),
            &mut no_exec,
            &runner_status,
            &mut |_, target| RunnerBinaryStep {
                convergence: converge.take().expect("converged once")(target),
                homeboy_path: "/slot/homeboy".to_string(),
            },
        )
    }

    #[test]
    fn the_target_is_the_installed_controller_not_the_running_build() {
        assert_ne!(
            homeboy_product_identity::build_identity().display,
            INSTALLED
        );
        let mut refs = Vec::new();
        let entry = upgrade(|target| {
            assert_eq!(target.display, INSTALLED);
            converge_runner_binary_with(
                target,
                || remote("0.400.0"),
                no_jobs,
                || {
                    install_controller_release_with("lab", target, |options| {
                        refs.push(options.git_ref.clone());
                        Err(homeboy_core::Error::internal_unexpected("stop"))
                    })
                },
            )
        });
        assert_eq!(refs, [Some("v0.401.0".to_string())]);
        assert_eq!(
            entry.recovery_commands,
            ["homeboy runner refresh-homeboy lab --ref v0.401.0 --reconnect"]
        );
        assert_eq!(
            runner_upgrade_target(None),
            homeboy_product_identity::build_identity()
        );
    }

    #[test]
    fn a_busy_daemon_defers_and_names_its_jobs() {
        let entry = upgrade(|target| {
            converge_runner_binary_with(
                target,
                || remote("0.400.0"),
                || Ok(vec!["job-1".to_string(), "job-2".to_string()]),
                || panic!("a busy daemon must not be replaced"),
            )
        });
        assert!(entry.deferred && entry.success, "{entry:?}");
        assert!(entry.detail.contains("job-1, job-2"), "{}", entry.detail);
        assert_eq!(entry.exit_code, 0);
        assert_eq!(
            entry.recovery_commands,
            ["homeboy upgrade --upgrade-runner lab"]
        );
    }

    #[test]
    fn extensions_are_not_synced_onto_an_unconverged_binary() {
        let runner = ssh_runner("lab", Some("/homeboy"));
        let updates = [super::super::tests::extension_update("rust", "abcdef1")];
        let entry = upgrade_runner_with_executor(
            &runner,
            &updates,
            &target(),
            &mut no_exec,
            &runner_status,
            &mut |_, target| RunnerBinaryStep {
                convergence: converge_runner_binary_with(
                    target,
                    || remote("0.400.0"),
                    || Ok(vec!["job-1".to_string()]),
                    || panic!("a busy daemon must not be replaced"),
                ),
                homeboy_path: "/homeboy".to_string(),
            },
        );
        assert!(entry.deferred);
        assert!(entry.extensions_synced.is_empty());
    }

    #[test]
    fn a_converged_binary_reports_the_installed_release() {
        let entry = upgrade(|target| {
            converge_runner_binary_with(
                target,
                || remote("0.400.0"),
                no_jobs,
                || ControllerReleaseInstall::Installed {
                    identity: Some(INSTALLED.to_string()),
                },
            )
        });
        assert!(
            entry.success && entry.upgraded && !entry.deferred,
            "{entry:?}"
        );
        assert_eq!(entry.previous_version.as_deref(), Some("0.400.0"));
        assert_eq!(entry.new_version.as_deref(), Some("0.401.0"));
        assert_eq!(entry.homeboy_path, "/slot/homeboy");
        assert!(entry.recovery_commands.is_empty());
        assert!(entry.detail.contains(INSTALLED), "{}", entry.detail);
    }

    #[test]
    fn an_already_current_binary_skips_the_install() {
        let entry = upgrade(|target| {
            converge_runner_binary_with(
                target,
                || remote("0.401.0"),
                no_jobs,
                || panic!("a current binary must not be reinstalled"),
            )
        });
        assert!(entry.success && !entry.upgraded, "{entry:?}");
        assert_eq!(entry.new_version.as_deref(), Some("0.401.0"));
        assert!(entry.detail.contains("already runs"), "{}", entry.detail);
    }

    #[test]
    fn upgrade_runners_refresh_failure_keeps_attributed_cause() {
        let entry = upgrade(|target| {
            converge_runner_binary_with(
                target,
                || remote("0.400.0"),
                no_jobs,
                || {
                    install_controller_release_with("lab", target, |_| {
                        let mut error = homeboy_core::Error::internal_unexpected("IO error");
                        error.details = serde_json::json!({
                            "context": "publish slot",
                            "error": "permission denied"
                        });
                        Err(error)
                    })
                },
            )
        });
        assert!(!entry.success && !entry.deferred);
        assert_eq!(entry.exit_code, 1);
        assert!(
            entry
                .detail
                .contains("IO error: publish slot: permission denied"),
            "{}",
            entry.detail
        );
    }

    #[test]
    fn a_newer_runner_is_never_downgraded_and_not_reported_converged() {
        let entry = upgrade(|target| {
            converge_runner_binary_with(
                target,
                || remote("0.402.0"),
                no_jobs,
                || panic!("never downgraded"),
            )
        });
        assert!(!entry.success, "{entry:?}");
        assert!(entry.recovery_commands.is_empty());
    }
}
