use crate::core::engine::resource::ResourceSummaryRun;
use crate::core::engine::run_dir::RunDir;
use crate::core::observation::{
    merge_metadata, ActiveObservation, NewFindingRecord, NewRunRecord, RunStatus,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationPersistenceWarning {
    pub operation: String,
    pub message: String,
}

impl ObservationPersistenceWarning {
    fn new(operation: impl Into<String>, error: impl std::fmt::Display) -> Self {
        Self {
            operation: operation.into(),
            message: error.to_string(),
        }
    }

    fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "operation": self.operation,
            "message": self.message,
        })
    }
}

pub trait WorkflowObservationAdapter<T> {
    fn start_record(&self) -> NewRunRecord;

    fn success_status(&self, workflow: &T) -> RunStatus;

    fn success_metadata(&self, workflow: &T) -> serde_json::Value;

    fn success_findings(&self, _run_id: &str, _workflow: &T) -> Vec<NewFindingRecord> {
        Vec::new()
    }

    fn error_metadata(&self, _error: &crate::core::Error) -> Option<serde_json::Value> {
        None
    }
}

pub fn finish_adapted_observed_workflow<T, A>(
    adapter: A,
    workflow: crate::core::Result<T>,
) -> crate::core::Result<T>
where
    A: WorkflowObservationAdapter<T>,
{
    let mut observation = BestEffortObservedRun::start(adapter.start_record());
    match workflow {
        Ok(workflow) => {
            if let Some(run_id) = observation.run_id().map(str::to_string) {
                let findings = adapter.success_findings(&run_id, &workflow);
                observation.record_findings(&findings);
            }
            observation.finish_with_merged_metadata(
                adapter.success_status(&workflow),
                adapter.success_metadata(&workflow),
            );
            Ok(workflow)
        }
        Err(error) => {
            observation.finish_error_with_merged_metadata(adapter.error_metadata(&error));
            Err(error)
        }
    }
}

struct BestEffortObservedRun {
    observation: Option<ActiveObservation>,
    warnings: Vec<ObservationPersistenceWarning>,
}

impl BestEffortObservedRun {
    fn start(record: NewRunRecord) -> Self {
        match ActiveObservation::start(record) {
            Ok(observation) => Self {
                observation: Some(observation),
                warnings: Vec::new(),
            },
            Err(error) => {
                warn_observation_persistence(&ObservationPersistenceWarning::new("start", error));
                Self {
                    observation: None,
                    warnings: vec![ObservationPersistenceWarning::new(
                        "start",
                        "observation start failed; run was not persisted",
                    )],
                }
            }
        }
    }

    fn run_id(&self) -> Option<&str> {
        self.observation.as_ref().map(ActiveObservation::run_id)
    }

    fn record_findings(&mut self, records: &[NewFindingRecord]) {
        if records.is_empty() {
            return;
        }
        let Some(observation) = &self.observation else {
            return;
        };
        if let Err(error) = observation.store().record_findings(records) {
            self.warn("record_findings", error);
        }
    }

    fn finish_with_merged_metadata(&mut self, status: RunStatus, metadata: serde_json::Value) {
        let Some(observation) = &self.observation else {
            return;
        };
        let metadata =
            self.metadata_with_warnings(observation.initial_metadata().clone(), metadata);
        if let Err(error) =
            observation
                .store()
                .finish_run(observation.run_id(), status, Some(metadata))
        {
            self.warn("finish_run", error);
        }
    }

    fn finish_error_with_merged_metadata(&mut self, metadata: Option<serde_json::Value>) {
        self.finish_with_merged_metadata(
            RunStatus::Error,
            metadata.unwrap_or_else(|| serde_json::json!({ "observation_status": "error" })),
        );
    }

    fn metadata_with_warnings(
        &self,
        initial: serde_json::Value,
        finish: serde_json::Value,
    ) -> serde_json::Value {
        let mut metadata = merge_metadata(initial, finish);
        if !self.warnings.is_empty() {
            metadata["observation_warnings"] = serde_json::Value::Array(
                self.warnings
                    .iter()
                    .map(ObservationPersistenceWarning::as_json)
                    .collect(),
            );
        }
        metadata
    }

    fn warn(&mut self, operation: &'static str, error: crate::core::Error) {
        let warning = ObservationPersistenceWarning::new(operation, error);
        warn_observation_persistence(&warning);
        self.warnings.push(warning);
    }
}

fn warn_observation_persistence(warning: &ObservationPersistenceWarning) {
    crate::log_status!(
        "observe",
        "warning: observation {} persistence failed: {}",
        warning.operation,
        warning.message
    );
}

pub fn finish_observed_workflow<O, T, F, E>(
    observation: Option<O>,
    workflow: crate::core::Result<T>,
    finish_success: F,
    finish_error: E,
) -> crate::core::Result<T>
where
    F: FnOnce(O, &T),
    E: FnOnce(O, &crate::core::Error),
{
    match workflow {
        Ok(workflow) => {
            if let Some(observation) = observation {
                finish_success(observation, &workflow);
            }
            Ok(workflow)
        }
        Err(error) => {
            if let Some(observation) = observation {
                finish_error(observation, &error);
            }
            Err(error)
        }
    }
}

pub struct ObservedWorkflowRunner {
    run_dir: RunDir,
    resource_run: ResourceSummaryRun,
}

impl ObservedWorkflowRunner {
    pub fn create(resource_label: impl Into<String>) -> crate::core::Result<Self> {
        Ok(Self {
            run_dir: RunDir::create()?,
            resource_run: ResourceSummaryRun::start(Some(resource_label.into())),
        })
    }

    pub fn run_dir(&self) -> &RunDir {
        &self.run_dir
    }

    pub fn finish<O, T, F, E>(
        self,
        observation: Option<O>,
        workflow: crate::core::Result<T>,
        finish_success: F,
        finish_error: E,
    ) -> crate::core::Result<T>
    where
        F: FnOnce(O, &T),
        E: FnOnce(O, &crate::core::Error),
    {
        match self.resource_run.write_to_run_dir(&self.run_dir) {
            Ok(_) => finish_observed_workflow(observation, workflow, finish_success, finish_error),
            Err(error) => {
                if let Some(observation) = observation {
                    finish_error(observation, &error);
                }
                Err(error)
            }
        }
    }

    pub fn finish_adapted<T, A>(
        self,
        adapter: A,
        workflow: crate::core::Result<T>,
    ) -> crate::core::Result<T>
    where
        A: WorkflowObservationAdapter<T>,
    {
        match self.resource_run.write_to_run_dir(&self.run_dir) {
            Ok(_) => finish_adapted_observed_workflow(adapter, workflow),
            Err(error) => {
                let mut observation = BestEffortObservedRun::start(adapter.start_record());
                observation.finish_error_with_merged_metadata(Some(serde_json::json!({
                    "observation_status": "error",
                    "error": error.to_string(),
                })));
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        finish_adapted_observed_workflow, ObservedWorkflowRunner, WorkflowObservationAdapter,
    };
    use crate::core::observation::{
        NewFindingRecord, NewRunRecord, ObservationStore, RunListFilter, RunStatus,
    };
    use crate::test_support::with_isolated_home;
    use std::cell::RefCell;
    use std::path::Path;
    use std::rc::Rc;

    struct FixtureAdapter {
        bad_finding: bool,
    }

    impl WorkflowObservationAdapter<i32> for FixtureAdapter {
        fn start_record(&self) -> NewRunRecord {
            NewRunRecord::builder("fixture")
                .component_id("homeboy")
                .command("homeboy fixture")
                .cwd_path(Path::new("/tmp/homeboy"))
                .current_homeboy_version()
                .metadata(serde_json::json!({ "phase": "initial" }))
                .build()
        }

        fn success_status(&self, workflow: &i32) -> RunStatus {
            if *workflow == 0 {
                RunStatus::Pass
            } else {
                RunStatus::Fail
            }
        }

        fn success_metadata(&self, workflow: &i32) -> serde_json::Value {
            serde_json::json!({
                "exit_code": workflow,
                "observation_status": if *workflow == 0 { "pass" } else { "fail" },
            })
        }

        fn success_findings(&self, run_id: &str, _workflow: &i32) -> Vec<NewFindingRecord> {
            vec![NewFindingRecord {
                run_id: if self.bad_finding {
                    "missing-run".to_string()
                } else {
                    run_id.to_string()
                },
                tool: "fixture".to_string(),
                rule: Some("rule".to_string()),
                file: Some("src/lib.rs".to_string()),
                line: Some(1),
                severity: Some("warning".to_string()),
                fingerprint: Some("fingerprint".to_string()),
                message: "fixture finding".to_string(),
                fixable: Some(false),
                metadata_json: serde_json::json!({}),
            }]
        }

        fn error_metadata(&self, error: &crate::core::Error) -> Option<serde_json::Value> {
            Some(serde_json::json!({
                "observation_status": "error",
                "error": error.to_string(),
            }))
        }
    }

    fn latest_fixture_run() -> crate::core::observation::RunRecord {
        ObservationStore::open_initialized()
            .expect("store")
            .latest_run(RunListFilter {
                kind: Some("fixture".to_string()),
                component_id: Some("homeboy".to_string()),
                ..RunListFilter::default()
            })
            .expect("latest run")
            .expect("fixture run")
    }

    #[test]
    fn observed_workflow_runner_finishes_success_after_resource_summary() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let runner = ObservedWorkflowRunner::create("test demo").expect("runner");
        let resource_summary_path = runner.run_dir().step_file("resource-summary.json");

        let result = runner.finish(
            Some(events.clone()),
            Ok::<_, crate::core::Error>(7),
            |events, workflow| events.borrow_mut().push(format!("success:{workflow}")),
            |events, error| events.borrow_mut().push(format!("error:{error}")),
        );

        assert_eq!(result.expect("workflow success"), 7);
        assert!(resource_summary_path.is_file());
        assert_eq!(events.borrow().as_slice(), ["success:7"]);
    }

    #[test]
    fn observed_workflow_runner_finishes_error_after_resource_summary() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let runner = ObservedWorkflowRunner::create("test demo").expect("runner");
        let resource_summary_path = runner.run_dir().step_file("resource-summary.json");
        let error = crate::core::Error::validation_invalid_argument(
            "fixture",
            "simulated workflow error",
            None,
            None,
        );

        let result = runner.finish(
            Some(events.clone()),
            Err::<i32, _>(error),
            |events, workflow| events.borrow_mut().push(format!("success:{workflow}")),
            |events, error| events.borrow_mut().push(format!("error:{error}")),
        );

        assert!(result.is_err());
        assert!(resource_summary_path.is_file());
        assert_eq!(events.borrow().len(), 1);
        assert!(events.borrow()[0].contains("simulated workflow error"));
    }

    #[test]
    fn adapted_observed_workflow_finishes_success_and_records_findings() {
        with_isolated_home(|_| {
            let result = finish_adapted_observed_workflow(
                FixtureAdapter { bad_finding: false },
                Ok::<_, crate::core::Error>(0),
            );

            assert_eq!(result.expect("workflow"), 0);
            let run = latest_fixture_run();
            assert_eq!(run.status, "pass");
            assert_eq!(run.metadata_json["phase"], "initial");
            assert_eq!(run.metadata_json["observation_status"], "pass");

            let findings = ObservationStore::open_initialized()
                .expect("store")
                .list_findings(crate::core::observation::FindingListFilter {
                    run_id: Some(run.id),
                    tool: Some("fixture".to_string()),
                    ..crate::core::observation::FindingListFilter::default()
                })
                .expect("findings");
            assert_eq!(findings.len(), 1);
        });
    }

    #[test]
    fn adapted_observed_workflow_finishes_errors() {
        with_isolated_home(|_| {
            let error = crate::core::Error::validation_invalid_argument(
                "fixture",
                "simulated workflow error",
                None,
                None,
            );

            let result = finish_adapted_observed_workflow(
                FixtureAdapter { bad_finding: false },
                Err::<i32, _>(error),
            );

            assert!(result.is_err());
            let run = latest_fixture_run();
            assert_eq!(run.status, "error");
            assert_eq!(run.metadata_json["phase"], "initial");
            assert_eq!(run.metadata_json["observation_status"], "error");
            assert!(run.metadata_json["error"]
                .as_str()
                .expect("error metadata")
                .contains("simulated workflow error"));
        });
    }

    #[test]
    fn adapted_observed_workflow_surfaces_persistence_warnings_in_metadata() {
        with_isolated_home(|_| {
            finish_adapted_observed_workflow(
                FixtureAdapter { bad_finding: true },
                Ok::<_, crate::core::Error>(1),
            )
            .expect("workflow still succeeds");

            let run = latest_fixture_run();
            assert_eq!(run.status, "fail");
            assert_eq!(
                run.metadata_json["observation_warnings"][0]["operation"],
                "record_findings"
            );
            assert!(run.metadata_json["observation_warnings"][0]["message"]
                .as_str()
                .expect("warning message")
                .contains("referenced run record not found"));
        });
    }
}
