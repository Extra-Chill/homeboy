use clap::{Args, Subcommand};
use fs4::fs_std::FileExt;
use homeboy::core::deferred_workload;
use serde::Serialize;
use std::collections::BTreeSet;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use super::CmdResult;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const CAPABILITY_MISMATCH_EXIT_CODE: i32 = 75;
const CAPABILITY_MISMATCH_ERROR: &str = "deferred workload runner capability mismatch";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunnerCapabilityInventory {
    pub runner_id: String,
    pub runtime_ids: BTreeSet<String>,
    pub capabilities: BTreeSet<String>,
}

#[derive(Args)]
pub struct DeferredWorkloadArgs {
    #[command(subcommand)]
    command: DeferredWorkloadCommand,
}

#[derive(Subcommand)]
enum DeferredWorkloadCommand {
    /// Run the singleton controller-owned deferred-workload worker
    Worker {
        #[arg(long, value_name = "TOKEN")]
        startup_token: String,
    },
    /// Inspect deferred workloads and the controller worker
    Status,
}

#[derive(Serialize)]
struct DeferredWorkloadStatusOutput {
    schema: &'static str,
    worker: Option<deferred_workload::DeferredWorkloadWorkerStatus>,
    records: Vec<serde_json::Value>,
    diagnostics: serde_json::Value,
}

pub fn run(args: DeferredWorkloadArgs) -> CmdResult<serde_json::Value> {
    match args.command {
        DeferredWorkloadCommand::Worker { startup_token } => {
            run_worker(&startup_token)?;
            Ok((
                serde_json::json!({ "schema": "homeboy/deferred-workload-worker-result/v1", "status": "stopped" }),
                0,
            ))
        }
        DeferredWorkloadCommand::Status => {
            let output = DeferredWorkloadStatusOutput {
                schema: "homeboy/deferred-workload-status/v1",
                worker: deferred_workload::worker_status()?,
                records: deferred_workload::records()?
                    .iter()
                    .map(redacted_record)
                    .collect(),
                diagnostics: serde_json::json!({
                    "worker_command": "homeboy deferred-workload worker",
                    "status_command": "homeboy deferred-workload status",
                    "ci_alternative": "Run the portable command in CI or configure a ready Homeboy runner."
                }),
            };
            Ok((
                serde_json::to_value(output).expect("deferred workload status serializes"),
                0,
            ))
        }
    }
}

pub fn ensure_worker() -> homeboy::core::Result<()> {
    ensure_worker_with(deferred_workload::worker_is_live, || {
        let executable = std::env::current_exe().map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some("resolve deferred worker executable".to_string()),
            )
        })?;
        let mut command = Command::new(executable);
        let startup_token = uuid::Uuid::new_v4().to_string();
        command.args([
            "deferred-workload",
            "worker",
            "--startup-token",
            &startup_token,
        ]);
        // A detached worker must not keep an invoking client's capture pipes
        // open after the foreground command exits.
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        command.spawn().map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some("spawn deferred workload worker".to_string()),
            )
        })?;
        Ok(())
    })
}

pub fn restart_worker_if_pending() -> homeboy::core::Result<()> {
    restart_worker_if_pending_with(deferred_workload::worker_is_live, || ensure_worker())
}

fn ensure_worker_with(
    is_live: impl Fn(&deferred_workload::DeferredWorkloadWorkerStatus) -> bool,
    spawn: impl FnOnce() -> homeboy::core::Result<()>,
) -> homeboy::core::Result<()> {
    if deferred_workload::worker_status()?
        .as_ref()
        .is_some_and(is_live)
    {
        return Ok(());
    }
    spawn()
}

fn restart_worker_if_pending_with(
    is_live: impl Fn(&deferred_workload::DeferredWorkloadWorkerStatus) -> bool,
    spawn: impl FnOnce() -> homeboy::core::Result<()>,
) -> homeboy::core::Result<()> {
    if deferred_workload::records()?.iter().any(|record| {
        matches!(
            record.state,
            deferred_workload::DeferredWorkloadState::Deferred
                | deferred_workload::DeferredWorkloadState::Claimed
        )
    }) {
        ensure_worker_with(is_live, spawn)?;
    }
    Ok(())
}

fn run_worker(startup_token: &str) -> homeboy::core::Result<()> {
    let lock = deferred_workload::worker_lock()?;
    if lock.try_lock_exclusive().is_err() {
        return Ok(());
    }
    std::env::set_var("HOMEBOY_DEFERRED_WORKLOAD_OWNER", startup_token);
    let owner = startup_token.to_string();
    deferred_workload::append_worker_log(format!("worker started owner={owner}"))?;
    run_worker_with(
        &owner,
        || {
            let readiness = crate::runner::runners::lab_runner_readiness()?;
            let Some(runner_id) = (readiness.state
                == crate::runner::runners::LabRunnerReadinessState::ConnectedReady)
                .then_some(readiness.selected_runner_id)
                .flatten()
            else {
                return Ok(None);
            };
            crate::runner::runners::runner_capability_inventory(&runner_id).map(|inventory| {
                Some(RunnerCapabilityInventory {
                    runner_id,
                    runtime_ids: inventory.runtime_ids,
                    capabilities: inventory.capabilities,
                })
            })
        },
        dispatch_record,
        deferred_workload_now_ms,
        thread::sleep,
    )
}

pub(crate) fn run_worker_with(
    owner: &str,
    mut readiness: impl FnMut() -> homeboy::core::Result<Option<RunnerCapabilityInventory>>,
    mut dispatch: impl FnMut(
        &deferred_workload::DeferredWorkload,
        &str,
        &str,
    ) -> homeboy::core::Result<bool>,
    now: impl Fn() -> u64,
    mut sleep: impl FnMut(Duration),
) -> homeboy::core::Result<()> {
    loop {
        let pending = deferred_workload::records()?.into_iter().any(|record| {
            matches!(
                record.state,
                deferred_workload::DeferredWorkloadState::Deferred
                    | deferred_workload::DeferredWorkloadState::Claimed
            )
        });
        if !pending {
            deferred_workload::write_worker_status(owner, "idle", "no deferred workloads")?;
            return Ok(());
        }
        let inventory = match readiness() {
            Ok(Some(inventory)) => inventory,
            Ok(None) => {
                deferred_workload::write_worker_status(
                    owner,
                    "waiting_for_runner",
                    "no ready runner",
                )?;
                sleep(POLL_INTERVAL);
                continue;
            }
            Err(error) => {
                deferred_workload::write_worker_status(owner, "waiting_for_runner", error.message)?;
                sleep(POLL_INTERVAL);
                continue;
            }
        };
        let Some(record) = deferred_workload::claim_next_matching_at(
            &inventory.runner_id,
            owner,
            now(),
            |candidate| runner_satisfies_requirements(candidate, &inventory),
        )?
        else {
            deferred_workload::write_worker_status(
                owner,
                "waiting_for_runner",
                "no claimable workload for selected runner",
            )?;
            sleep(POLL_INTERVAL);
            continue;
        };
        let runner_id = &inventory.runner_id;
        deferred_workload::write_worker_status(
            owner,
            "dispatching",
            format!("{} via {runner_id}", record.id),
        )?;
        deferred_workload::append_worker_log(format!("claimed {} via {runner_id}", record.id))?;
        let success = match dispatch(&record, &runner_id, owner) {
            Ok(success) => success,
            Err(error) if error.message == CAPABILITY_MISMATCH_ERROR => {
                deferred_workload::defer_claim(&record.id, owner)?;
                deferred_workload::append_worker_log(format!(
                    "deferred {} after runner capability preflight mismatch",
                    record.id
                ))?;
                continue;
            }
            Err(error) => return Err(error),
        };
        deferred_workload::terminalize(&record.id, success)?;
        deferred_workload::append_worker_log(format!(
            "terminalized {} success={success}",
            record.id
        ))?;
    }
}

fn runner_satisfies_requirements(
    record: &deferred_workload::DeferredWorkload,
    inventory: &RunnerCapabilityInventory,
) -> bool {
    record
        .test_requirements
        .is_satisfied_by(&inventory.runtime_ids, &inventory.capabilities)
}

fn deferred_workload_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn dispatch_record(
    record: &deferred_workload::DeferredWorkload,
    runner_id: &str,
    owner: &str,
) -> homeboy::core::Result<bool> {
    let executable = std::env::current_exe().map_err(|error| {
        homeboy::core::Error::internal_io(
            error.to_string(),
            Some("resolve deferred workload executable".to_string()),
        )
    })?;
    let args = child_args(record, runner_id);
    let mut child = Command::new(executable)
        .args(&args[1..])
        .env("HOMEBOY_DEFERRED_WORKLOAD_REPLAY", "1")
        .spawn()
        .map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some("dispatch deferred workload".to_string()),
            )
        })?;
    loop {
        if let Some(status) = child.try_wait().map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some("wait for deferred workload".to_string()),
            )
        })? {
            if status.code() == Some(CAPABILITY_MISMATCH_EXIT_CODE) {
                return Err(homeboy::core::Error::validation_invalid_argument(
                    "runner_capabilities",
                    CAPABILITY_MISMATCH_ERROR,
                    Some(runner_id.to_string()),
                    None,
                ));
            }
            return Ok(status.success());
        }
        if !deferred_workload::heartbeat(&record.id, owner)? {
            return Ok(false);
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn child_args(record: &deferred_workload::DeferredWorkload, runner_id: &str) -> Vec<String> {
    let mut args = record.args.clone();
    let mut overrides = vec!["--runner".to_string(), runner_id.to_string()];
    for (name, value) in &record.job_overrides.env {
        overrides.extend(["--runner-env".to_string(), format!("{name}={value}")]);
    }
    for name in &record.job_overrides.secret_env_names {
        overrides.extend(["--runner-secret-env".to_string(), name.clone()]);
    }
    args.splice(1..1, overrides);
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    fn inventory(id: &str) -> RunnerCapabilityInventory {
        RunnerCapabilityInventory {
            runner_id: id.to_string(),
            runtime_ids: ["homeboy".to_string()].into(),
            capabilities: ["test-runner".to_string()].into(),
        }
    }

    fn input() -> deferred_workload::DeferredWorkloadInput {
        deferred_workload::DeferredWorkloadInput {
            command_label: "review test".to_string(),
            args: vec![
                "homeboy".to_string(),
                "review".to_string(),
                "test".to_string(),
            ],
            placement: "auto".to_string(),
            resource_requirement: "eligible_lab_runner".to_string(),
            portability: "portable_lab_route".to_string(),
            reason: "no ready runner".to_string(),
            ci_alternative: "run in CI".to_string(),
            resolved_contract: serde_json::json!({}),
            resolved_resources: serde_json::json!({}),
            test_requirements: deferred_workload::DeferredWorkloadRequirements {
                required_runtimes: ["homeboy".to_string()].into(),
                required_capabilities: ["test-runner".to_string()].into(),
            },
            job_overrides: Default::default(),
        }
    }

    #[test]
    fn warm_deferred_workload_waits_then_dispatches_once_when_runner_appears() {
        crate::test_support::with_isolated_home(|_| {
            let deferred = deferred_workload::defer(input()).expect("defer warm workload");
            let ready = Rc::new(Cell::new(false));
            let ready_after_wait = ready.clone();
            let dispatched = Rc::new(RefCell::new(Vec::new()));
            let dispatched_by_worker = dispatched.clone();

            run_worker_with(
                "worker-a",
                || Ok(ready.get().then(|| inventory("compatible-runner"))),
                move |record, runner_id, _| {
                    dispatched_by_worker
                        .borrow_mut()
                        .push((record.id.clone(), runner_id.to_string()));
                    Ok(true)
                },
                || 10,
                |_| ready_after_wait.set(true),
            )
            .expect("worker completes deferred workload");

            assert_eq!(
                dispatched.borrow().as_slice(),
                &[(deferred.id, "compatible-runner".to_string())]
            );
            assert_eq!(
                deferred_workload::records().expect("records")[0].state,
                deferred_workload::DeferredWorkloadState::Dispatched
            );
        });
    }

    #[test]
    fn warm_defer_dispatches_public_db_service_values_and_secret_reference_without_plaintext() {
        crate::test_support::with_isolated_home(|_| {
            let mut input = input();
            input.job_overrides = homeboy::core::lab_offload::LabJobOverrides {
                env: [
                    ("DB_SERVICE_HOST".to_string(), "db.fixture".to_string()),
                    ("DB_SERVICE_PORT".to_string(), "3306".to_string()),
                ]
                .into(),
                secret_env_names: vec!["DB_SERVICE_PASSWORD".to_string()],
                workspace_root: None,
            };
            let deferred = deferred_workload::defer(input).expect("defer warm workload");
            let ready = Rc::new(Cell::new(false));
            let ready_after_wait = ready.clone();

            run_worker_with(
                "worker-a",
                || Ok(ready.get().then(|| inventory("compatible-runner"))),
                move |record, runner_id, _| {
                    assert_eq!(runner_id, "compatible-runner");
                    assert_eq!(record.job_overrides.env["DB_SERVICE_HOST"], "db.fixture");
                    assert_eq!(record.job_overrides.env["DB_SERVICE_PORT"], "3306");
                    assert!(!record.job_overrides.env.contains_key("DB_SERVICE_PASSWORD"));
                    assert_eq!(
                        record.job_overrides.secret_env_names,
                        ["DB_SERVICE_PASSWORD"]
                    );
                    let durable_json = serde_json::to_string(record).expect("deferred JSON");
                    assert!(!durable_json.contains("fixture-password"));
                    Ok(true)
                },
                || 10,
                |_| ready_after_wait.set(true),
            )
            .expect("worker dispatches after compatible runner appears");

            assert_eq!(
                deferred_workload::records().expect("records")[0].state,
                deferred_workload::DeferredWorkloadState::Dispatched
            );
            assert_eq!(
                deferred.id,
                deferred_workload::records().expect("records")[0].id
            );
        });
    }

    #[test]
    fn incompatible_runner_waits_without_dispatching_until_compatible_runner_arrives() {
        crate::test_support::with_isolated_home(|_| {
            deferred_workload::defer(input()).expect("defer workload");
            let compatible = Rc::new(Cell::new(false));
            let compatible_after_wait = compatible.clone();
            let readiness_calls = Rc::new(Cell::new(0));
            let calls = readiness_calls.clone();
            let dispatches = Rc::new(Cell::new(0));
            let dispatch_count = dispatches.clone();

            run_worker_with(
                "worker-a",
                move || {
                    calls.set(calls.get() + 1);
                    if compatible.get() {
                        Ok(Some(inventory("compatible-runner")))
                    } else {
                        Ok(Some(RunnerCapabilityInventory {
                            runner_id: "incompatible-runner".to_string(),
                            runtime_ids: ["other-runtime".to_string()].into(),
                            capabilities: BTreeSet::new(),
                        }))
                    }
                },
                move |_, _, _| {
                    dispatch_count.set(dispatch_count.get() + 1);
                    Ok(true)
                },
                || 10,
                |_| compatible_after_wait.set(true),
            )
            .expect("worker dispatches after compatibility changes");

            assert!(
                readiness_calls.get() >= 2,
                "incompatible readiness must wait"
            );
            assert_eq!(dispatches.get(), 1);
        });
    }

    #[test]
    fn incompatible_work_waits_without_busy_spinning() {
        crate::test_support::with_isolated_home(|_| {
            let deferred = deferred_workload::defer(input()).expect("defer workload");
            let sleeps = Cell::new(0);
            let dispatches = Cell::new(0);

            run_worker_with(
                "worker-a",
                || {
                    Ok(Some(RunnerCapabilityInventory {
                        runner_id: "incompatible-runner".to_string(),
                        runtime_ids: ["other-runtime".to_string()].into(),
                        capabilities: BTreeSet::new(),
                    }))
                },
                |_, _, _| {
                    dispatches.set(dispatches.get() + 1);
                    Ok(true)
                },
                || 10,
                |_| {
                    sleeps.set(sleeps.get() + 1);
                    deferred_workload::terminalize(&deferred.id, false)
                        .expect("remove incompatible fixture after wait");
                },
            )
            .expect("worker waits and exits when no work remains");

            assert_eq!(sleeps.get(), 1);
            assert_eq!(dispatches.get(), 0);
        });
    }

    #[test]
    fn child_argv_reconstructs_persisted_secret_references() {
        let mut record = deferred_workload::DeferredWorkload {
            id: "deferred-fixture".to_string(),
            fingerprint: "fixture".to_string(),
            command_label: "review test".to_string(),
            args: vec![
                "homeboy".to_string(),
                "review".to_string(),
                "test".to_string(),
            ],
            placement: "auto".to_string(),
            resource_requirement: "eligible_lab_runner".to_string(),
            portability: "portable_lab_route".to_string(),
            reason: "fixture".to_string(),
            ci_alternative: "CI".to_string(),
            resolved_contract: serde_json::json!({}),
            resolved_resources: serde_json::json!({}),
            test_requirements: deferred_workload::DeferredWorkloadRequirements {
                required_runtimes: ["homeboy".to_string()].into(),
                required_capabilities: BTreeSet::new(),
            },
            job_overrides: Default::default(),
            state: deferred_workload::DeferredWorkloadState::Deferred,
            created_at_ms: 0,
            updated_at_ms: 0,
            runner_id: None,
            claim_owner: None,
            claim_expires_at_ms: None,
        };
        record.job_overrides.secret_env_names = vec!["DB_SERVICE_PASSWORD".to_string()];
        let args = child_args(&record, "compatible-runner");

        assert!(args
            .windows(2)
            .any(|pair| { pair == ["--runner-secret-env", "DB_SERVICE_PASSWORD"] }));
    }

    #[test]
    fn restarted_worker_reclaims_an_expired_claim() {
        crate::test_support::with_isolated_home(|_| {
            let deferred = deferred_workload::defer(input()).expect("defer workload");
            let first = deferred_workload::claim_next_at("first-runner", "dead-worker", 1)
                .expect("claim workload")
                .expect("deferred workload");
            let dispatched = Rc::new(Cell::new(0));
            let dispatch_count = dispatched.clone();

            run_worker_with(
                "restarted-worker",
                || Ok(Some(inventory("recovery-runner"))),
                move |record, runner_id, owner| {
                    assert_eq!(record.id, deferred.id);
                    assert_eq!(runner_id, "recovery-runner");
                    assert_eq!(owner, "restarted-worker");
                    dispatch_count.set(dispatch_count.get() + 1);
                    Ok(true)
                },
                || first.claim_expires_at_ms.expect("claim expiry"),
                |_| panic!("recovered worker should not wait"),
            )
            .expect("restarted worker reclaims expired work");

            assert_eq!(dispatched.get(), 1);
            assert_eq!(
                deferred_workload::records().expect("records")[0].state,
                deferred_workload::DeferredWorkloadState::Dispatched
            );
        });
    }

    #[test]
    fn pending_workload_restarts_a_dead_worker_but_not_a_live_one() {
        crate::test_support::with_isolated_home(|_| {
            deferred_workload::defer(input()).expect("defer workload");
            let spawned = Cell::new(0);
            restart_worker_if_pending_with(
                |_| false,
                || {
                    spawned.set(spawned.get() + 1);
                    Ok(())
                },
            )
            .expect("restart dead worker");
            assert_eq!(spawned.get(), 1);
        });
    }
}

pub(crate) fn redacted_record(record: &deferred_workload::DeferredWorkload) -> serde_json::Value {
    let mut value = serde_json::to_value(record).expect("deferred workload serializes");
    let secret_names = record
        .job_overrides
        .secret_env_names
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    if let Some(env) = value
        .pointer_mut("/job_overrides/env")
        .and_then(serde_json::Value::as_object_mut)
    {
        for name in secret_names {
            if env.contains_key(name) {
                env.insert(
                    name.to_string(),
                    serde_json::Value::String("[REDACTED]".to_string()),
                );
            }
        }
    }
    if let Some(args) = value
        .get_mut("args")
        .and_then(serde_json::Value::as_array_mut)
    {
        redact_settings_args(args);
    }
    value
}

fn redact_settings_args(args: &mut [serde_json::Value]) {
    let mut redact_next = false;
    for arg in args {
        let Some(value) = arg.as_str() else { continue };
        if redact_next {
            *arg = serde_json::Value::String("[REDACTED]".to_string());
            redact_next = false;
        } else if matches!(
            value,
            "--setting" | "--setting-json" | "--settings-json-file" | "--settings-profile"
        ) {
            redact_next = true;
        } else if value.starts_with("--setting=")
            || value.starts_with("--setting-json=")
            || value.starts_with("--settings-json-file=")
            || value.starts_with("--settings-profile=")
        {
            *arg = serde_json::Value::String(
                value.split_once('=').expect("checked").0.to_string() + "=[REDACTED]",
            );
        }
    }
}
