//! Clap argument definitions for the `agent-task controller` subcommand tree.

use clap::{Args, Subcommand};

#[derive(Subcommand, Debug)]
pub enum AgentTaskControllerCommand {
    /// Create a durable loop controller record.
    Init(AgentTaskControllerInitArgs),
    /// Initialize or resume a durable loop controller from a repo-authored JSON spec.
    ///
    /// With a configured default Lab runner, --resume uses automatic Lab offload unless local execution is explicitly forced.
    FromSpec(AgentTaskControllerFromSpecArgs),
    /// Materialize, initialize, and run a bounded controller loop from a repo-authored JSON spec.
    ///
    /// With a configured default Lab runner, this uses automatic Lab offload unless local execution is explicitly forced.
    RunFromSpec(AgentTaskControllerRunFromSpecArgs),
    /// Materialize a repo-authored loop spec with explicit run inputs.
    ///
    /// With a configured default Lab runner, this uses automatic Lab offload unless local execution is explicitly forced.
    Materialize(AgentTaskControllerMaterializeArgs),
    /// Validate a proof, materialized spec, or controller record for deterministic handoff.
    ValidateProof(AgentTaskControllerValidateProofArgs),
    /// Compile a controller spec into a dry Homeboy plan without writing state.
    Plan(AgentTaskControllerPlanArgs),
    /// Read a durable loop controller record.
    Status(AgentTaskControllerStatusArgs),
    /// List durable loop controller records.
    List,
    /// Apply a generic external controller event.
    Events(AgentTaskControllerApplyEventArgs),
    /// Apply an external event and resume matching waits.
    ApplyEvent(AgentTaskControllerApplyEventArgs),
    /// Claim and execute the next pending controller action.
    RunNext(AgentTaskControllerRunNextArgs),
    /// Claim and execute one pending controller action.
    Run(AgentTaskControllerRunArgs),
    /// Execute pending controller actions until no executable action remains.
    Resume(AgentTaskControllerRunNextArgs),
    /// Mark a tracked entity as human-ready work.
    MarkHumanReady(AgentTaskControllerMarkHumanReadyArgs),
    /// Run a one-command end-to-end controller proof from a named profile + runner.
    Proof(AgentTaskControllerProofArgs),
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerInitArgs {
    /// Durable loop id. Unsafe path characters are normalized for storage.
    pub loop_id: String,

    /// Initial controller phase.
    #[arg(long, default_value = "init", value_name = "PHASE")]
    pub phase: String,

    /// Declared graph/config version for resume compatibility.
    #[arg(long = "config-version", default_value = "v1", value_name = "VERSION")]
    pub config_version: String,
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerFromSpecArgs {
    /// Repo loop spec JSON, @file, or - for stdin.
    #[arg(value_name = "SPEC")]
    pub spec: String,

    /// Execute pending actions after applying the spec.
    #[arg(long)]
    pub resume: bool,

    /// Explicit controller run inputs JSON, @file, or - for stdin. Supports `inputs` and `metadata` objects.
    #[arg(long, value_name = "JSON")]
    pub inputs: Option<String>,

    /// Declarative policy result JSON, @file, or - for stdin. Repeatable.
    #[arg(long = "policy-result", value_name = "JSON")]
    pub policy_results: Vec<String>,

    /// Maximum controller actions to execute when --resume is supplied.
    #[arg(
        long = "max-actions",
        visible_alias = "max-iterations",
        value_name = "N"
    )]
    pub max_actions: Option<u32>,

    /// On --resume, discard stale persisted controller state and re-create it from this spec.
    #[arg(long, conflicts_with_all = ["fork", "resume_existing"])]
    pub replace: bool,

    /// On --resume, apply this spec under a derived fork loop id, leaving the original untouched.
    #[arg(long, conflicts_with_all = ["replace", "resume_existing"])]
    pub fork: bool,

    /// On --resume, accept stale/mismatched persisted state and resume the existing controller as-is.
    #[arg(long = "resume-existing", conflicts_with_all = ["replace", "fork"])]
    pub resume_existing: bool,

    /// Compile and preflight generic controller prerequisites without writing state.
    #[arg(long)]
    pub doctor: bool,

    /// Executor backend to use for controller-spawned dispatch actions when the action omits one.
    #[arg(long = "dispatch-backend", value_name = "BACKEND")]
    pub dispatch_backend: Option<String>,

    /// Extension-provider selector: the Homeboy executor provider id (e.g.
    /// `wordpress.codebox-agent-task-executor`) that runs controller-spawned
    /// dispatch actions when the action omits one. This is NOT a model or AI
    /// runtime name (codex, opencode, claude-code) — pass those in
    /// --dispatch-provider-config. Run `homeboy agent-task providers` for valid ids.
    #[arg(
        long = "dispatch-selector",
        visible_alias = "dispatch-provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub dispatch_selector: Option<String>,

    /// Model override to use for controller-spawned dispatch actions when the action omits one.
    #[arg(long = "dispatch-model", value_name = "MODEL")]
    pub dispatch_model: Option<String>,

    /// Agent/model provider config (JSON, @file, or -): the nested AI
    /// runtime/provider/model the selected executor uses for controller-spawned
    /// dispatch actions when the action omits one. Put AI runtime names like
    /// `codex`/`opencode`/`claude-code` here, not in --dispatch-selector.
    #[arg(long = "dispatch-provider-config", value_name = "JSON")]
    pub dispatch_provider_config: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerRunFromSpecArgs {
    /// Repo loop spec JSON, @file, -, or a generator manifest that writes a spec file.
    #[arg(value_name = "SPEC")]
    pub spec: String,

    /// Explicit run inputs JSON, @file, or - for stdin. Supports `inputs` and `metadata` objects.
    #[arg(long, value_name = "JSON")]
    pub inputs: Option<String>,

    /// Declarative policy result JSON, @file, or - for stdin. Repeatable.
    #[arg(long = "policy-result", value_name = "JSON")]
    pub policy_results: Vec<String>,

    /// Maximum controller actions to execute before returning a bounded partial result.
    #[arg(
        long = "max-actions",
        visible_alias = "max-iterations",
        value_name = "N"
    )]
    pub max_actions: u32,

    /// One-flag safe proof-run mode: automatically reset stale persisted controller
    /// state and re-derive isolated run-scoped state from this spec, with no manual
    /// state cleanup. Use this for proof/rerun workflows when the persisted spec
    /// fingerprint conflicts with the requested spec.
    #[arg(long = "reconcile-stale", conflicts_with_all = ["replace", "fork", "resume_existing"])]
    pub reconcile_stale: bool,

    /// Discard stale persisted controller state and re-create it from this spec before running.
    #[arg(long, conflicts_with_all = ["fork", "resume_existing", "reconcile_stale"])]
    pub replace: bool,

    /// Apply this spec under a derived fork loop id, leaving the original controller untouched.
    #[arg(long, conflicts_with_all = ["replace", "resume_existing", "reconcile_stale"])]
    pub fork: bool,

    /// Accept stale/mismatched persisted state and resume the existing controller as-is.
    #[arg(long = "resume-existing", conflicts_with_all = ["replace", "fork", "reconcile_stale"])]
    pub resume_existing: bool,

    /// Executor backend to use for controller-spawned dispatch actions when the action omits one.
    #[arg(long = "dispatch-backend", value_name = "BACKEND")]
    pub dispatch_backend: Option<String>,

    /// Extension-provider selector: the Homeboy executor provider id (e.g.
    /// `wordpress.codebox-agent-task-executor`) that runs controller-spawned
    /// dispatch actions when the action omits one. This is NOT a model or AI
    /// runtime name (codex, opencode, claude-code) — pass those in
    /// --dispatch-provider-config. Run `homeboy agent-task providers` for valid ids.
    #[arg(
        long = "dispatch-selector",
        visible_alias = "dispatch-provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub dispatch_selector: Option<String>,

    /// Model override to use for controller-spawned dispatch actions when the action omits one.
    #[arg(long = "dispatch-model", value_name = "MODEL")]
    pub dispatch_model: Option<String>,

    /// Agent/model provider config (JSON, @file, or -): the nested AI
    /// runtime/provider/model the selected executor uses for controller-spawned
    /// dispatch actions when the action omits one. Put AI runtime names like
    /// `codex`/`opencode`/`claude-code` here, not in --dispatch-selector.
    #[arg(long = "dispatch-provider-config", value_name = "JSON")]
    pub dispatch_provider_config: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerMaterializeArgs {
    /// Repo loop spec JSON, @file, -, or a generator manifest that writes a spec file.
    #[arg(value_name = "SPEC")]
    pub spec: String,

    /// Explicit run inputs JSON, @file, or - for stdin. Supports `inputs` and `metadata` objects.
    #[arg(long, value_name = "JSON")]
    pub inputs: Option<String>,

    /// Declarative policy result JSON, @file, or - for stdin. Repeatable.
    #[arg(long = "policy-result", value_name = "JSON")]
    pub policy_results: Vec<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerValidateProofArgs {
    /// Proof JSON, materialize output JSON, controller record JSON, @file, or - for stdin.
    #[arg(value_name = "JSON")]
    pub input: String,
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerPlanArgs {
    /// Controller spec JSON, @file, or - for stdin.
    #[arg(value_name = "SPEC")]
    pub spec: String,
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerStatusArgs {
    /// Durable loop id returned by `agent-task controller init`.
    pub loop_id: String,
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerApplyEventArgs {
    /// Durable loop id returned by `agent-task controller init`.
    pub loop_id: String,

    /// External event type, for example github.pr.merged or task.completed.
    #[arg(long = "event-type", value_name = "TYPE")]
    pub event_type: String,

    /// Stable event id. Generated from the loop history length when omitted.
    #[arg(long = "event-id", value_name = "ID")]
    pub event_id: Option<String>,

    /// Optional deterministic event key, such as repo#pr or a check-suite id.
    #[arg(long = "event-key", value_name = "KEY")]
    pub event_key: Option<String>,

    /// Optional target entity id for wait matching and lineage.
    #[arg(long = "entity-id", value_name = "ID")]
    pub entity_id: Option<String>,

    /// Event payload JSON, @file, or - for stdin. May contain a `policy` object to evaluate.
    #[arg(long, value_name = "JSON")]
    pub payload: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerRunNextArgs {
    /// Durable loop id returned by `agent-task controller init`.
    pub loop_id: String,

    /// Executor backend to use for controller-spawned dispatch actions when the action omits one.
    #[arg(long = "dispatch-backend", value_name = "BACKEND")]
    pub dispatch_backend: Option<String>,

    /// Extension-provider selector: the Homeboy executor provider id (e.g.
    /// `wordpress.codebox-agent-task-executor`) that runs controller-spawned
    /// dispatch actions when the action omits one. This is NOT a model or AI
    /// runtime name (codex, opencode, claude-code) — pass those in
    /// --dispatch-provider-config. Run `homeboy agent-task providers` for valid ids.
    #[arg(
        long = "dispatch-selector",
        visible_alias = "dispatch-provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub dispatch_selector: Option<String>,

    /// Model override to use for controller-spawned dispatch actions when the action omits one.
    #[arg(long = "dispatch-model", value_name = "MODEL")]
    pub dispatch_model: Option<String>,

    /// Agent/model provider config (JSON, @file, or -): the nested AI
    /// runtime/provider/model the selected executor uses for controller-spawned
    /// dispatch actions when the action omits one. Put AI runtime names like
    /// `codex`/`opencode`/`claude-code` here, not in --dispatch-selector.
    #[arg(long = "dispatch-provider-config", value_name = "JSON")]
    pub dispatch_provider_config: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerRunArgs {
    /// Durable loop id returned by `agent-task controller init`.
    pub loop_id: String,

    /// Pending controller action id to execute.
    #[arg(long = "action-id", value_name = "ID")]
    pub action_id: String,

    /// Executor backend to use for controller-spawned dispatch actions when the action omits one.
    #[arg(long = "dispatch-backend", value_name = "BACKEND")]
    pub dispatch_backend: Option<String>,

    /// Extension-provider selector: the Homeboy executor provider id (e.g.
    /// `wordpress.codebox-agent-task-executor`) that runs controller-spawned
    /// dispatch actions when the action omits one. This is NOT a model or AI
    /// runtime name (codex, opencode, claude-code) — pass those in
    /// --dispatch-provider-config. Run `homeboy agent-task providers` for valid ids.
    #[arg(
        long = "dispatch-selector",
        visible_alias = "dispatch-provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub dispatch_selector: Option<String>,

    /// Model override to use for controller-spawned dispatch actions when the action omits one.
    #[arg(long = "dispatch-model", value_name = "MODEL")]
    pub dispatch_model: Option<String>,

    /// Agent/model provider config (JSON, @file, or -): the nested AI
    /// runtime/provider/model the selected executor uses for controller-spawned
    /// dispatch actions when the action omits one. Put AI runtime names like
    /// `codex`/`opencode`/`claude-code` here, not in --dispatch-selector.
    #[arg(long = "dispatch-provider-config", value_name = "JSON")]
    pub dispatch_provider_config: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerProofArgs {
    /// Named proof profile (intent + policy). Resolved from the registry passed
    /// via --profiles; the orchestration never branches on the profile name.
    #[arg(long, value_name = "NAME")]
    pub profile: String,

    /// Runner to dispatch the proof through (for example a Lab runner id).
    #[arg(long, value_name = "RUNNER")]
    pub runner: String,

    /// Proof profile registry JSON, @file, or - for stdin: a generic object
    /// mapping profile names to profile definitions. Keeps profile data out of
    /// core so adding a profile is pure data.
    #[arg(long, value_name = "JSON")]
    pub profiles: Option<String>,

    /// Optional explicit seed material for run-scoped identity. Defaults to a
    /// fresh timestamp so each invocation derives an isolated run/loop id.
    #[arg(long = "seed", value_name = "SEED")]
    pub seed: Option<String>,

    /// Maximum controller actions to execute once preflight passes.
    #[arg(
        long = "max-actions",
        visible_alias = "max-iterations",
        value_name = "N",
        default_value = "100"
    )]
    pub max_actions: u32,

    /// Run preflight reconciliation only; do not dispatch even when it passes.
    #[arg(long)]
    pub preflight_only: bool,
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerMarkHumanReadyArgs {
    /// Durable loop id returned by `agent-task controller init`.
    pub loop_id: String,

    /// Entity id to mark human-ready.
    #[arg(long = "entity-id", value_name = "ID")]
    pub entity_id: String,

    /// Operator-visible reason stored in loop history.
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,
}
