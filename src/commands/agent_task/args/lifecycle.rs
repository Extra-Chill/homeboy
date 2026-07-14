use clap::Args;

#[derive(Args, Debug)]
pub struct RunPlanArgs {
    /// AgentTaskPlan JSON file, @file, or - for stdin.
    #[arg(long, value_name = "PATH")]
    pub plan: String,
    /// Also persist the completed run lifecycle record under this id.
    #[arg(long, value_name = "ID")]
    pub record_run_id: Option<String>,
    /// Provider wall-clock timeout in milliseconds. Overrides the plan timeout.
    #[arg(long = "timeout-ms", value_name = "MS")]
    pub timeout_ms: Option<u64>,
}

#[derive(Args, Debug)]
pub struct RunArgs {
    /// Durable run id returned by `agent-task submit` or `agent-task run-plan --record-run-id`.
    pub run_id: String,
    /// Provider wall-clock timeout in milliseconds. Overrides the submitted plan timeout.
    #[arg(long = "timeout-ms", value_name = "MS")]
    pub timeout_ms: Option<u64>,
}

#[derive(Args, Debug)]
pub struct SubmitArgs {
    /// AgentTaskPlan JSON file, @file, or - for stdin.
    #[arg(long, value_name = "PATH")]
    pub plan: String,
    /// Optional durable run id. Generated when omitted.
    #[arg(long, value_name = "ID")]
    pub run_id: Option<String>,
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Durable run id returned by `agent-task submit` or `agent-task run-plan --record-run-id`.
    pub run_id: String,
    /// Emit the bridge-friendly durable run status envelope.
    #[arg(long)]
    pub bridge: bool,
    /// Return only bridge events after this cursor.
    #[arg(long, value_name = "CURSOR", requires = "bridge")]
    pub since_cursor: Option<u64>,
    /// Emit the full verbose payload (all artifact/evidence refs) instead of the
    /// default compact, recovery-first summary.
    #[arg(long, conflicts_with = "bridge")]
    pub full: bool,
}

#[derive(Args, Debug)]
pub struct EvidenceArgs {
    /// Durable run id returned by `agent-task submit` or `agent-task run-plan --record-run-id`.
    pub run_id: String,
    /// Evidence kind to hydrate, such as executor-result, executor-input, or transcript.
    #[arg(long = "kind", value_name = "KIND")]
    pub kind: Option<String>,
    /// Task id to hydrate evidence for.
    #[arg(long = "task", value_name = "TASK_ID")]
    pub task: Option<String>,
    /// Only include evidence attached to failed/provider-error/timed-out task outcomes.
    #[arg(long = "failure-only")]
    pub failure_only: bool,
}

#[derive(Args, Debug)]
pub struct DiagnoseArgs {
    /// Durable run id returned by `agent-task submit` or `agent-task run-plan --record-run-id`.
    pub run_id: String,
}

#[derive(Args, Debug)]
pub struct ReplayProviderBoundaryArgs {
    /// Durable run id whose latest executor input should be inspected.
    pub run_id: String,
    /// Task id to inspect when the run has multiple executor-input evidence refs.
    #[arg(long = "task", value_name = "TASK_ID")]
    pub task: Option<String>,
}

#[derive(Args, Debug)]
pub struct RetryArgs {
    /// Existing durable run id whose plan should be retried.
    pub run_id: String,
    /// Optional durable run id for the retry. Generated when omitted.
    #[arg(long, value_name = "ID")]
    pub new_run_id: Option<String>,
    /// Execute the newly queued retry immediately.
    #[arg(long)]
    pub run: bool,
}

#[derive(Args, Debug)]
pub struct CancelArgs {
    /// Durable run id returned by `agent-task submit` or `agent-task run-plan --record-run-id`.
    pub run_id: String,
    /// Operator-visible reason stored on the durable run record.
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,
}
