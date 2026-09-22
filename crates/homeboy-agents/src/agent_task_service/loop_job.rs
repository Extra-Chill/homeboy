//! Shared-work supervision for one detached loop-controller coordinator.

use std::time::Duration;

use homeboy_core::process::ProcessStartIdentity;
use homeboy_core::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::work_job::{
    register_work_job_handler, work_job_submission, WorkJobHandler, WorkJobInvocation,
    WorkJobPhase, WorkJobStep,
};
use crate::agent_task_controller_service::{ControllerDispatchHook, ControllerDispatchOverrides};
use crate::agent_task_dispatch_service;
use crate::agent_task_loop_controller::{self, AgentTaskLoopControllerState};
use crate::agent_task_provider::{AgentTaskProviderCatalog, ExtensionProviderAgentTaskExecutor};
use crate::agent_task_scheduler::SharedAgentTaskExecutor;

pub const AGENT_TASK_LOOP_JOB_TYPE: &str = "agent-task-loop";
pub const AGENT_TASK_LOOP_JOB_VERSION: u32 = 2;
const AGENT_TASK_LOOP_JOB_SCHEMA: &str = "homeboy/agent-task-loop-job/v2";
const LEGACY_AGENT_TASK_LOOP_JOB_SCHEMA: &str = "homeboy/agent-task-loop-job/v1";
const SUPERVISION_POLL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskLoopJobKind {
    #[default]
    LegacyChild,
    DaemonExecution,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskLoopJobRequest {
    pub schema: String,
    pub loop_id: String,
    #[serde(default)]
    pub kind: AgentTaskLoopJobKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_start_identity: Option<ProcessStartIdentity>,
    #[serde(default)]
    pub generation: String,
    #[serde(default)]
    pub dispatch_defaults: Value,
    /// The caller's admitted provider catalog. The daemon must not rediscover
    /// providers from its own environment while executing this job.
    #[serde(default)]
    pub provider_catalog: AgentTaskProviderCatalog,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskLoopJob {
    pub schema: String,
    pub idempotency_key: String,
    pub request: AgentTaskLoopJobRequest,
    #[serde(default)]
    pub phase: WorkJobPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_state: Option<AgentTaskLoopControllerState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_result: Option<Value>,
}

impl AgentTaskLoopJob {
    fn new(request: AgentTaskLoopJobRequest) -> Result<Self> {
        if !matches!(
            request.schema.as_str(),
            AGENT_TASK_LOOP_JOB_SCHEMA | LEGACY_AGENT_TASK_LOOP_JOB_SCHEMA
        ) || request.loop_id.trim().is_empty()
        {
            return Err(invalid_loop_job(
                "loop jobs require a recognized schema and durable loop id",
            ));
        }
        if request.child_pid == Some(0) {
            return Err(invalid_loop_job(
                "loop jobs require the detached coordinator's process id",
            ));
        }
        if request.child_pid.is_some() != request.child_start_identity.is_some() {
            return Err(invalid_loop_job(
                "loop jobs require both coordinator pid and start identity",
            ));
        }
        if request.kind == AgentTaskLoopJobKind::DaemonExecution
            && request.generation.trim().is_empty()
        {
            return Err(invalid_loop_job(
                "daemon loop executions require a non-empty generation",
            ));
        }
        if request.kind == AgentTaskLoopJobKind::DaemonExecution && request.child_pid.is_some() {
            return Err(invalid_loop_job(
                "daemon loop executions cannot carry a legacy child identity",
            ));
        }
        if request.kind == AgentTaskLoopJobKind::LegacyChild && request.child_pid.is_none() {
            return Err(invalid_loop_job(
                "legacy loop jobs require a coordinator process identity",
            ));
        }
        Ok(Self {
            schema: AGENT_TASK_LOOP_JOB_SCHEMA.to_string(),
            idempotency_key: if request.generation.is_empty() {
                format!("agent-task-loop:{}", request.loop_id)
            } else {
                format!("agent-task-loop:{}:{}", request.loop_id, request.generation)
            },
            request,
            phase: WorkJobPhase::Queued,
            controller_state: None,
            resume_result: None,
        })
    }

    fn parse(value: Value) -> Result<Self> {
        let mut job: Self = serde_json::from_value(value)
            .map_err(|error| invalid_loop_job(&format!("invalid durable loop job: {error}")))?;
        // Old queued jobs remain readable, but every resumed checkpoint is
        // rewritten with the explicit v2 execution-kind contract.
        if job.schema == LEGACY_AGENT_TASK_LOOP_JOB_SCHEMA {
            job.schema = AGENT_TASK_LOOP_JOB_SCHEMA.to_string();
        }
        if job.request.schema == LEGACY_AGENT_TASK_LOOP_JOB_SCHEMA {
            job.request.schema = AGENT_TASK_LOOP_JOB_SCHEMA.to_string();
        }
        let expected = Self::new(job.request.clone())?;
        if job.schema != AGENT_TASK_LOOP_JOB_SCHEMA
            || job.idempotency_key != expected.idempotency_key
        {
            return Err(invalid_loop_job(
                "loop job identity does not match its immutable request",
            ));
        }
        if job.phase == WorkJobPhase::Completed
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
            "kind": self.request.kind,
            "controller_state": self.controller_state,
            "generation": self.request.generation,
        })
    }

    fn result(&self) -> Value {
        json!({
            "phase": self.phase,
            "loop_id": self.request.loop_id,
            "controller_state": self.controller_state,
            "resume": self.resume_result,
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
        if job.phase != WorkJobPhase::Queued || job.controller_state.is_some() {
            return Err(invalid_loop_job(
                "new loop jobs must start queued without controller state",
            ));
        }
        job.phase = WorkJobPhase::Supervising;
        job.refresh_controller_state();
        job.to_checkpoint()
    }

    fn initial_progress(&self, checkpoint: &Value) -> Result<Value> {
        Ok(AgentTaskLoopJob::parse(checkpoint.clone())?.result())
    }

    fn terminal_result(&self, checkpoint: &Value) -> Result<Option<Value>> {
        let job = AgentTaskLoopJob::parse(checkpoint.clone())?;
        job.phase
            .eq(&WorkJobPhase::Completed)
            .then(|| Ok(job.result()))
            .transpose()
    }

    fn advance(&self, checkpoint: Value, invocation: WorkJobInvocation) -> Result<WorkJobStep> {
        let mut job = AgentTaskLoopJob::parse(checkpoint)?;
        if job.phase == WorkJobPhase::Completed {
            return Ok(WorkJobStep::Complete(job.result()));
        }
        self.observe(&mut job, invocation)
    }

    fn cancelled(&self, checkpoint: Value) -> Result<Value> {
        let mut job = AgentTaskLoopJob::parse(checkpoint)?;
        terminalize_interrupted(&mut job, AgentTaskLoopControllerState::Abandoned)
    }

    fn cancel(&self, checkpoint: &Value) -> Result<()> {
        let job = AgentTaskLoopJob::parse(checkpoint.clone())?;
        let Some(child_pid) = job.request.child_pid else {
            agent_task_loop_controller::cancel_owned_provider_runs(
                &job.request.loop_id,
                "controller work job cancelled",
            )?;
            return Ok(());
        };
        let Some(child_start_identity) = job.request.child_start_identity.as_ref() else {
            return Ok(());
        };
        if job.phase == WorkJobPhase::Completed
            || !super::work_job::supervised_child_is_live(child_pid, child_start_identity)
        {
            return Ok(());
        }
        if controller_state_is_terminal(
            agent_task_loop_controller::controller_status(&job.request.loop_id)?.state,
        ) {
            return Ok(());
        }
        homeboy_core::process::terminate_process_tree(child_pid).map(|_| ())
    }
}

impl LoopWorkHandler {
    fn observe(
        &self,
        job: &mut AgentTaskLoopJob,
        invocation: WorkJobInvocation,
    ) -> Result<WorkJobStep> {
        job.phase = WorkJobPhase::Supervising;
        job.refresh_controller_state();
        if job
            .controller_state
            .is_some_and(controller_state_is_terminal)
        {
            job.phase = WorkJobPhase::Completed;
            return Ok(WorkJobStep::Complete(job.result()));
        }
        if job.request.child_pid.is_none() {
            let mut catalog = job.request.provider_catalog.clone();
            apply_admitted_environment(
                &mut catalog,
                &job.request.dispatch_defaults["env_materialization"],
            )?;
            let executor: SharedAgentTaskExecutor = std::sync::Arc::new(
                ExtensionProviderAgentTaskExecutor::from_catalog(catalog.clone()),
            );
            let dispatch = LoopDispatchHook {
                executor: executor.clone(),
                catalog,
                defaults: ControllerDispatchOverrides {
                    backend: job.request.dispatch_defaults["backend"]
                        .as_str()
                        .map(str::to_string),
                    selector: job.request.dispatch_defaults["selector"]
                        .as_str()
                        .map(str::to_string),
                    model: job.request.dispatch_defaults["model"]
                        .as_str()
                        .map(str::to_string),
                    provider_config: job.request.dispatch_defaults["provider_config_ref"]
                        .as_str()
                        .map(|reference| format!("@{reference}")),
                },
            };
            let report = match crate::agent_task_controller_service::resume_with_options(
                &job.request.loop_id,
                executor,
                &dispatch,
                crate::agent_task_controller_service::ControllerResumeOptions {
                    max_actions: 1,
                    stop_on_terminal: true,
                },
            ) {
                Ok(report) => report,
                Err(error) => {
                    job.refresh_controller_state();
                    if job
                        .controller_state
                        .is_some_and(controller_state_is_terminal)
                    {
                        job.phase = WorkJobPhase::Completed;
                        return Ok(WorkJobStep::Complete(job.result()));
                    }
                    return Err(error);
                }
            };
            job.resume_result =
                Some(serde_json::to_value(report.value).map_err(|error| {
                    homeboy_core::Error::internal_json(error.to_string(), None)
                })?);
            #[cfg(test)]
            if TEST_INTERRUPT_AFTER_LOOP_DISPATCH.with(|flag| flag.replace(false)) {
                return Err(homeboy_core::Error::internal_unexpected(
                    "test interruption after loop child admission",
                ));
            }
            job.refresh_controller_state();
            if job
                .controller_state
                .is_some_and(controller_state_is_terminal)
            {
                job.phase = WorkJobPhase::Completed;
                return Ok(WorkJobStep::Complete(job.result()));
            }
            return Ok(WorkJobStep::Continue {
                checkpoint: job.to_checkpoint()?,
                progress: job.result(),
                wait: SUPERVISION_POLL,
            });
        }
        let _ = invocation;
        if !super::work_job::supervised_child_is_live(
            job.request.child_pid.expect("legacy child job"),
            job.request
                .child_start_identity
                .as_ref()
                .expect("legacy child identity"),
        ) {
            return Ok(WorkJobStep::Complete(terminalize_interrupted(
                job,
                AgentTaskLoopControllerState::Failed,
            )?));
        }
        Ok(WorkJobStep::Continue {
            checkpoint: job.to_checkpoint()?,
            progress: job.result(),
            wait: SUPERVISION_POLL,
        })
    }
}

#[cfg(test)]
thread_local! {
    static TEST_INTERRUPT_AFTER_LOOP_DISPATCH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn with_test_loop_checkpoint_interrupt<T>(body: impl FnOnce() -> T) -> T {
    TEST_INTERRUPT_AFTER_LOOP_DISPATCH.with(|flag| flag.set(true));
    let result = body();
    TEST_INTERRUPT_AFTER_LOOP_DISPATCH.with(|flag| flag.set(false));
    result
}

/// Apply the caller's public environment projection to provider declarations.
/// The command runner then uses its normal launch-context path and resolves
/// secret names separately via `SecretEnvPlan`. Unlisted names are rejected
/// instead of becoming authority through the daemon environment.
fn apply_admitted_environment(catalog: &mut AgentTaskProviderCatalog, value: &Value) -> Result<()> {
    if value.is_null() {
        return Ok(());
    }
    let mut plan: homeboy_core::env_materialization_plan::EnvMaterializationPlan =
        serde_json::from_value(value.clone()).map_err(|error| {
            invalid_loop_job(&format!(
                "invalid loop environment materialization plan: {error}"
            ))
        })?;
    plan.normalize();
    for (name, value) in &plan.public_env {
        let mut declared = false;
        for provider in &mut catalog.providers {
            if crate::agent_task_provider::provider_secret_env_plan(
                provider,
                &serde_json::from_value(json!({
                    "task_id": "loop-environment-materialization",
                    "executor": { "backend": provider.backend },
                    "instructions": ""
                }))
                .map_err(|error| {
                    invalid_loop_job(&format!("invalid provider secret plan input: {error}"))
                })?,
            )
            .secret_env_names()
            .iter()
            .any(|secret_name| secret_name == name)
            {
                return Err(invalid_loop_job(&format!(
                    "loop environment materialization cannot override secret provider variable `{name}`"
                )));
            }
            for env in &mut provider.invocation.env {
                if env.name == *name {
                    if env.redacted == Some(true) || env.source.as_deref() == Some("secret_env") {
                        return Err(invalid_loop_job(&format!(
                            "loop environment materialization cannot override secret provider variable `{name}`"
                        )));
                    }
                    env.source = Some("value".to_string());
                    env.value = Some(value.clone());
                    env.redacted = Some(false);
                    declared = true;
                }
            }
        }
        if !declared {
            return Err(invalid_loop_job(&format!(
                "loop environment materialization names undeclared provider variable `{name}`"
            )));
        }
    }
    Ok(())
}

#[derive(Clone)]
struct LoopDispatchHook {
    executor: SharedAgentTaskExecutor,
    catalog: AgentTaskProviderCatalog,
    defaults: ControllerDispatchOverrides,
}

impl ControllerDispatchHook for LoopDispatchHook {
    fn dispatch(&self, request: &Value) -> Result<(Value, i32)> {
        let command = crate::agent_task_controller_service::controller_request_dispatch_command(
            request,
            &self.defaults,
        )?;
        agent_task_dispatch_service::run_dispatch_command_with_provider_catalog(
            command,
            self.executor.clone(),
            &self.catalog,
        )
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
    job.phase = WorkJobPhase::Completed;
    Ok(job.result())
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
        kind: AgentTaskLoopJobKind::LegacyChild,
        child_pid: Some(child_pid),
        child_start_identity: Some(child_start_identity.clone()),
        generation: String::new(),
        dispatch_defaults: Value::Null,
        provider_catalog: AgentTaskProviderCatalog::default(),
    })?;
    work_job_submission(
        &LoopWorkHandler,
        job.idempotency_key.clone(),
        job.to_checkpoint()?,
    )
}

/// Admit one daemon-owned loop execution without launching a public CLI child.
pub fn loop_work_job_execution_submission(
    loop_id: &str,
    generation: &str,
    dispatch_defaults: Value,
    provider_catalog: AgentTaskProviderCatalog,
) -> Result<Value> {
    if generation.trim().is_empty() {
        return Err(invalid_loop_job(
            "new loop executions require a non-empty generation",
        ));
    }
    let dispatch_defaults = admitted_dispatch_defaults(dispatch_defaults)?;
    let job = AgentTaskLoopJob::new(AgentTaskLoopJobRequest {
        schema: AGENT_TASK_LOOP_JOB_SCHEMA.to_string(),
        loop_id: loop_id.to_string(),
        kind: AgentTaskLoopJobKind::DaemonExecution,
        child_pid: None,
        child_start_identity: None,
        generation: generation.to_string(),
        dispatch_defaults,
        provider_catalog,
    })?;
    work_job_submission(
        &LoopWorkHandler,
        job.idempotency_key.clone(),
        job.to_checkpoint()?,
    )
}

fn admitted_dispatch_defaults(value: Value) -> Result<Value> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_loop_job("dispatch defaults must be an object"))?;
    let mut admitted = serde_json::Map::new();
    for key in [
        "backend",
        "selector",
        "model",
        "provider_config_ref",
        "provider_account",
        "provider_catalog",
        "env_materialization",
        "secret_env_plan",
    ] {
        if let Some(value) = object.get(key) {
            admitted.insert(key.to_string(), value.clone());
        }
    }
    Ok(Value::Object(admitted))
}

fn invalid_loop_job(message: &str) -> homeboy_core::Error {
    homeboy_core::Error::validation_invalid_argument("loop_job", message, None, None)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use homeboy_core::daemon::controller_job_driver::ControllerJobDriver;
    use homeboy_core::process::ProcessIdentityState;
    use homeboy_core::test_support::{with_isolated_home, ControllerJobHarness, EnvVarGuard};

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
    fn execution_submissions_admit_catalog_and_never_persist_provider_config() {
        let submission = loop_work_job_execution_submission(
            "loop-execution",
            "generation-1",
            json!({
                "backend": "fixture",
                "provider_config": "credential-value-must-not-cross-boundary"
            }),
            AgentTaskProviderCatalog::default(),
        )
        .expect("build execution submission");

        let encoded = submission.to_string();
        assert!(!encoded.contains("credential-value-must-not-cross-boundary"));
        assert!(submission["request"]["request"]["request"]["provider_catalog"].is_object());
    }

    #[test]
    fn execution_dispatch_keeps_the_admitted_provider_config_reference() {
        let submission = loop_work_job_execution_submission(
            "loop-config-reference-dispatch",
            "generation-1",
            json!({
                "backend": "fixture",
                "provider_config_ref": "provider-configs/caller-a"
            }),
            AgentTaskProviderCatalog::default(),
        )
        .expect("build execution submission");
        let defaults = ControllerDispatchOverrides {
            backend: Some("fixture".to_string()),
            provider_config: submission["request"]["request"]["request"]["dispatch_defaults"]
                ["provider_config_ref"]
                .as_str()
                .map(|reference| format!("@{reference}")),
            ..Default::default()
        };
        let command = crate::agent_task_controller_service::controller_request_dispatch_command(
            &json!({"mode": "dispatch", "prompt": "fixture"}),
            &defaults,
        )
        .expect("dispatch command");
        assert_eq!(
            command.core.provider_config.as_deref(),
            Some("@provider-configs/caller-a")
        );
    }

    #[test]
    fn loop_work_job_provider_receives_caller_environment_and_private_config_reference() {
        with_isolated_home(|_| {
            let loop_id = "loop-caller-environment-a";
            let observed = tempfile::NamedTempFile::new().expect("observed provider output");
            let config = tempfile::NamedTempFile::new().expect("private provider config");
            std::fs::write(config.path(), r#"{"account":"caller-a"}"#)
                .expect("write caller config");
            let _daemon = EnvVarGuard::set("HOMEBOY_FIXTURE_AUTHORITY", "daemon-b");
            let mut record = agent_task_loop_controller::create_controller(
                loop_id,
                "repair",
                "v1",
            )
            .expect("create controller");
            record.record_action(
                crate::agent_task_loop_controller::AgentTaskLoopPolicyAction::SpawnTask {
                    dedupe_key: "caller-environment-provider".to_string(),
                    entity_id: None,
                    request: json!({
                        "mode": "dispatch",
                        "dispatch": { "backend": "caller-fixture", "prompt": "observe" }
                    }),
                },
                "caller environment handoff fixture",
            );
            agent_task_loop_controller::write_controller(&record).expect("write controller");
            let provider: crate::agent_task_provider::AgentTaskExecutorProvider =
                serde_json::from_value(json!({
                    "id": "caller-fixture",
                    "backend": "caller-fixture",
                    "command_argv": [
                        "sh", "-c",
                        format!(
                            "printf '%s\\n%s' $HOMEBOY_FIXTURE_AUTHORITY $HOMEBOY_AGENT_TASK_EXECUTOR_CONFIG_JSON > {}; printf '%s' '{{\"schema\":\"homeboy/agent-task-outcome/v1\",\"task_id\":\"fixture\",\"status\":\"succeeded\"}}'",
                            observed.path().display()
                        )
                    ],
                    "invocation": { "env": [{
                        "name": "HOMEBOY_FIXTURE_AUTHORITY",
                        "source": "env",
                        "required": true
                    }] },
                    "capabilities": ["structured_outcome"]
                }))
                .expect("provider fixture");
            let catalog = AgentTaskProviderCatalog {
                providers: vec![provider],
                ..Default::default()
            };
            let mut job = AgentTaskLoopJob::new(AgentTaskLoopJobRequest {
                schema: AGENT_TASK_LOOP_JOB_SCHEMA.to_string(),
                loop_id: loop_id.to_string(),
                kind: AgentTaskLoopJobKind::DaemonExecution,
                child_pid: None,
                child_start_identity: None,
                generation: "generation-caller-a".to_string(),
                dispatch_defaults: json!({
                    "backend": "caller-fixture",
                    "provider_config_ref": config.path().display().to_string(),
                    "env_materialization": {
                        "schema": "homeboy/env-materialization-plan/v1",
                        "public_env": { "HOMEBOY_FIXTURE_AUTHORITY": "caller-a" }
                    }
                }),
                provider_catalog: catalog,
            })?;

            let _ = LoopWorkHandler.observe(&mut job, WorkJobInvocation::Execute)?;
            let observed = std::fs::read_to_string(observed.path()).expect("provider ran");
            assert!(observed.starts_with("caller-a\n"), "observed={observed}");
            assert!(observed.contains(r#""account":"caller-a""#));
            assert!(!observed.contains("daemon-b"));
            Ok::<(), homeboy_core::Error>(())
        })
        .expect("caller environment handoff converges");
    }

    #[test]
    fn public_environment_materialization_cannot_override_secret_authority() {
        let mut catalog = AgentTaskProviderCatalog {
            providers: vec![serde_json::from_value(json!({
                "id": "secret-fixture",
                "backend": "secret-fixture",
                "invocation": { "env": [{
                    "name": "FAKE_TOKEN",
                    "source": "secret_env",
                    "redacted": true
                }] }
            }))
            .expect("secret provider")],
            ..Default::default()
        };
        let error = apply_admitted_environment(
            &mut catalog,
            &json!({
                "schema": "homeboy/env-materialization-plan/v1",
                "public_env": { "FAKE_TOKEN": "not-a-secret" }
            }),
        )
        .expect_err("public env must not replace secret authority");
        assert!(
            error.to_string().contains("secret provider variable"),
            "unexpected validation error: {error:?}"
        );
    }

    #[test]
    fn mixed_legacy_process_identity_is_rejected_without_panicking() {
        let request = serde_json::to_value(AgentTaskLoopJobRequest {
            schema: AGENT_TASK_LOOP_JOB_SCHEMA.to_string(),
            loop_id: "loop-invalid-identity".to_string(),
            kind: AgentTaskLoopJobKind::LegacyChild,
            child_pid: Some(4242),
            child_start_identity: None,
            generation: String::new(),
            dispatch_defaults: Value::Null,
            provider_catalog: AgentTaskProviderCatalog::default(),
        })
        .expect("encode request");
        let error = AgentTaskLoopJob::parse(json!({
            "schema": AGENT_TASK_LOOP_JOB_SCHEMA,
            "idempotency_key": "agent-task-loop:loop-invalid-identity",
            "request": request,
            "phase": "queued"
        }))
        .expect_err("mixed identity must be rejected");
        assert!(format!("{error:?}").contains("both coordinator pid and start identity"));
    }

    #[test]
    fn new_execution_requires_a_generation_and_preserves_provider_config_reference() {
        let error = loop_work_job_execution_submission(
            "loop-missing-generation",
            "",
            json!({
                "backend": "fixture",
                "provider_config_ref": "provider-configs/primary"
            }),
            AgentTaskProviderCatalog::default(),
        )
        .expect_err("execution generation is required");
        assert!(format!("{error:?}").contains("generation"));

        let submission = loop_work_job_execution_submission(
            "loop-config-reference",
            "generation-1",
            json!({
                "backend": "fixture",
                "provider_config_ref": "provider-configs/primary"
            }),
            AgentTaskProviderCatalog::default(),
        )
        .expect("reference-only config is admitted");
        assert_eq!(
            submission["request"]["request"]["request"]["dispatch_defaults"]["provider_config_ref"],
            "provider-configs/primary"
        );
    }

    #[test]
    fn cancelling_new_execution_cancels_owned_run_instead_of_returning_immediately() {
        with_isolated_home(|_| {
            let loop_id = "loop-cancel-execution";
            let mut record = agent_task_loop_controller::create_controller(loop_id, "repair", "v1")
                .expect("create controller");
            record.state = AgentTaskLoopControllerState::Running;
            record.task_lineage.push(
                crate::agent_task_loop_controller::AgentTaskLoopTaskLineage {
                    run_id: "owned-provider-run".to_string(),
                    task_id: None,
                    parent_run_id: None,
                    parent_task_id: None,
                    entity_id: None,
                    dedupe_key: Some("dispatch".to_string()),
                    artifact_refs: Vec::new(),
                    inputs: Value::Null,
                    outputs: Value::Null,
                },
            );
            agent_task_loop_controller::write_controller(&record).expect("write controller");
            let job = AgentTaskLoopJob::new(AgentTaskLoopJobRequest {
                schema: AGENT_TASK_LOOP_JOB_SCHEMA.to_string(),
                loop_id: loop_id.to_string(),
                kind: AgentTaskLoopJobKind::DaemonExecution,
                child_pid: None,
                child_start_identity: None,
                generation: "generation-1".to_string(),
                dispatch_defaults: json!({}),
                provider_catalog: AgentTaskProviderCatalog::default(),
            })?;

            LoopWorkHandler.cancel(&job.to_checkpoint()?)?;
            assert_eq!(
                agent_task_loop_controller::load_controller(loop_id)?.state,
                AgentTaskLoopControllerState::Abandoned
            );
            Ok::<(), homeboy_core::Error>(())
        })
        .expect("new execution cancellation converges");
    }

    #[test]
    fn active_provider_execution_is_cancelled_through_the_work_job_harness() {
        with_isolated_home(|_| {
            register_loop_work_job_handler();
            let loop_id = "loop-active-provider-cancel";
            let mut record = agent_task_loop_controller::create_controller(loop_id, "repair", "v1")
                .expect("create controller");
            record.state = AgentTaskLoopControllerState::Running;
            record.record_action(
                crate::agent_task_loop_controller::AgentTaskLoopPolicyAction::SpawnTask {
                    dedupe_key: "long-provider".to_string(),
                    entity_id: None,
                    request: json!({
                        "mode": "dispatch",
                        "dispatch": { "backend": "long-provider", "prompt": "wait" }
                    }),
                },
                "active provider cancellation fixture",
            );
            agent_task_loop_controller::write_controller(&record).expect("write controller");
            let provider_marker = tempfile::tempdir().expect("provider marker directory");
            let marker_path = provider_marker.path().join("started");
            let provider: crate::agent_task_provider::AgentTaskExecutorProvider =
                serde_json::from_value(json!({
                    "id": "long-provider",
                    "backend": "long-provider",
                    "command_argv": [
                        "sh",
                        "-c",
                        format!("touch {}; exec sleep 30", marker_path.display())
                    ],
                    "capabilities": ["structured_outcome"]
                }))
                .expect("long provider fixture");
            let catalog = AgentTaskProviderCatalog {
                providers: vec![provider],
                ..Default::default()
            };
            let job = AgentTaskLoopJob::new(AgentTaskLoopJobRequest {
                schema: AGENT_TASK_LOOP_JOB_SCHEMA.to_string(),
                loop_id: loop_id.to_string(),
                kind: AgentTaskLoopJobKind::DaemonExecution,
                child_pid: None,
                child_start_identity: None,
                generation: "generation-1".to_string(),
                dispatch_defaults: json!({ "backend": "long-provider" }),
                provider_catalog: catalog,
            })?;
            let submission = work_job_submission(
                &LoopWorkHandler,
                job.idempotency_key.clone(),
                job.to_checkpoint()?,
            )?;
            let work_request = submission["request"].clone();
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = Arc::new(
                ControllerJobHarness::new(Arc::clone(&driver), work_request.clone())
                    .expect("construct provider harness"),
            );
            let prepared = driver.prepare(work_request).expect("prepare provider job");
            let thread_driver = Arc::clone(&driver);
            let thread_harness = Arc::clone(&harness);
            let started = std::time::Instant::now();
            let execution = std::thread::spawn(move || {
                thread_driver
                    .execute(prepared, thread_harness.handle())
                    .expect("provider execution returns after cancellation")
            });

            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let _provider_run_id = loop {
                let active = agent_task_loop_controller::load_controller(loop_id)
                    .expect("controller exists")
                    .metadata
                    .get("active_provider_runs")
                    .and_then(Value::as_array)
                    .and_then(|runs| runs.first())
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if let Some(run_id) = active
                    .filter(|run_id| !homeboy_agents_run_is_not_running(run_id))
                    .filter(|_| marker_path.exists())
                {
                    break run_id;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "provider admission missing"
                );
                std::thread::sleep(Duration::from_millis(20));
            };
            harness
                .request_cancellation("cancel active provider")
                .expect("request cancellation");
            driver
                .cancel(
                    &harness
                        .checkpoint()
                        .expect("read checkpoint")
                        .expect("checkpoint exists"),
                )
                .expect("cancel active provider");
            let result = execution.join().expect("join provider execution");
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "provider cancellation did not interrupt the active execution"
            );
            assert_eq!(result["result"]["controller_state"], "abandoned");
            Ok::<(), homeboy_core::Error>(())
        })
        .expect("active provider cancellation converges");
    }

    fn homeboy_agents_run_is_not_running(run_id: &str) -> bool {
        let Ok(store) =
            crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
        else {
            return true;
        };
        !store.read_record(run_id).is_ok_and(|record| {
            record.state == crate::agent_task_lifecycle::AgentTaskRunState::Running
        })
    }

    #[test]
    fn waiting_execution_stays_resumable_instead_of_becoming_completed() {
        with_isolated_home(|_| {
            let mut record = agent_task_loop_controller::create_controller(
                "loop-waiting-execution",
                "repair",
                "v1",
            )
            .expect("create controller");
            record.state = AgentTaskLoopControllerState::Waiting;
            agent_task_loop_controller::write_controller(&record).expect("write waiting state");
            let mut job = AgentTaskLoopJob::new(AgentTaskLoopJobRequest {
                schema: AGENT_TASK_LOOP_JOB_SCHEMA.to_string(),
                loop_id: record.loop_id.clone(),
                kind: AgentTaskLoopJobKind::DaemonExecution,
                child_pid: None,
                child_start_identity: None,
                generation: "generation-1".to_string(),
                dispatch_defaults: json!({}),
                provider_catalog: AgentTaskProviderCatalog::default(),
            })?;

            let step = LoopWorkHandler.observe(&mut job, WorkJobInvocation::Execute)?;
            assert!(matches!(step, WorkJobStep::Continue { .. }));
            assert_ne!(job.phase, WorkJobPhase::Completed);
            Ok::<(), homeboy_core::Error>(())
        })
        .expect("waiting execution remains resumable");
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
            harness
                .request_cancellation("cancel racing terminal replay")
                .expect("request cancellation");

            let first = driver
                .resume(checkpoint.clone(), harness.handle())
                .expect("first replay");
            let second = driver
                .resume(checkpoint, harness.handle())
                .expect("second replay");

            assert_eq!(first, second);
            assert_eq!(first["result"]["controller_state"], "completed");
            assert_eq!(
                agent_task_loop_controller::load_controller("loop-complete")
                    .expect("read terminal controller")
                    .state,
                AgentTaskLoopControllerState::Completed
            );
        });
    }

    #[test]
    fn interrupted_loop_child_admission_recovers_without_redispatch() {
        with_isolated_home(|_| {
            register_loop_work_job_handler();
            let marker = tempfile::NamedTempFile::new().expect("provider marker");
            let mut record = agent_task_loop_controller::create_controller(
                "loop-interrupted-admission",
                "repair",
                "v1",
            )
            .expect("create controller");
            record.record_action(
                agent_task_loop_controller::AgentTaskLoopPolicyAction::SpawnTask {
                    dedupe_key: "interrupted-child-admission".to_string(),
                    entity_id: None,
                    request: json!({
                        "mode": "dispatch",
                        "dispatch": { "backend": "interrupted-fixture", "prompt": "run" }
                    }),
                },
                "interrupted child admission",
            );
            agent_task_loop_controller::write_controller(&record).expect("write controller");
            let provider: crate::agent_task_provider::AgentTaskExecutorProvider =
                serde_json::from_value(json!({
                "id": "interrupted-fixture",
                "backend": "interrupted-fixture",
                "command_argv": [
                    "sh", "-c",
                    format!(
                        "printf x >> {}; printf '%s' '{{\"schema\":\"homeboy/agent-task-outcome/v1\",\"task_id\":\"fixture\",\"status\":\"succeeded\"}}'",
                        marker.path().display()
                    )
                ],
                "capabilities": ["structured_outcome"]
                }))
                .expect("provider fixture");
            let request = loop_work_job_execution_submission(
                "loop-interrupted-admission",
                "generation-1",
                json!({ "backend": "interrupted-fixture" }),
                AgentTaskProviderCatalog {
                    providers: vec![provider],
                    ..Default::default()
                },
            )?["request"]
                .clone();
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = ControllerJobHarness::new(Arc::clone(&driver), request.clone())
                .expect("construct work harness");
            let prepared = driver.prepare(request).expect("prepare work job");
            let interrupted = with_test_loop_checkpoint_interrupt(|| {
                driver.execute(prepared, harness.handle())
            });
            assert!(interrupted.is_err(), "fault hook must interrupt before checkpoint");
            assert_eq!(
                std::fs::read(marker.path()).expect("provider marker").len(),
                1
            );
            let checkpoint = harness
                .checkpoint()
                .expect("checkpoint")
                .expect("prepared checkpoint");
            assert!(checkpoint["checkpoint"]["resume"].is_null());

            let mut recovered = agent_task_loop_controller::load_controller(
                "loop-interrupted-admission",
            )?;
            recovered.state = AgentTaskLoopControllerState::Completed;
            agent_task_loop_controller::write_controller(&recovered)?;
            let result = driver.resume(checkpoint, harness.handle())?;
            assert_eq!(result["result"]["controller_state"], "completed");
            assert_eq!(
                std::fs::read(marker.path()).expect("provider marker after recovery").len(),
                1
            );
            Ok::<(), homeboy_core::Error>(())
        })
        .expect("interrupted admission recovery converges");
    }

    #[test]
    fn cancellation_preserves_a_controller_that_terminalized_while_child_lives() {
        with_isolated_home(|_| {
            let loop_id = "loop-cancel-racing-terminal";
            let mut record = agent_task_loop_controller::create_controller(loop_id, "repair", "v1")
                .expect("create controller");
            let mut child = std::process::Command::new("sh")
                .args(["-c", "sleep 30"])
                .spawn()
                .expect("spawn coordinator fixture");
            let identity = homeboy_core::process::process_start_identity(child.id())
                .expect("inspect fixture")
                .expect("fixture identity");
            let request = loop_work_job_submission(loop_id, child.id(), &identity)
                .expect("build submission")["request"]
                .clone();
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = ControllerJobHarness::new(Arc::clone(&driver), request.clone())
                .expect("construct work harness");
            let prepared = driver.prepare(request).expect("prepare loop work");

            record.state = AgentTaskLoopControllerState::Completed;
            agent_task_loop_controller::write_controller(&record).expect("complete controller");
            harness
                .request_cancellation("cancel racing terminal controller")
                .expect("request cancellation");

            driver
                .cancel(&prepared)
                .expect("authoritative cancel recheck");
            let result = driver
                .resume(prepared, harness.handle())
                .expect("preserve terminal controller outcome");

            assert_eq!(result["result"]["phase"], "completed");
            assert_eq!(result["result"]["controller_state"], "completed");
            assert!(matches!(
                homeboy_core::process::process_identity_state(child.id(), None),
                ProcessIdentityState::Live
            ));

            homeboy_core::process::terminate_process_tree(child.id()).expect("clean fixture child");
            let _ = child.wait();
        });
    }

    #[test]
    fn idle_ticks_do_not_repeat_checkpoint_or_progress_writes() {
        with_isolated_home(|_| {
            agent_task_loop_controller::create_controller("loop-idle", "repair", "v1")
                .expect("create controller");
            let child = std::process::Command::new("sh")
                .args(["-c", "sleep 30"])
                .spawn()
                .expect("spawn coordinator fixture");
            let identity = homeboy_core::process::process_start_identity(child.id())
                .expect("inspect fixture")
                .expect("fixture identity");
            let request = loop_work_job_submission("loop-idle", child.id(), &identity)
                .expect("build submission")["request"]
                .clone();
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = Arc::new(
                ControllerJobHarness::new(Arc::clone(&driver), request.clone())
                    .expect("construct work harness"),
            );
            let prepared = driver.prepare(request).expect("prepare loop work");
            let thread_harness = Arc::clone(&harness);
            let thread_driver = Arc::clone(&driver);
            let execution = std::thread::spawn(move || {
                thread_driver
                    .execute(prepared, thread_harness.handle())
                    .expect("execute idle loop");
            });

            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while harness.events().expect("read events").len() < 2 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "initial events missing"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            let initial_events = harness.events().expect("read initial events").len();
            std::thread::sleep(Duration::from_millis(650));
            assert_eq!(
                harness.events().expect("read idle events").len(),
                initial_events
            );

            harness
                .request_cancellation("stop idle loop")
                .expect("request cancellation");
            driver
                .cancel(
                    &harness
                        .checkpoint()
                        .expect("read checkpoint")
                        .expect("checkpoint"),
                )
                .expect("cancel coordinator");
            execution.join().expect("join loop execution");
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
