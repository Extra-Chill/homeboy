//! Shared-work supervision for one detached loop-controller coordinator.

use std::time::Duration;

use homeboy_core::process::{
    process_identity_state_with_start_identity, ProcessIdentityState, ProcessStartIdentity,
};
use homeboy_core::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::work_job::{
    register_work_job_handler, work_job_submission, WorkJobHandle, WorkJobHandler,
    WorkJobInvocation,
};
use crate::agent_task_loop_controller::{self, AgentTaskLoopControllerState};

pub const AGENT_TASK_LOOP_JOB_TYPE: &str = "agent-task-loop";
pub const AGENT_TASK_LOOP_JOB_VERSION: u32 = 1;
const AGENT_TASK_LOOP_JOB_SCHEMA: &str = "homeboy/agent-task-loop-job/v1";
const SUPERVISION_POLL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskLoopJobRequest {
    pub schema: String,
    pub loop_id: String,
    pub child_pid: u32,
    pub child_start_identity: ProcessStartIdentity,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskLoopJobPhase {
    #[default]
    Queued,
    Supervising,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskLoopJob {
    pub schema: String,
    pub idempotency_key: String,
    pub request: AgentTaskLoopJobRequest,
    #[serde(default)]
    pub phase: AgentTaskLoopJobPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_state: Option<AgentTaskLoopControllerState>,
}

impl AgentTaskLoopJob {
    fn new(request: AgentTaskLoopJobRequest) -> Result<Self> {
        if request.schema != AGENT_TASK_LOOP_JOB_SCHEMA || request.loop_id.trim().is_empty() {
            return Err(invalid_loop_job(
                "loop jobs require a recognized schema and durable loop id",
            ));
        }
        if request.child_pid == 0 {
            return Err(invalid_loop_job(
                "loop jobs require the detached coordinator's process id",
            ));
        }
        Ok(Self {
            schema: AGENT_TASK_LOOP_JOB_SCHEMA.to_string(),
            idempotency_key: format!("agent-task-loop:{}", request.loop_id),
            request,
            phase: AgentTaskLoopJobPhase::Queued,
            controller_state: None,
        })
    }

    fn parse(value: Value) -> Result<Self> {
        let job: Self = serde_json::from_value(value)
            .map_err(|error| invalid_loop_job(&format!("invalid durable loop job: {error}")))?;
        let expected = Self::new(job.request.clone())?;
        if job.schema != AGENT_TASK_LOOP_JOB_SCHEMA
            || job.idempotency_key != expected.idempotency_key
        {
            return Err(invalid_loop_job(
                "loop job identity does not match its immutable request",
            ));
        }
        if job.phase == AgentTaskLoopJobPhase::Completed
            && !job
                .controller_state
                .is_some_and(controller_state_is_terminal)
        {
            return Err(invalid_loop_job(
                "completed loop jobs require a terminal controller state",
            ));
        }
        Ok(job)
    }

    fn to_checkpoint(&self) -> Result<Value> {
        serde_json::to_value(self).map_err(|error| {
            homeboy_core::Error::internal_json(
                error.to_string(),
                Some("serialize loop work checkpoint".to_string()),
            )
        })
    }

    fn public_projection(&self) -> Value {
        json!({
            "schema": self.schema,
            "idempotency_key": self.idempotency_key,
            "phase": self.phase,
            "loop_id": self.request.loop_id,
            "controller_state": self.controller_state,
        })
    }

    fn result(&self) -> Value {
        json!({
            "phase": self.phase,
            "loop_id": self.request.loop_id,
            "controller_state": self.controller_state,
        })
    }

    fn refresh_controller_state(&mut self) -> bool {
        let Ok(record) = agent_task_loop_controller::controller_status(&self.request.loop_id)
        else {
            return false;
        };
        let changed = self.controller_state != Some(record.state);
        self.controller_state = Some(record.state);
        changed
    }
}

struct LoopWorkHandler;

impl WorkJobHandler for LoopWorkHandler {
    fn work_type(&self) -> &'static str {
        AGENT_TASK_LOOP_JOB_TYPE
    }

    fn version(&self) -> u32 {
        AGENT_TASK_LOOP_JOB_VERSION
    }

    fn public_request(&self, request: &Value) -> Result<Value> {
        Ok(AgentTaskLoopJob::parse(request.clone())?.public_projection())
    }

    fn public_progress(&self, progress: &Value) -> Result<Value> {
        Ok(json!({
            "phase": progress.get("phase").cloned().unwrap_or(Value::Null),
            "loop_id": progress.get("loop_id").cloned().unwrap_or(Value::Null),
            "controller_state": progress.get("controller_state").cloned().unwrap_or(Value::Null),
        }))
    }

    fn public_result(&self, result: &Value) -> Result<Value> {
        self.public_progress(result)
    }

    fn validate_secret_references(&self, request: &Value) -> Result<()> {
        AgentTaskLoopJob::parse(request.clone()).map(|_| ())
    }

    fn prepare(&self, request: Value) -> Result<Value> {
        let mut job = AgentTaskLoopJob::parse(request)?;
        if job.phase != AgentTaskLoopJobPhase::Queued || job.controller_state.is_some() {
            return Err(invalid_loop_job(
                "new loop jobs must start queued without controller state",
            ));
        }
        job.phase = AgentTaskLoopJobPhase::Supervising;
        job.refresh_controller_state();
        job.to_checkpoint()
    }

    fn advance(
        &self,
        checkpoint: Value,
        handle: WorkJobHandle,
        invocation: WorkJobInvocation,
    ) -> Result<Value> {
        let mut job = AgentTaskLoopJob::parse(checkpoint)?;
        if invocation == WorkJobInvocation::Resume && job.phase == AgentTaskLoopJobPhase::Completed
        {
            return Ok(job.result());
        }
        self.supervise(&mut job, handle)
    }

    fn cancel(&self, checkpoint: &Value) -> Result<()> {
        let job = AgentTaskLoopJob::parse(checkpoint.clone())?;
        if job.phase == AgentTaskLoopJobPhase::Completed || !coordinator_is_live(&job.request) {
            return Ok(());
        }
        homeboy_core::process::terminate_process_tree(job.request.child_pid).map(|_| ())
    }
}

impl LoopWorkHandler {
    fn supervise(&self, job: &mut AgentTaskLoopJob, handle: WorkJobHandle) -> Result<Value> {
        job.phase = AgentTaskLoopJobPhase::Supervising;
        job.refresh_controller_state();
        handle.checkpoint(job.to_checkpoint()?)?;
        handle.progress(job.result())?;

        loop {
            if job.refresh_controller_state() {
                handle.checkpoint(job.to_checkpoint()?)?;
                handle.progress(job.result())?;
            }
            if job
                .controller_state
                .is_some_and(controller_state_is_terminal)
            {
                job.phase = AgentTaskLoopJobPhase::Completed;
                return Ok(job.result());
            }
            if handle.is_cancelled() {
                let _ = self.cancel(&job.to_checkpoint()?);
                return terminalize_interrupted(job, AgentTaskLoopControllerState::Abandoned);
            }
            if !coordinator_is_live(&job.request) {
                return terminalize_interrupted(job, AgentTaskLoopControllerState::Failed);
            }
            std::thread::sleep(SUPERVISION_POLL);
        }
    }
}

fn terminalize_interrupted(
    job: &mut AgentTaskLoopJob,
    interrupted_state: AgentTaskLoopControllerState,
) -> Result<Value> {
    job.refresh_controller_state();
    if !job
        .controller_state
        .is_some_and(controller_state_is_terminal)
    {
        let mut record = agent_task_loop_controller::load_controller(&job.request.loop_id)?;
        record.state = interrupted_state;
        agent_task_loop_controller::write_controller(&record)?;
        job.controller_state = Some(interrupted_state);
    }
    job.phase = AgentTaskLoopJobPhase::Completed;
    Ok(job.result())
}

fn coordinator_is_live(request: &AgentTaskLoopJobRequest) -> bool {
    matches!(
        process_identity_state_with_start_identity(
            request.child_pid,
            None,
            Some(&request.child_start_identity),
        ),
        ProcessIdentityState::Live
    )
}

fn controller_state_is_terminal(state: AgentTaskLoopControllerState) -> bool {
    matches!(
        state,
        AgentTaskLoopControllerState::HumanReady
            | AgentTaskLoopControllerState::Completed
            | AgentTaskLoopControllerState::Abandoned
            | AgentTaskLoopControllerState::Escalated
            | AgentTaskLoopControllerState::Failed
    )
}

pub fn register_loop_work_job_handler() {
    static REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    REGISTERED.get_or_init(|| {
        register_work_job_handler(std::sync::Arc::new(LoopWorkHandler))
            .expect("register loop work job handler");
    });
}

pub fn loop_work_job_submission(
    loop_id: &str,
    child_pid: u32,
    child_start_identity: &ProcessStartIdentity,
) -> Result<Value> {
    let job = AgentTaskLoopJob::new(AgentTaskLoopJobRequest {
        schema: AGENT_TASK_LOOP_JOB_SCHEMA.to_string(),
        loop_id: loop_id.to_string(),
        child_pid,
        child_start_identity: child_start_identity.clone(),
    })?;
    work_job_submission(
        &LoopWorkHandler,
        job.idempotency_key.clone(),
        job.to_checkpoint()?,
    )
}

fn invalid_loop_job(message: &str) -> homeboy_core::Error {
    homeboy_core::Error::validation_invalid_argument("loop_job", message, None, None)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use homeboy_core::daemon::controller_job_driver::ControllerJobDriver;
    use homeboy_core::test_support::{with_isolated_home, ControllerJobHarness};

    use super::*;
    use crate::agent_task_service::work_job::{
        WorkJobDriver, WORK_JOB_CHECKPOINT_SCHEMA, WORK_JOB_TYPE, WORK_JOB_VERSION,
    };

    const IDENTITY: ProcessStartIdentity = ProcessStartIdentity::Linux {
        starttime_ticks: 4242,
    };

    fn submission(loop_id: &str, pid: u32) -> Value {
        register_loop_work_job_handler();
        loop_work_job_submission(loop_id, pid, &IDENTITY).expect("build loop work submission")
    }

    #[test]
    fn new_loop_submissions_use_the_shared_work_driver() {
        let submission = submission("loop-shared", 4242);

        assert_eq!(submission["type"], WORK_JOB_TYPE);
        assert_eq!(submission["version"], WORK_JOB_VERSION);
        assert_eq!(submission["idempotency_key"], "agent-task-loop:loop-shared");
        assert_eq!(submission["request"]["work_type"], AGENT_TASK_LOOP_JOB_TYPE);
        assert_eq!(
            submission["request"]["request"]["request"]["loop_id"],
            "loop-shared"
        );
        WorkJobDriver
            .validate_secret_references(&submission["request"])
            .expect("reference-only loop request validates");
        let public = WorkJobDriver
            .public_request(&submission["request"])
            .expect("safe public projection");
        assert_eq!(public["loop_id"], "loop-shared");
        assert!(public.get("child_pid").is_none());
        assert!(!public.to_string().contains("starttime_ticks"));
    }

    #[test]
    fn dead_coordinator_terminalizes_through_the_shared_harness() {
        with_isolated_home(|_| {
            agent_task_loop_controller::create_controller("loop-dead", "repair", "v1")
                .expect("create controller");
            let request = submission("loop-dead", u32::MAX)["request"].clone();
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = ControllerJobHarness::new(Arc::clone(&driver), request.clone())
                .expect("construct work harness");
            let prepared = driver.prepare(request).expect("prepare loop work");

            let result = driver
                .execute(prepared, harness.handle())
                .expect("observe dead coordinator");

            assert_eq!(result["result"]["phase"], "completed");
            assert_eq!(result["result"]["controller_state"], "failed");
            assert_eq!(
                agent_task_loop_controller::load_controller("loop-dead")
                    .expect("read terminal controller")
                    .state,
                AgentTaskLoopControllerState::Failed
            );
            let checkpoint = harness
                .checkpoint()
                .expect("read checkpoint")
                .expect("loop supervision checkpoint");
            assert_eq!(checkpoint["schema"], WORK_JOB_CHECKPOINT_SCHEMA);
            assert_eq!(checkpoint["work_type"], AGENT_TASK_LOOP_JOB_TYPE);
        });
    }

    #[test]
    fn completed_loop_checkpoint_replays_without_reexecution() {
        with_isolated_home(|_| {
            let mut record =
                agent_task_loop_controller::create_controller("loop-complete", "repair", "v1")
                    .expect("create controller");
            record.state = AgentTaskLoopControllerState::Completed;
            agent_task_loop_controller::write_controller(&record).expect("complete controller");
            let request = submission("loop-complete", u32::MAX)["request"].clone();
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = ControllerJobHarness::new(Arc::clone(&driver), request.clone())
                .expect("construct work harness");
            let mut checkpoint = driver.prepare(request).expect("prepare loop work");
            checkpoint["checkpoint"]["phase"] = json!("completed");

            let first = driver
                .resume(checkpoint.clone(), harness.handle())
                .expect("first replay");
            let second = driver
                .resume(checkpoint, harness.handle())
                .expect("second replay");

            assert_eq!(first, second);
            assert_eq!(first["result"]["controller_state"], "completed");
        });
    }

    #[test]
    fn cancellation_through_the_shared_harness_stops_the_coordinator() {
        with_isolated_home(|_| {
            register_loop_work_job_handler();
            agent_task_loop_controller::create_controller("loop-cancel", "repair", "v1")
                .expect("create controller");
            let child = std::process::Command::new("sh")
                .args(["-c", "sleep 30"])
                .spawn()
                .expect("spawn coordinator fixture");
            let identity = homeboy_core::process::process_start_identity(child.id())
                .expect("inspect fixture")
                .expect("fixture identity");
            let request = loop_work_job_submission("loop-cancel", child.id(), &identity)
                .expect("build submission")["request"]
                .clone();
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = ControllerJobHarness::new(Arc::clone(&driver), request.clone())
                .expect("construct work harness");
            let prepared = driver.prepare(request).expect("prepare loop work");
            harness
                .request_cancellation("test cancellation")
                .expect("request cancellation");

            driver
                .cancel(&prepared)
                .expect("cancel coordinator process tree");
            let result = driver
                .execute(prepared, harness.handle())
                .expect("terminalize cancelled loop work");

            assert_eq!(result["result"]["controller_state"], "abandoned");
            assert!(matches!(
                homeboy_core::process::process_identity_state(child.id(), None),
                ProcessIdentityState::Dead
            ));
        });
    }
}
