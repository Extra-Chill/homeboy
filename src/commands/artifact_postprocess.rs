use std::path::PathBuf;

use clap::Args;
use homeboy::core::artifacts::{
    run_artifact_postprocess_plan_for_persisted_root, ArtifactPostprocessPlan,
};
use serde::Serialize;

use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct ArtifactPostprocessArgs {
    /// Artifact postprocess plan JSON file, @file spec, or - for stdin.
    #[arg(value_name = "PLAN")]
    pub plan: String,

    /// Artifact root id from the plan to use as HOMEBOY_ARTIFACT_POSTPROCESS_ARTIFACT_ROOT.
    #[arg(long, value_name = "ID")]
    pub artifact_root_id: Option<String>,

    /// Optional artifact root id from the plan to expose as ${run.input}.
    #[arg(long, value_name = "ID")]
    pub input_root_id: Option<String>,

    /// Write the bare artifact-postprocess result contract to this path.
    #[arg(long, value_name = "PATH")]
    pub result: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct ArtifactPostprocessCommandOutput {
    pub command: &'static str,
    pub plan_file: String,
    pub artifact_root_id: Option<String>,
    pub input_root_id: Option<String>,
    pub result_file: Option<String>,
    pub result: homeboy::core::artifacts::ArtifactPostprocessResult,
}

pub fn run(
    args: ArtifactPostprocessArgs,
    _global: &GlobalArgs,
) -> CmdResult<ArtifactPostprocessCommandOutput> {
    let raw = homeboy::core::config::read_json_spec_to_string(&args.plan)?;
    let plan: ArtifactPostprocessPlan = serde_json::from_str(&raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_json(
            error,
            Some("parse artifact postprocess plan".to_string()),
            Some(args.plan.clone()),
        )
    })?;
    let result = run_artifact_postprocess_plan_for_persisted_root(
        &plan,
        args.artifact_root_id.as_deref(),
        args.input_root_id.as_deref(),
    )?;
    if let Some(path) = args.result.as_ref() {
        write_result(path, &result)?;
    }
    let exit_code = if result.success { 0 } else { 1 };

    Ok((
        ArtifactPostprocessCommandOutput {
            command: "artifact-postprocess",
            plan_file: args.plan,
            artifact_root_id: args.artifact_root_id,
            input_root_id: args.input_root_id,
            result_file: args.result.map(|path| path.to_string_lossy().to_string()),
            result,
        },
        exit_code,
    ))
}

fn write_result(
    path: &std::path::Path,
    result: &homeboy::core::artifacts::ArtifactPostprocessResult,
) -> homeboy::core::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(parent.display().to_string()))
        })?;
    }
    let json = homeboy::core::config::to_json_string(result)?;
    std::fs::write(path, format!("{json}\n")).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })
}
