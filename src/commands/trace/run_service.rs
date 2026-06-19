use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::trace as extension_trace;
use homeboy::core::rig;

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
    pub(super) rig_state: Option<rig::RigStateSnapshot>,
}

impl TraceRunService {
    fn execute(&self, request: TraceRunRequest) -> homeboy::core::Result<TraceRunExecution> {
        execute_trace_run_impl(request.args)
    }
}

pub(super) fn execute_trace_run(args: TraceArgs) -> homeboy::core::Result<TraceRunExecution> {
    TraceRunService.execute(TraceRunRequest::new(args))
}
