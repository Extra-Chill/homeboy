use std::path::PathBuf;

use clap::Args;
use homeboy::core::artifacts::{
    record_artifact_postprocess_outputs, run_artifact_postprocess_plan_for_persisted_root,
    ArtifactPostprocessPlan,
};
use serde::Serialize;

use super::CmdResult;

#[derive(Args, Clone)]
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

    /// Persist produced artifacts as evidence on this existing observation run. Lab runners use this to make postprocess output resolvable through run evidence.
    #[arg(long, value_name = "RUN_ID")]
    pub run_id: Option<String>,

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
    pub run_id: Option<String>,
    pub recorded_artifact_count: usize,
    pub result_file: Option<String>,
    pub result: homeboy::core::artifacts::ArtifactPostprocessResult,
}

pub fn run(args: ArtifactPostprocessArgs) -> CmdResult<ArtifactPostprocessCommandOutput> {
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
    let recorded_artifact_count = if let Some(run_id) = args.run_id.as_deref() {
        let store = homeboy::core::observation::ObservationStore::open_initialized()?;
        record_artifact_postprocess_outputs(&store, run_id, &result.outputs)?.len()
    } else {
        0
    };
    if let Some(path) = args.result.as_ref() {
        write_result(path, &result)?;
    }
    let exit_code = if result.success { 0 } else { 1 };

    Ok((
        ArtifactPostprocessCommandOutput {
            command: "runs.artifact.postprocess",
            plan_file: args.plan,
            artifact_root_id: args.artifact_root_id,
            input_root_id: args.input_root_id,
            run_id: args.run_id,
            recorded_artifact_count,
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

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::core::observation::{runs_service, NewRunRecord, ObservationStore};

    #[test]
    fn lab_contract_postprocess_persists_binary_artifact_evidence() {
        homeboy::core::test_support::with_isolated_home(|home| {
            let artifact_root = home.path().join("lab-output");
            let plan_path = home.path().join("postprocess.json");
            std::fs::write(
                &plan_path,
                serde_json::json!({
                    "schema": "homeboy/artifact-postprocess/v1",
                    "plan_id": "lab-binary",
                    "artifact_roots": [{ "id": "output", "path": artifact_root }],
                    "actions": [{
                        "id": "binary", "helper": "sh", "action": "-c", "output": "report.bin",
                        "parameters": { "args": ["printf '\\001\\377\\020' > \"$HOMEBOY_ARTIFACT_POSTPROCESS_OUTPUT\""] }
                    }]
                })
                .to_string(),
            )
            .expect("plan");
            let store = ObservationStore::open_initialized().expect("store");
            let observation = store
                .start_run(
                    NewRunRecord::builder("lab-runner")
                        .command("homeboy runs artifact postprocess")
                        .metadata(serde_json::json!({ "runner_id": "homeboy-lab" }))
                        .build(),
                )
                .expect("run");

            let (output, exit_code) = run(ArtifactPostprocessArgs {
                plan: format!("@{}", plan_path.display()),
                artifact_root_id: None,
                input_root_id: None,
                run_id: Some(observation.id.clone()),
                result: None,
            })
            .expect("postprocess command");
            let evidence = runs_service::list_artifacts_for_run(&store, &observation.id)
                .expect("run evidence");

            assert_eq!(exit_code, 0);
            assert_eq!(output.recorded_artifact_count, 1);
            assert_eq!(
                std::fs::read(&evidence[0].path).expect("binary artifact"),
                [1, 255, 16]
            );
        });
    }
}
