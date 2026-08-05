//! Durable admission for a detached Cook controller.

use crate::agent_task_lifecycle;
use homeboy_core::Result;

use super::cook::AgentTaskCookServiceOptions;
use super::cook_pre_execution::materialize_initial_cook_attempt;
use super::cook_recipe::persist_initial_recipe;

/// The addressable identity returned only after a detached Cook has a real
/// controller recipe, lifecycle record, and Cook index entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachedCookAdmission {
    pub cook_id: String,
    pub run_id: String,
}

/// Admit a fully compiled detached Cook before its process is spawned.
///
/// Callers compile and validate the request before calling this boundary. This
/// operation owns the durable suffix: recipe, attempt record, index, and
/// initial controller phase.
pub fn prepare_detached_cook(
    options: &AgentTaskCookServiceOptions,
) -> Result<DetachedCookAdmission> {
    persist_initial_recipe(options)?;
    materialize_initial_cook_attempt(options)?;
    agent_task_lifecycle::record_cook_progress(
        &options.initial_run_id,
        "detached_handoff_accepted",
        1,
        Some("durably admitted before detached controller startup"),
    )?;
    let record = agent_task_lifecycle::status(&options.initial_run_id)?;
    Ok(DetachedCookAdmission {
        cook_id: options.cook_id.clone(),
        run_id: record.run_id,
    })
}
