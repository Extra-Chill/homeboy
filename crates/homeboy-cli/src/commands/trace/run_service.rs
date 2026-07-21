use homeboy::core::engine::run_dir::RunDir;
use homeboy::rig;
use homeboy_extension::trace as extension_trace;

use super::{execute_trace_run_impl, TraceArgs};

pub(super) struct TraceRunRequest {
    args: TraceArgs,
}

impl TraceRunRequest {
    fn new(args: TraceArgs) -> Self {
        Self { args }
    }
}

pub(super) struct TraceRunService;

pub(super) struct TraceRunExecution {
    pub(super) workflow: extension_trace::TraceRunWorkflowResult,
    pub(super) run_dir: RunDir,
    pub(super) artifact_path: Option<String>,
    pub(super) artifact_dir: Option<String>,
    pub(super) evidence_promoted: bool,
    pub(super) rig_state: Option<rig::RigStateSnapshot>,
    /// Observation run id of the child trace run, when an observation store was
    /// available. Surfaced so compare orchestration can link child run records
    /// into the first-class compare pair artifact instead of forcing downstream
    /// reporting to rediscover run ids from artifact directories.
    pub(super) run_id: Option<String>,
}

impl TraceRunExecution {
    pub(super) fn artifact_path(&self) -> String {
        self.artifact_path.clone().unwrap_or_else(|| {
            self.run_dir
                .step_file(homeboy::core::engine::run_dir::files::TRACE_RESULTS)
                .to_string_lossy()
                .to_string()
        })
    }

    pub(super) fn artifact_dir(&self) -> String {
        self.artifact_dir.clone().unwrap_or_else(|| {
            self.run_dir
                .path()
                .join("artifacts")
                .to_string_lossy()
                .to_string()
        })
    }

    pub(super) fn finish(&self, success: bool) {
        self.run_dir.finish(success && self.evidence_promoted);
    }
}

impl TraceRunService {
    fn execute(&self, request: TraceRunRequest) -> homeboy::core::Result<TraceRunExecution> {
        execute_trace_run_impl(request.args)
    }
}

pub(super) fn execute_trace_run(args: TraceArgs) -> homeboy::core::Result<TraceRunExecution> {
    TraceRunService.execute(TraceRunRequest::new(args))
}
