//! Clap argument definitions for the `agent-task controller` subcommand tree.

use clap::{Args, Subcommand};

#[derive(Subcommand, Debug)]
pub enum AgentTaskControllerCommand {
    /// Create a durable loop controller record.
    Init(AgentTaskControllerInitArgs),
    /// Initialize or resume a durable loop controller from a repo-authored JSON spec.
    FromSpec(AgentTaskControllerFromSpecArgs),
    /// Materialize a repo-authored loop spec with explicit run inputs.
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
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerMaterializeArgs {
    /// Repo loop spec JSON, @file, or - for stdin.
    #[arg(value_name = "SPEC")]
    pub spec: String,

    /// Explicit run inputs JSON, @file, or - for stdin. Supports `inputs` and `metadata` objects.
    #[arg(long, value_name = "JSON")]
    pub inputs: Option<String>,
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
}

#[derive(Args, Debug)]
pub struct AgentTaskControllerRunArgs {
    /// Durable loop id returned by `agent-task controller init`.
    pub loop_id: String,

    /// Pending controller action id to execute.
    #[arg(long = "action-id", value_name = "ID")]
    pub action_id: String,
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
