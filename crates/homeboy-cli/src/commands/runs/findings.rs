use clap::Args;
use serde::Serialize;

use homeboy::core::observation::{
    runs_service, FindingListFilter, ObservationStore, RecordedHomeboyFinding,
};
use homeboy::core::Error;

use crate::commands::{
    issues,
    runs::{
        latest::latest_run_context, latest::RunsLatestFindingOutput, latest::RunsLatestRunArgs,
        run_summary, RunsOutput,
    },
    CmdResult,
};

#[derive(Args, Clone, Default)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct RunsFindingsArgs {
    #[command(subcommand)]
    pub command: Option<issues::IssuesCommand>,

    /// Observation run ID
    pub run_id: Option<String>,
    /// Finding tool, for example lint
    #[arg(long)]
    pub tool: Option<String>,
    /// Finding file path
    #[arg(long)]
    pub file: Option<String>,
    /// Finding fingerprint
    #[arg(long)]
    pub fingerprint: Option<String>,
    /// Maximum findings to return
    #[arg(long, default_value_t = 100)]
    pub limit: i64,
}

#[derive(Args, Clone, Default)]
pub struct RunsLatestFindingArgs {
    /// Run kind: bench, rig, trace, etc.
    #[arg(long)]
    pub kind: Option<String>,
    /// Component ID
    #[arg(long = "component")]
    pub component_id: Option<String>,
    /// Rig ID
    #[arg(long)]
    pub rig: Option<String>,
    /// Run status
    #[arg(long)]
    pub status: Option<String>,
    /// Finding tool, for example lint
    #[arg(long)]
    pub tool: Option<String>,
    /// Finding file path
    #[arg(long)]
    pub file: Option<String>,
}

#[derive(Serialize)]
pub struct RunsFindingsOutput {
    pub command: &'static str,
    pub run_id: String,
    pub findings: Vec<RecordedHomeboyFinding>,
}

#[derive(Serialize)]
pub struct RunsFindingOutput {
    pub command: &'static str,
    pub finding: RecordedHomeboyFinding,
}

pub fn findings(store: &ObservationStore, args: RunsFindingsArgs) -> CmdResult<RunsOutput> {
    if let Some(command) = args.command {
        let (output, exit_code) = issues::run(issues::IssuesArgs { command })?;
        return Ok((RunsOutput::FindingsReconcile(output), exit_code));
    }

    let run_id = args
        .run_id
        .ok_or_else(|| Error::validation_missing_argument(vec!["run_id".to_string()]))?;
    // Route run lookup through the observation facade so `runs findings` shares
    // the one activity-aware surface: durable label aliases, Lab mirror
    // resolution, and the stable missing-run error with runner guidance (#6768).
    // Filter on the resolved record id — the facade accepts labels, which are
    // not themselves finding `run_id` values.
    let run_id = runs_service::require_run(&store, &run_id)?.id;
    let findings = store
        .list_findings(FindingListFilter {
            run_id: Some(run_id.clone()),
            tool: args.tool,
            file: args.file,
            fingerprint: args.fingerprint,
            limit: Some(args.limit),
        })?
        .into_iter()
        .map(RecordedHomeboyFinding::from)
        .collect();

    Ok((
        RunsOutput::Findings(RunsFindingsOutput {
            command: "runs.findings",
            run_id,
            findings,
        }),
        0,
    ))
}

pub fn finding(store: &ObservationStore, finding_id: &str) -> CmdResult<RunsOutput> {
    let finding = store.get_finding(finding_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "finding_id",
            format!("finding not found: {finding_id}"),
            Some(finding_id.to_string()),
            None,
        )
    })?;

    Ok((
        RunsOutput::Finding(RunsFindingOutput {
            command: "runs.finding",
            finding: RecordedHomeboyFinding::from(finding),
        }),
        0,
    ))
}

pub fn latest_finding(args: RunsLatestFindingArgs) -> CmdResult<RunsOutput> {
    let (store, run) = latest_run_context(RunsLatestRunArgs {
        kind: args.kind,
        component_id: args.component_id,
        rig: args.rig,
        status: args.status,
    })?;
    let finding = store
        .latest_finding(FindingListFilter {
            run_id: Some(run.id.clone()),
            tool: args.tool,
            file: args.file,
            fingerprint: None,
            limit: Some(1),
        })?
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "filter",
                format!(
                    "no finding matched the provided filters in latest run {}",
                    run.id
                ),
                Some(run.id.clone()),
                None,
            )
        })?;

    Ok((
        RunsOutput::LatestFinding(RunsLatestFindingOutput {
            command: "runs.latest-finding",
            run: run_summary(run),
            finding: RecordedHomeboyFinding::from(finding),
        }),
        0,
    ))
}

#[cfg(test)]
mod tests {

    /// The observation store the enclosing isolated home installs.
    ///
    /// A test is the entry point for its own unit of work, so opening once here
    /// is a boundary open, not an ambient one inside production code (#7505).
    fn test_store() -> homeboy::core::observation::ObservationStore {
        homeboy::core::observation::ObservationStore::open_initialized().expect("observation store")
    }
    use homeboy::core::observation::{NewFindingRecord, NewRunRecord, ObservationStore};
    use homeboy::test_support::with_isolated_home;
    use serde_json::json;

    use super::*;

    struct XdgGuard(Option<String>);

    impl XdgGuard {
        fn unset() -> Self {
            let prior = std::env::var("XDG_DATA_HOME").ok();
            std::env::remove_var("XDG_DATA_HOME");
            Self(prior)
        }
    }

    impl Drop for XdgGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    fn labeled_run(store: &ObservationStore, label: &str) -> String {
        store
            .start_run(
                NewRunRecord::builder("bench")
                    .component_id("homeboy")
                    .command(format!("homeboy bench homeboy --run-id {label}"))
                    .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
                    .homeboy_version("test-version")
                    .rig_id("studio")
                    .metadata(json!({ "requested_run_id": label }))
                    .build(),
            )
            .expect("run")
            .id
    }

    fn record_finding(store: &ObservationStore, run_id: &str) {
        store
            .record_finding(&NewFindingRecord {
                run_id: run_id.to_string(),
                tool: "lint".to_string(),
                rule: Some("style".to_string()),
                file: Some("src/lib.rs".to_string()),
                line: Some(7),
                severity: Some("error".to_string()),
                fingerprint: Some("lint::style".to_string()),
                message: "style violation".to_string(),
                fixable: None,
                metadata_json: json!({}),
            })
            .expect("finding");
    }

    #[test]
    fn findings_resolve_a_durable_run_label_and_report_the_record_id() {
        // #6768: this command used to carry a private `require_run` that only
        // matched record ids, and then filtered findings on the *caller's*
        // string. Routing through `runs_service::require_run` resolves the
        // alias, and filtering on the resolved id is what makes the lookup
        // return the run's findings instead of an empty list.
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run_id = labeled_run(&store, "findings-label");
            record_finding(&store, &run_id);

            let (output, exit) = findings(
                &test_store(),
                RunsFindingsArgs {
                    run_id: Some("findings-label".to_string()),
                    limit: 100,
                    ..Default::default()
                },
            )
            .expect("durable run label resolves through runs_service::require_run");

            assert_eq!(exit, 0);
            let RunsOutput::Findings(output) = output else {
                panic!("expected findings output");
            };
            assert_eq!(output.run_id, run_id);
            assert_eq!(output.findings.len(), 1);
        });
    }

    #[test]
    fn findings_still_resolve_an_exact_record_id() {
        // Behavior preservation: the facade tries the exact record id first, so
        // the pre-existing contract is unchanged.
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run_id = labeled_run(&store, "findings-exact");
            record_finding(&store, &run_id);

            let (output, _) = findings(
                &test_store(),
                RunsFindingsArgs {
                    run_id: Some(run_id.clone()),
                    limit: 100,
                    ..Default::default()
                },
            )
            .expect("record id resolves");

            let RunsOutput::Findings(output) = output else {
                panic!("expected findings output");
            };
            assert_eq!(output.run_id, run_id);
            assert_eq!(output.findings.len(), 1);
        });
    }

    #[test]
    fn findings_report_the_facade_missing_run_error() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let _store = ObservationStore::open_initialized().expect("store");
            // `RunsOutput` is not `Debug`, so match rather than `expect_err`.
            let error = match findings(
                &test_store(),
                RunsFindingsArgs {
                    run_id: Some("definitely-missing-run".to_string()),
                    limit: 100,
                    ..Default::default()
                },
            ) {
                Ok(_) => panic!("expected a missing-run error"),
                Err(error) => error,
            };
            assert!(
                error.message.contains("run record not found"),
                "unexpected message: {}",
                error.message
            );
        });
    }
}
