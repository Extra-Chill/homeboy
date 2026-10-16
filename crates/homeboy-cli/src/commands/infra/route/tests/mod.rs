#![cfg(test)]

mod dispatch;
mod handoff;
mod terminal_review_wait;

#[test]
fn terminal_cook_uses_shared_work_job_with_the_existing_active_fence() {
    use homeboy::core::daemon::controller_job_driver::ControllerJobDriver;

    register_execution_orchestration();
    let request = serde_json::json!({
        "schema": "homeboy/terminal-cook-continuation-request/v1",
        "cook_id": "cook",
        "run_id": "run",
        "generation": 2,
    });
    let submission = terminal_cook_work_submission(&request).expect("typed work submission");
    assert_eq!(submission["type"], "work");
    assert_eq!(submission["version"], 1);
    assert_eq!(submission["request"]["work_type"], TERMINAL_COOK_WORK_TYPE);
    assert_eq!(
        submission["request"]["work_version"],
        TERMINAL_COOK_JOB_VERSION
    );
    assert_eq!(submission["request"]["request"], request);
    assert_eq!(
        submission["active_idempotency_key"],
        terminal_cook_job_idempotency_keys("cook", "run", 2).1
    );

    let driver = crate::agents::agent_task_service::WorkJobDriver;
    driver
        .validate_secret_references(&submission["request"])
        .expect("request carries references only");
    assert_eq!(
        driver.linked_durable_run_id(&submission["request"]),
        Some("run".to_string())
    );
    assert_eq!(
        driver.public_request(&submission["request"]).unwrap(),
        serde_json::json!({ "cook_id": "cook", "run_id": "run" })
    );
    let checkpoint = driver
        .prepare(submission["request"].clone())
        .expect("shared driver prepares terminal Cook handler");
    assert_eq!(checkpoint["work_type"], TERMINAL_COOK_WORK_TYPE);
    assert_eq!(checkpoint["checkpoint"], request);
}

#[test]
fn terminal_cook_job_retry_epoch_changes_submission_key_but_keeps_attempt_fence() {
    let first = terminal_cook_job_idempotency_keys("cook", "run", 0);
    let retry = terminal_cook_job_idempotency_keys("cook", "run", 1);
    assert_ne!(first.0, retry.0, "a retry must create a fresh durable job");
    assert_eq!(first.1, retry.1, "one attempt's generations share a fence");
    assert_ne!(
        first.1,
        terminal_cook_job_idempotency_keys("cook", "follow-up", 0).1,
        "each attempt of a Cook has its own fence"
    );
}

/// Attempt 2's continuation is admitted while attempt 1's job is still
/// active (#16053); a second generation of attempt 1 stays fenced.
#[test]
fn next_attempt_continuation_schedules_while_previous_attempt_job_is_active() {
    use homeboy::core::daemon::controller_job_driver::ControllerJobDriver;

    homeboy::core::test_support::with_isolated_home(|_| {
        register_execution_orchestration();
        crate::agents::agent_task_service::register_work_job_driver();
        let continuation = |run_id: &str, generation: u64| {
            terminal_cook_work_submission(&serde_json::json!({
                "schema": "homeboy/terminal-cook-continuation-request/v1",
                "cook_id": "cook",
                "run_id": run_id,
                "generation": generation,
            }))
            .expect("typed work submission")
        };
        let driver: Arc<dyn ControllerJobDriver> =
            Arc::new(crate::agents::agent_task_service::WorkJobDriver);
        let harness = homeboy::core::test_support::ControllerJobHarness::new(
            driver,
            continuation("attempt-0", 0)["request"].clone(),
        )
        .expect("job store harness");

        let attempt_one = harness
            .admit_keyed(continuation("attempt-1", 0))
            .expect("attempt 1 continuation admitted");
        assert!(!attempt_one.status.is_terminal());
        let attempt_two = harness
            .admit_keyed(continuation("attempt-2", 0))
            .expect("attempt 2 is not fenced by attempt 1's active job");
        assert_ne!(attempt_two.id, attempt_one.id);
        assert!(!harness
            .store()
            .get(attempt_one.id)
            .expect("attempt 1 job")
            .status
            .is_terminal());

        let same_attempt = harness
            .admit_keyed(continuation("attempt-1", 1))
            .expect_err("a new generation of an active attempt is fenced");
        assert!(same_attempt
            .message
            .contains("controller idempotency key is already bound"));
    });
}

/// A claimed terminal continuation that failed retryably is handed to its
/// next generation instead of failing the work job, and a terminally failed
/// continuation is announced before the job fails (#15831).
#[test]
fn terminal_continuation_failure_step_hands_off_retries_and_announces_failure() {
    use crate::agents::agent_task_service::{CookContinuationState, WorkJobStep};

    let failure = || {
        Error::validation_invalid_argument(
            "cook_continuation.artifact_projection",
            "artifact projection is not ready",
            None,
            None,
        )
        .with_retryable(true)
    };

    let mut announced = false;
    let step =
        terminal_continuation_failure_step(CookContinuationState::Pending, failure(), || {
            announced = true
        })
        .expect("a pending retry completes this job");
    assert!(matches!(
        step,
        WorkJobStep::Complete(ref result)
            if result["retry_scheduled"] == true
                && result["error_code"] == "validation.invalid_argument"
    ));
    assert!(!announced, "a scheduled retry is not a terminal stop");

    let mut announced = false;
    let Err(error) =
        terminal_continuation_failure_step(CookContinuationState::Failed, failure(), || {
            announced = true
        })
    else {
        panic!("a failed continuation fails the job");
    };
    assert!(error.message.contains("artifact projection is not ready"));
    assert!(
        announced,
        "a terminal continuation failure must be announced"
    );

    let mut announced = false;
    assert!(
        terminal_continuation_failure_step(CookContinuationState::Claimed, failure(), || {
            announced = true
        })
        .is_err(),
        "an unexpected state still fails the job"
    );
    assert!(!announced);
}

use super::*;
use homeboy::core::test_support::bounded_output;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

pub(super) fn git_init(path: &Path) {
    let mut command = Command::new("git");
    command.args(["init", "-b", "main"]).current_dir(path);
    let output = bounded_output(command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(super) struct EnvGuard {
    previous: Vec<(&'static str, Option<String>)>,
    _guard: MutexGuard<'static, ()>,
}

pub(super) struct CwdGuard {
    previous: std::path::PathBuf,
    _guard: MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn set(name: &'static str, value: &str) -> Self {
        Self::set_many(&[(name, Some(value))])
    }

    fn remove(name: &'static str) -> Self {
        Self::set_many(&[(name, None)])
    }

    pub(super) fn set_many(changes: &[(&'static str, Option<&str>)]) -> Self {
        let guard = env_lock().lock().unwrap_or_else(|err| err.into_inner());
        let mut previous = Vec::with_capacity(changes.len());
        for (name, value) in changes {
            previous.push((*name, std::env::var(name).ok()));
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        Self {
            previous,
            _guard: guard,
        }
    }
}

pub(super) fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, previous) in self.previous.iter().rev() {
            match previous {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

impl CwdGuard {
    fn set(path: &std::path::Path) -> Self {
        let guard = env_lock().lock().unwrap_or_else(|err| err.into_inner());
        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(path).expect("set current dir");
        Self {
            previous,
            _guard: guard,
        }
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.previous).expect("restore current dir");
    }
}

pub(super) fn write_rig_source_metadata(home: &Path, rig_id: &str, linked: bool) {
    let sources_dir = home.join(".config").join("homeboy").join("rig-sources");
    fs::create_dir_all(&sources_dir).expect("create rig sources dir");
    let metadata = serde_json::json!({
        "source": "/tmp/rig-package",
        "source_root": "/tmp/rig-package",
        "package_path": "/tmp/rig-package",
        "rig_path": format!("/tmp/rig-package/rigs/{rig_id}/rig.json"),
        "discovery_path": "/tmp/rig-package",
        "linked": linked,
        "materialized": false
    });
    fs::write(
        sources_dir.join(format!("{rig_id}.json")),
        serde_json::to_string_pretty(&metadata).expect("serialize rig source metadata"),
    )
    .expect("write rig source metadata");
}

pub(super) fn write_command_only_rig(home: &Path, rig_id: &str) {
    let rigs_dir = home.join(".config").join("homeboy").join("rigs");
    fs::create_dir_all(&rigs_dir).expect("create rigs dir");
    let spec = serde_json::json!({
        "id": rig_id,
        "description": "command-only rig",
        "pipeline": {
            "up": [
                {
                    "kind": "command",
                    "command": "./scripts/run-matrix.sh",
                    "cwd": "tools",
                    "env": { "MATRIX": "portable" },
                    "label": "run matrix"
                }
            ]
        }
    });
    fs::write(
        rigs_dir.join(format!("{rig_id}.json")),
        serde_json::to_string_pretty(&spec).expect("serialize rig"),
    )
    .expect("write rig");
}
