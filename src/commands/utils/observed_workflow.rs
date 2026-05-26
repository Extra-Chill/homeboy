use homeboy::core::engine::resource::ResourceSummaryRun;
use homeboy::core::engine::run_dir::RunDir;

pub(crate) fn finish_observed_workflow<O, T, F, E>(
    observation: Option<O>,
    workflow: homeboy::core::Result<T>,
    finish_success: F,
    finish_error: E,
) -> homeboy::core::Result<T>
where
    F: FnOnce(O, &T),
    E: FnOnce(O, &homeboy::core::Error),
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

pub(crate) struct ObservedWorkflowRunner {
    run_dir: RunDir,
    resource_run: ResourceSummaryRun,
}

impl ObservedWorkflowRunner {
    pub(crate) fn create(resource_label: impl Into<String>) -> homeboy::core::Result<Self> {
        Ok(Self {
            run_dir: RunDir::create()?,
            resource_run: ResourceSummaryRun::start(Some(resource_label.into())),
        })
    }

    pub(crate) fn run_dir(&self) -> &RunDir {
        &self.run_dir
    }

    pub(crate) fn finish<O, T, F, E>(
        self,
        observation: Option<O>,
        workflow: homeboy::core::Result<T>,
        finish_success: F,
        finish_error: E,
    ) -> homeboy::core::Result<T>
    where
        F: FnOnce(O, &T),
        E: FnOnce(O, &homeboy::core::Error),
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
}

#[cfg(test)]
mod tests {
    use super::ObservedWorkflowRunner;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn observed_workflow_runner_finishes_success_after_resource_summary() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let runner = ObservedWorkflowRunner::create("test demo").expect("runner");
        let resource_summary_path = runner.run_dir().step_file("resource-summary.json");

        let result = runner.finish(
            Some(events.clone()),
            Ok::<_, homeboy::core::Error>(7),
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
        let error = homeboy::core::Error::validation_invalid_argument(
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
}
