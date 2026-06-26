//! Top-level `runs` command dispatch and `RunsArgs` inherent helpers.
//!
//! Routes parsed subcommands to their handlers and provides the global
//! `--runner` guidance surfaced when operators misuse the top-level flag.

use homeboy::core::Error;

use super::types::{RunsArgs, RunsArtifactArgs, RunsArtifactCommand, RunsCommand, RunsOutput};
use super::{
    bench, compare, distribution, drift, evidence, findings, fuzz_compare, handlers, hotspots,
    latest, loop_sync, query, reconcile, refs,
};
use super::{CmdResult, GlobalArgs};

impl RunsArgs {
    /// Whether this is a `runs show <id>` invocation eligible for the
    /// compact human summary (i.e. the caller did not pass `--json`).
    pub fn show_summary_eligible(&self) -> bool {
        matches!(self.command, RunsCommand::Show { json: false, .. })
    }

    pub fn absorb_global_runner_for_list(&mut self, runner: Option<String>) -> Option<String> {
        match (&mut self.command, runner) {
            (RunsCommand::List(args), Some(runner_id)) if args.runner.is_none() => {
                args.runner = Some(runner_id);
                None
            }
            (RunsCommand::List(args), Some(runner_id))
                if args.runner.as_deref() == Some(runner_id.as_str()) =>
            {
                None
            }
            (_, runner) => runner,
        }
    }

    pub fn list_runner(&self) -> Option<&str> {
        match &self.command {
            RunsCommand::List(args) => args.runner.as_deref(),
            _ => None,
        }
    }

    pub fn is_markdown_mode(&self) -> bool {
        matches!(self.command, RunsCommand::Compare(ref compare) if compare::is_table_mode(compare))
    }

    pub fn is_bundle_export(&self) -> bool {
        matches!(self.command, RunsCommand::Export(_))
    }

    pub fn is_artifact_get(&self) -> bool {
        matches!(
            self.command,
            RunsCommand::Artifact(RunsArtifactArgs {
                command: RunsArtifactCommand::Get(_),
            })
        )
    }

    pub fn has_command_local_runner_option(&self) -> bool {
        matches!(
            self.command,
            RunsCommand::Artifact(RunsArtifactArgs {
                command: RunsArtifactCommand::Attach(_),
            })
        )
    }

    fn global_runner_guidance(&self, runner_id: &str) -> (String, Vec<String>) {
        match &self.command {
            RunsCommand::List(_) => (
                format!(
                    "Use the runs-list runner option after the subcommand: `homeboy runs list --runner {runner_id}`."
                ),
                vec![
                    "The top-level --runner flag is reserved for Lab offload commands, not observation-store queries.".to_string(),
                    format!("Run `homeboy runs list --runner {runner_id}` to query the connected runner daemon."),
                ],
            ),
            RunsCommand::Show { run_id, .. }
            | RunsCommand::ResumePlan { run_id }
            | RunsCommand::Evidence { run_id }
            | RunsCommand::Env { run_id }
            | RunsCommand::Artifacts { run_id } => (
                format!(
                    "Lab-offloaded run records are mirrored locally; inspect run `{run_id}` with `homeboy runs show {run_id}` without --runner."
                ),
                vec![
                    format!("Run `homeboy runs show {run_id}` to inspect the mirrored local run record."),
                    format!("Run `homeboy runs artifacts {run_id}` to list mirrored artifact records."),
                    "Use `homeboy runs artifact get <run-id> <artifact-id>` for retrievable runner artifacts recorded in the local observation store.".to_string(),
                ],
            ),
            RunsCommand::Artifact(_) => (
                "Runner artifact commands use the local mirrored observation store; rerun without top-level --runner.".to_string(),
                vec![
                    "Run `homeboy runs artifacts <run-id>` without --runner to find the artifact id.".to_string(),
                    "Run `homeboy runs artifact get <run-id> <artifact-id>` without --runner to retrieve a recorded runner artifact.".to_string(),
                ],
            ),
            _ => (
                "The top-level --runner flag is reserved for Lab offload commands; runs queries inspect the local observation store unless a subcommand documents its own --runner option.".to_string(),
                vec![
                    "Omit top-level --runner for local mirrored run records.".to_string(),
                    "Use `homeboy runs list --runner <id>` only when listing runs from a connected runner daemon.".to_string(),
                ],
            ),
        }
    }
}

pub fn run(args: RunsArgs, _global: &GlobalArgs) -> CmdResult<RunsOutput> {
    match args.command {
        RunsCommand::List(args) => handlers::list_runs(args, "runs.list"),
        RunsCommand::Distribution(args) => {
            distribution::runs_distribution(args, "runs.distribution")
        }
        RunsCommand::LatestRun(args) => latest::latest_run(args),
        RunsCommand::Compare(args) => compare::compare_runs(args),
        RunsCommand::BenchCompare(args) => bench::bench_compare_from_args(args),
        RunsCommand::FuzzCompare(args) => fuzz_compare::fuzz_compare_from_args(args),
        RunsCommand::Hotspots(args) => hotspots::runs_hotspots(args),
        RunsCommand::Reconcile(args) => reconcile::reconcile_runs(args),
        RunsCommand::Show { run_id, json: _ } => handlers::show_run(&run_id),
        RunsCommand::ResumePlan { run_id } => handlers::resume_plan(&run_id),
        RunsCommand::Evidence { run_id } => evidence::evidence(&run_id),
        RunsCommand::Env { run_id } => handlers::env(&run_id),
        RunsCommand::Artifacts { run_id } => handlers::artifacts(&run_id),
        RunsCommand::Artifact(args) => handlers::artifact_command(args),
        RunsCommand::Findings(args) => findings::findings(args),
        RunsCommand::Finding { finding_id } => findings::finding(&finding_id),
        RunsCommand::LatestFinding(args) => findings::latest_finding(args),
        RunsCommand::Export(args) => super::bundle::export_runs(args),
        RunsCommand::Import(args) => super::bundle::import_runs(args),
        RunsCommand::Query(args) => query::runs_query(args),
        RunsCommand::Refs(args) => refs::runs_refs(args),
        RunsCommand::Drift(args) => drift::runs_drift(args),
        RunsCommand::LoopSync(args) => loop_sync::loop_sync(args),
    }
}

pub fn global_runner_error(args: &RunsArgs, runner_id: &str) -> Error {
    let (message, hints) = args.global_runner_guidance(runner_id);
    Error::validation_invalid_argument("runner", message, Some(runner_id.to_string()), Some(hints))
}

pub fn run_markdown(args: RunsArgs, _global: &GlobalArgs) -> CmdResult<String> {
    match args.command {
        RunsCommand::Compare(args) => compare::run_markdown(args),
        _ => Err(Error::validation_invalid_argument(
            "output_mode",
            "Only `homeboy runs compare --format=table` supports table output",
            None,
            None,
        )),
    }
}
