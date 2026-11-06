//! Top-level `runs` command dispatch and `RunsArgs` inherent helpers.
//!
//! Routes parsed subcommands to their handlers and provides the global
//! `--runner` guidance surfaced when operators misuse the top-level flag.

use homeboy::core::observation::ObservationStore;
use homeboy::core::paths::PathRoots;
use homeboy::core::Error;

use super::types::{
    RunsArgs, RunsArtifactArgs, RunsArtifactCommand, RunsArtifactGetArgs, RunsArtifactsArgs,
    RunsCommand, RunsOutput,
};
use super::CmdResult;
use super::{
    compare, distribution, evidence, findings, handlers, hotspots, latest, reconcile, refs, report,
    resources, watch,
};

impl RunsArgs {
    pub(crate) fn logs_run_id(&self) -> Option<&str> {
        match &self.command {
            RunsCommand::Logs(args) => Some(&args.run_id),
            _ => None,
        }
    }

    /// Whether this is a `runs show <id>` invocation eligible for the
    /// compact human summary (i.e. the caller asked for neither `--json`
    /// nor `--format json`).
    pub(crate) fn show_summary_eligible(&self) -> bool {
        if self.workflow_reader_id().is_some() {
            return false;
        }
        match &self.command {
            RunsCommand::Show {
                json,
                presentation,
                field,
                ..
            } => field.is_empty() && !presentation.json_or_flag(*json),
            _ => false,
        }
    }

    /// Whether this is a `runs evidence <id> --dossier` invocation eligible for the
    /// compact human dossier (i.e. the caller asked for neither `--json`
    /// nor `--format json`).
    pub(crate) fn dossier_summary_eligible(&self) -> bool {
        match &self.command {
            RunsCommand::Evidence {
                dossier: true,
                json,
                presentation,
                field,
                ..
            } => field.is_empty() && !presentation.json_or_flag(*json),
            _ => false,
        }
    }

    /// Whether this is a `runs evidence <id> --proof` invocation eligible for the compact
    /// human summary (i.e. the caller asked for neither `--json` nor
    /// `--format json`).
    pub(crate) fn proof_summary_eligible(&self) -> bool {
        match &self.command {
            RunsCommand::Evidence {
                proof: true,
                json,
                presentation,
                field,
                ..
            } => field.is_empty() && !presentation.json_or_flag(*json),
            _ => false,
        }
    }

    pub(crate) fn absorb_global_runner_for_command_option(
        &mut self,
        runner: Option<String>,
    ) -> Option<String> {
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
            (RunsCommand::Artifacts(args), Some(runner_id)) if args.runner.is_none() => {
                args.runner = Some(runner_id);
                None
            }
            (RunsCommand::Artifacts(args), Some(runner_id))
                if args.runner.as_deref() == Some(runner_id.as_str()) =>
            {
                None
            }
            (
                RunsCommand::Artifact(RunsArtifactArgs {
                    command: RunsArtifactCommand::Get(args),
                }),
                Some(runner_id),
            ) if args.runner.is_none() => {
                args.runner = Some(runner_id);
                None
            }
            (
                RunsCommand::Artifact(RunsArtifactArgs {
                    command: RunsArtifactCommand::Get(args),
                }),
                Some(runner_id),
            ) if args.runner.as_deref() == Some(runner_id.as_str()) => None,
            (_, runner) => runner,
        }
    }

    pub(crate) fn is_artifacts(&self) -> bool {
        matches!(self.command, RunsCommand::Artifacts(_))
    }

    pub(crate) fn is_markdown_mode(&self) -> bool {
        matches!(self.command, RunsCommand::Compare(ref compare) if compare::is_table_mode(compare))
            || matches!(self.command, RunsCommand::Report(ref report_args) if report::is_markdown_mode(report_args))
    }

    pub(crate) fn is_bundle_export(&self) -> bool {
        matches!(self.command, RunsCommand::Export(_))
    }

    pub(crate) fn is_artifact_get(&self) -> bool {
        matches!(
            self.command,
            RunsCommand::Artifact(RunsArtifactArgs {
                command: RunsArtifactCommand::Get(_),
            })
        )
    }

    pub(crate) fn has_command_local_runner_option(&self) -> bool {
        matches!(
            self.command,
            RunsCommand::Artifacts(RunsArtifactsArgs {
                runner: Some(_),
                ..
            }) | RunsCommand::Artifact(RunsArtifactArgs {
                command: RunsArtifactCommand::Get(RunsArtifactGetArgs {
                    runner: Some(_),
                    ..
                }),
            }) | RunsCommand::Artifact(RunsArtifactArgs {
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
            | RunsCommand::Evidence { run_id, .. }
            | RunsCommand::Env { run_id } => (
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

pub fn run(args: RunsArgs) -> CmdResult<RunsOutput> {
    if args.workflow_reader_id().is_some() || args.workflow_discovery() {
        return super::workflow::run(args);
    }
    if let RunsCommand::List(list) = &args.command {
        super::cook::validate_discovery(list)?;
    }
    if args.cook_discovery() {
        let RunsCommand::List(list) = args.command else {
            unreachable!()
        };
        return super::cook::discover(list).map(|(value, code)| (RunsOutput::Cook(value), code));
    }
    // Durable events must not depend on opening the observation store.
    if let RunsCommand::Logs(args) = args.command {
        return super::logs(args).map(|(events, code)| (RunsOutput::Cook(events), code));
    }
    // Boundary: one `homeboy runs` invocation is one unit of work, so the
    // observation store is opened exactly once here and handed to the handlers
    // below. Each of those used to open its own, which made a single command
    // several independently-resolved stores (#7505).
    let roots = PathRoots::from_environment()?;
    let store = ObservationStore::open_initialized_in_roots(&roots)?;
    match args.command {
        RunsCommand::Logs(_) => unreachable!("durable logs dispatched before opening the store"),
        RunsCommand::List(args) => handlers::list_runs(&store, args, "runs.list"),
        RunsCommand::Distribution(args) => {
            distribution::runs_distribution(&store, args, "runs.distribution")
        }
        RunsCommand::LatestRun(args) => latest::latest_run(args),
        RunsCommand::Compare(args) => compare::compare_runs(&store, args),
        RunsCommand::Hotspots(args) => hotspots::runs_hotspots(&store, args),
        RunsCommand::Reconcile(args) => reconcile::reconcile_runs(&store, args),
        RunsCommand::Watch(args) => watch::watch_run(&store, args),
        RunsCommand::Cancel { run_id } => handlers::cancel_run(&store, &run_id),
        RunsCommand::Show {
            run_id,
            json: _,
            presentation: _,
            field,
        } => {
            let (output, exit_code) = handlers::show_run_in_store(&store, &run_id)?;
            if field.is_empty() {
                Ok((output, exit_code))
            } else {
                handlers::apply_field_selection(output, &field)
            }
        }
        RunsCommand::Evidence {
            run_id,
            full,
            kind,
            task,
            failure_only,
            field,
            proof,
            dossier,
            json: _,
            presentation: _,
        } => {
            let (output, exit_code) = if proof || dossier {
                evidence::project(&store, &run_id, full, proof, dossier)?
            } else {
                evidence::evidence_from_args(
                    &store,
                    super::cook::EvidenceArgs {
                        run_id,
                        full,
                        kind,
                        task,
                        failure_only,
                    },
                )?
            };
            if field.is_empty() {
                Ok((output, exit_code))
            } else {
                handlers::apply_field_selection(output, &field)
            }
        }
        RunsCommand::Env { run_id } => handlers::env(&store, &run_id),
        RunsCommand::Artifacts(args) => handlers::artifacts_from_args(&store, args),
        RunsCommand::Artifact(args) => handlers::artifact_command(&store, args),
        RunsCommand::Findings(args) => findings::findings(&store, args),
        RunsCommand::Export(args) => super::bundle::export_runs(&store, args),
        RunsCommand::Import(args) => super::bundle::import_runs(&store, args),
        RunsCommand::Refs(args) => refs::runs_refs(&store, args),
        RunsCommand::Resources(args) => {
            resources::runs_resources_in_store(&store, roots.config(), args)
        }
        RunsCommand::Report(args) => {
            report::run(args).map(|(output, exit_code)| (RunsOutput::Report(output), exit_code))
        }
    }
}

pub(crate) fn global_runner_error(args: &RunsArgs, runner_id: &str) -> Error {
    let (message, hints) = args.global_runner_guidance(runner_id);
    Error::validation_invalid_argument("runner", message, Some(runner_id.to_string()), Some(hints))
}

pub(crate) fn run_markdown(args: RunsArgs) -> CmdResult<String> {
    // Same boundary rule as `run`: one invocation, one store (#7505).
    let roots = PathRoots::from_environment()?;
    let store = ObservationStore::open_initialized_in_roots(&roots)?;
    match args.command {
        RunsCommand::Compare(args) => compare::run_markdown(&store, args),
        RunsCommand::Report(args) => report::run_markdown(args),
        _ => Err(Error::validation_invalid_argument(
            "output_mode",
            "Only `homeboy runs compare --format=table` supports table output",
            None,
            None,
        )),
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::RunsArgs;
    use clap::Parser;

    /// Minimal CLI wrapper so these tests exercise the real clap surface
    /// (including the flattened `PresentationArgs`) rather than a hand-built
    /// `RunsCommand` value.
    #[derive(Parser)]
    struct RunsCli {
        #[command(flatten)]
        runs: RunsArgs,
    }

    fn parse(args: &[&str]) -> RunsArgs {
        RunsCli::try_parse_from(args)
            .expect("runs args should parse")
            .runs
    }

    #[test]
    fn logs_preserves_cursor_and_canonical_payload() {
        let args = parse(&["runs", "logs", "run-1", "--cursor", "event-page-2"]);
        let super::RunsCommand::Logs(logs) = args.command else {
            panic!("expected logs");
        };
        assert_eq!(logs.run_id, "run-1");
        assert_eq!(logs.cursor.as_deref(), Some("event-page-2"));
        assert!(RunsCli::try_parse_from(["runs", "logs", "run-1", "--limit", "10"]).is_err());
        let page =
            serde_json::json!({"schema": "homeboy/control-plane-event-page/v1", "events": []});
        assert_eq!(
            serde_json::to_value(super::RunsOutput::Cook(page.clone())).unwrap(),
            page,
        );
    }

    #[test]
    fn bare_show_proof_and_dossier_are_summary_eligible() {
        assert!(parse(&["runs", "show", "run-1"]).show_summary_eligible());
        assert!(parse(&["runs", "evidence", "run-1", "--proof"]).proof_summary_eligible());
        assert!(parse(&["runs", "evidence", "run-1", "--dossier"]).dossier_summary_eligible());
    }

    #[test]
    fn legacy_json_flag_still_suppresses_the_compact_summary() {
        assert!(!parse(&["runs", "show", "run-1", "--json"]).show_summary_eligible());
        assert!(
            !parse(&["runs", "evidence", "run-1", "--proof", "--json"]).proof_summary_eligible()
        );
        assert!(
            !parse(&["runs", "evidence", "run-1", "--dossier", "--json"])
                .dossier_summary_eligible()
        );
    }

    #[test]
    fn format_json_suppresses_the_compact_summary_identically() {
        assert!(!parse(&["runs", "show", "run-1", "--format", "json"]).show_summary_eligible());
        assert!(
            !parse(&["runs", "evidence", "run-1", "--proof", "--format", "json"])
                .proof_summary_eligible()
        );
        assert!(
            !parse(&["runs", "evidence", "run-1", "--dossier", "--format", "json"])
                .dossier_summary_eligible()
        );
    }

    #[test]
    fn both_spellings_together_are_accepted() {
        assert!(
            !parse(&["runs", "show", "run-1", "--json", "--format=json"]).show_summary_eligible()
        );
    }

    #[test]
    fn evidence_projections_conflict_and_old_verbs_are_rejected() {
        let error = RunsCli::try_parse_from(["runs", "evidence", "run-1", "--proof", "--dossier"])
            .err()
            .expect("projections must conflict");
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        for projection in ["--proof", "--dossier"] {
            let error =
                RunsCli::try_parse_from(["runs", "evidence", "run-1", projection, "--full"])
                    .err()
                    .expect("--full applies only to plain evidence");
            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
        for verb in ["proof", "dossier"] {
            assert!(RunsCli::try_parse_from(["runs", verb, "run-1"]).is_err());
        }
        let evidence = parse(&["runs", "evidence", "run-1"]);
        assert!(!evidence.proof_summary_eligible());
        assert!(!evidence.dossier_summary_eligible());
    }

    #[test]
    fn report_subcommands_parse_under_runs() {
        let report = parse(&[
            "runs",
            "report",
            "failure-digest",
            "--output-dir",
            "artifacts",
            "--results",
            r#"{"audit":"pass"}"#,
        ]);
        assert!(report.is_markdown_mode());
    }

    #[test]
    fn non_json_formats_keep_the_compact_summary() {
        for format in ["auto", "markdown", "text"] {
            assert!(
                parse(&["runs", "show", "run-1", "--format", format]).show_summary_eligible(),
                "--format {format} must not suppress the compact summary"
            );
        }
    }

    #[test]
    fn detail_is_accepted_but_does_not_change_the_presentation() {
        // `runs show`/`proof`/`dossier` render their documented default at
        // both detail levels; the format axis is the only knob. Pinned so a
        // later migration has to change this test deliberately rather than
        // silently repurposing the flag.
        for detail in ["summary", "full"] {
            assert!(
                parse(&["runs", "show", "run-1", "--detail", detail]).show_summary_eligible(),
                "--detail {detail} must not suppress the compact summary"
            );
            assert!(
                !parse(&["runs", "show", "run-1", "--detail", detail, "--json"])
                    .show_summary_eligible(),
                "--detail {detail} must not resurrect the compact summary"
            );
        }
    }

    #[test]
    fn show_still_parses_field_selectors_alongside_presentation() {
        let args = parse(&["runs", "show", "run-1", "--format=json", "-q", "$.status"]);
        assert!(!args.show_summary_eligible());
    }

    #[test]
    fn eligibility_helpers_do_not_cross_subcommands() {
        let show = parse(&["runs", "show", "run-1"]);
        assert!(!show.proof_summary_eligible());
        assert!(!show.dossier_summary_eligible());
    }
}
