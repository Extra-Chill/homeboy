use homeboy::core::observation::{NewRunRecord, ObservationStore, RunStatus};
use homeboy::test_support::with_isolated_home;
use serde_json::Value;

use super::{list_runs, RunsListArgs, RunsOutput};

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

fn sample_run(kind: &str, component_id: &str, rig_id: &str, metadata: Value) -> NewRunRecord {
    NewRunRecord::builder(kind)
        .component_id(component_id)
        .command(format!("homeboy {kind} {component_id}"))
        .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
        .homeboy_version("test-version")
        .git_sha(Some("abc123".to_string()))
        .rig_id(rig_id)
        .metadata(metadata)
        .build()
}

#[test]
fn rig_runs_list_surfaces_compact_artifact_index() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run(
                "rig",
                "homeboy",
                "proof-rig",
                serde_json::json!({
                    "pipeline": {
                        "name": "check",
                        "steps": [{
                            "kind": "command",
                            "label": "produce proof report",
                            "status": "fail",
                            "error": "report failed"
                        }],
                        "passed": 0,
                        "failed": 1
                    }
                }),
            ))
            .expect("rig run");
        store
            .finish_run(&run.id, RunStatus::Fail, None)
            .expect("finish rig run");
        let report = home.path().join("proof-report.json");
        std::fs::write(&report, "{}").expect("report artifact");
        store
            .record_artifact(&run.id, "proof_report", &report)
            .expect("record report");

        let (output, _) = list_runs(
            RunsListArgs {
                runner: None,
                kind: None,
                component_id: None,
                rig: Some("proof-rig".to_string()),
                status: None,
                limit: 20,
                include_active_runner_jobs: false,
            },
            "rig.runs",
        )
        .expect("rig runs");

        let RunsOutput::List(output) = output else {
            panic!("expected list output");
        };
        assert_eq!(output.command, "rig.runs");
        assert_eq!(output.runs.len(), 1);
        let artifact_index = output.runs[0]
            .artifact_index
            .as_ref()
            .expect("artifact index");
        assert_eq!(artifact_index.run_id, run.id);
        assert_eq!(artifact_index.rig_id, "proof-rig");
        assert_eq!(artifact_index.status, "fail");
        assert!(artifact_index
            .artifact_index_path
            .ends_with("rig-artifact-index.json"));
        assert_eq!(
            artifact_index.evidence_commands.artifacts_command,
            format!("homeboy runs artifacts {}", run.id)
        );
        assert!(artifact_index
            .key_report_refs
            .iter()
            .any(|artifact| artifact.kind == "proof_report"));
        assert_eq!(artifact_index.failed_step_refs.len(), 1);
        assert_eq!(
            artifact_index.failed_step_refs[0].label,
            "produce proof report"
        );
    });
}
