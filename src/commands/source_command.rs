use homeboy::core::ci_profile::{self, CiResolvedJob};
use homeboy::core::component::Component;
use homeboy::core::engine::execution_context::{self, ExecutionContext, ResolveOptions};
use homeboy::core::engine::resource::ResourceSummaryRun;
use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::ExtensionCapability;

use super::utils::args::{ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs};

pub(crate) fn resolve_source_context(
    comp: &PositionalComponentArgs,
    settings: &SettingArgs,
    extension_override: &ExtensionOverrideArgs,
    capability: Option<ExtensionCapability>,
) -> homeboy::core::Result<ExecutionContext> {
    execution_context::resolve(&ResolveOptions {
        component_id: comp.component.clone(),
        path_override: comp.path.clone(),
        capability,
        settings_overrides: settings.setting.clone(),
        settings_json_overrides: settings.setting_json.clone(),
        extension_overrides: extension_override.extensions.clone(),
    })
}

pub(crate) fn resolve_ci_job_for_command(
    job_id: Option<&str>,
    component: &Component,
    command: &'static str,
) -> homeboy::core::Result<Option<CiResolvedJob>> {
    let Some(job_id) = job_id else {
        return Ok(None);
    };
    let extension_ids = component_extension_ids(component);
    let extension_id = ci_profile::select_extension_id(&extension_ids)?;
    let job = ci_profile::resolve_job_for_extension(&extension_id, job_id)?;
    ci_profile::validate_job_command(&job, command)?;
    Ok(Some(job))
}

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

fn component_extension_ids(component: &Component) -> Vec<String> {
    let mut ids: Vec<String> = component
        .extensions
        .as_ref()
        .map(|extensions| extensions.keys().cloned().collect())
        .unwrap_or_default();
    ids.sort();
    ids
}

#[cfg(test)]
mod tests {
    use super::{component_extension_ids, ObservedWorkflowRunner};
    use homeboy::core::component::{Component, ScopedExtensionConfig};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    #[test]
    fn component_extension_ids_are_sorted() {
        let mut extensions = HashMap::new();
        extensions.insert("wordpress".to_string(), ScopedExtensionConfig::default());
        extensions.insert("nodejs".to_string(), ScopedExtensionConfig::default());
        let mut component = Component::new(
            "demo".to_string(),
            "/tmp/demo".to_string(),
            String::new(),
            None,
        );
        component.extensions = Some(extensions);

        assert_eq!(
            component_extension_ids(&component),
            vec!["nodejs", "wordpress"]
        );
    }

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
