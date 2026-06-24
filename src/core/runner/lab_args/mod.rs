//! Lab offload argument transformation.
//!
//! Splits controller→runner argument rewriting into cohesive submodules:
//! - [`path_remap`]: local→remote path remapping primitives.
//! - [`provider_config`]: `--provider-config` inlining/remapping/injection.
//! - [`agent_task_specs`]: agent-task `--plan`/`--prompt`/`--task`/`--tasks`
//!   `@file` materialization.
//! - [`offload`]: offload source-path resolution and controller-only flag
//!   stripping/rewriting.
//!
//! This root stays thin: it owns the shared passthrough sentinel and re-exports
//! the surface the rest of the `runner` module consumes.

mod agent_task_specs;
mod at_files;
mod envelope;
mod offload;
mod path_remap;
mod provider_config;

#[cfg(test)]
mod tests;

/// Sentinel inserted to mark an explicit user-provided passthrough boundary so
/// Lab offload rewriting can distinguish it from synthesized passthrough args.
pub(super) const EXPLICIT_PASSTHROUGH_SENTINEL: &str = "__homeboy_explicit_passthrough__";

#[cfg(test)]
pub(super) use agent_task_specs::materialize_inline_agent_task_json_specs_in_args;
#[cfg(test)]
pub(super) use agent_task_specs::{
    inline_agent_task_prompt_files_in_args, remap_agent_task_plan_in_args,
};
pub(super) use agent_task_specs::{materialize_agent_task_specs_in_args, AgentTaskInlineJsonSpec};
pub(super) use at_files::{lab_at_file_specs, remap_lab_at_file_args, LabAtFileSpec};
pub(super) use offload::{
    lab_offload_source_path, rewrite_lab_offload_args, rewrite_runner_resident_lab_offload_args,
};
pub(super) use path_remap::{remap_path_settings_in_args, LabPathRemap};
pub(super) use provider_config::{
    inject_agent_task_default_provider_config_in_args,
    preflight_provider_config_paths_materialized_in_args, provider_config_runtime_manifest,
    remap_provider_config_in_args,
};
