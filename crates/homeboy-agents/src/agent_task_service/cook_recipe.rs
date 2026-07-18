//! Durable, versioned input boundary for cook continuation scheduling.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;

use crate::agent_task_scheduler::AgentTaskPlan;
use crate::agent_task_service::cook::{
    AgentTaskCookAttemptDispatcher, AgentTaskCookServiceOptions,
};
use homeboy_core::command_invocation::CommandInvocation;
use homeboy_core::{paths, Error, Result};

pub const COOK_RECIPE_SCHEMA: &str = "homeboy/agent-task-cook-recipe/v1";
const CONTINUATION_SCHEMA: &str = "homeboy/agent-task-cook-continuation/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskCookRecipe {
    pub schema: String,
    pub cook_id: String,
    pub attempts: Vec<AgentTaskCookRecipeAttempt>,
    pub promotion_transport: Value,
    pub gate_policy: Value,
    pub retry_budget: Value,
    pub finalization: Value,
    pub source_refs: Vec<String>,
    pub runtime_generation: String,
    pub sensitive_mappings: Vec<String>,
    pub harvest_context: crate::agent_task_scheduler::HarvestExecutionContext,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskCookRecipeAttempt {
    pub attempt: u32,
    pub run_id: String,
    pub plan: AgentTaskPlan,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskCookContinuation {
    pub schema: String,
    pub key: String,
    pub cook_id: String,
    pub run_id: String,
    #[serde(default)]
    pub retries: u32,
}

#[derive(Debug)]
pub struct ClaimedCookContinuation {
    continuation: AgentTaskCookContinuation,
    path: PathBuf,
}

impl ClaimedCookContinuation {
    pub fn continuation(&self) -> &AgentTaskCookContinuation {
        &self.continuation
    }

    pub fn complete(self) -> Result<()> {
        fs::rename(&self.path, continuation_state_path(&self.path, "completed")).map_err(|error| {
            Error::internal_io(error.to_string(), Some(self.path.display().to_string()))
        })
    }

    pub fn retry(mut self) -> Result<()> {
        const MAX_CONTINUATION_RETRIES: u32 = 3;
        self.continuation.retries = self.continuation.retries.saturating_add(1);
        if self.continuation.retries > MAX_CONTINUATION_RETRIES {
            return self.fail("cook continuation retry budget exhausted");
        }
        fs::write(
            &self.path,
            serde_json::to_vec(&self.continuation)
                .map_err(|error| Error::internal_json(error.to_string(), None))?,
        )
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(self.path.display().to_string()))
        })?;
        fs::rename(&self.path, continuation_state_path(&self.path, "pending")).map_err(|error| {
            Error::internal_io(error.to_string(), Some(self.path.display().to_string()))
        })
    }

    pub fn fail(self, diagnostic: &str) -> Result<()> {
        fail_claimed_path(&self.path, diagnostic)
    }
}

/// The consumer boundary is deliberately injected: status/reconciliation only
/// writes durable signals and never invokes process-local closures.
pub trait AgentTaskCookContinuationScheduler {
    /// Returns true only when this call created the durable queue entry.
    fn enqueue(&self, continuation: &AgentTaskCookContinuation) -> Result<bool>;
}

pub fn persist_initial_recipe(
    options: &AgentTaskCookServiceOptions,
) -> Result<AgentTaskCookRecipe> {
    let attempt_dispatch = options
        .attempt_dispatcher
        .as_ref()
        .map(|dispatcher| dispatcher.durable_recipe())
        .transpose()?
        .unwrap_or_else(|| serde_json::json!({ "kind": "local" }));
    let recipe = AgentTaskCookRecipe {
        schema: COOK_RECIPE_SCHEMA.to_string(),
        cook_id: options.cook_id.clone(),
        attempts: vec![AgentTaskCookRecipeAttempt {
            attempt: 1,
            run_id: options.initial_run_id.clone(),
            plan: options.initial_plan.clone(),
        }],
        promotion_transport: serde_json::json!({
            "provider_command": options.provider_command,
            "provider_invocation": options.provider_invocation,
            "attempt_dispatch": attempt_dispatch,
        }),
        gate_policy: serde_json::to_value(&options.gates).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize cook gate policy".to_string()),
            )
        })?,
        retry_budget: serde_json::json!({ "max_attempts": options.max_attempts, "execution_budget": options.initial_plan.options.execution_budget }),
        finalization: serde_json::json!({
            "no_finalize": options.no_finalize,
            "base": options.base,
            "head": options.head,
            "title": options.title,
            "commit_message": options.commit_message,
            "protected_branches": options.protected_branches,
            "ai_tool": options.ai_tool,
            "ai_model": options.ai_model,
            "ai_used_for": options.ai_used_for,
            "to_worktree": options.to_worktree,
            "source_worktree_path": options.source_worktree_path,
            "task_base_sha": options.task_base_sha,
        }),
        source_refs: options.source_refs.clone(),
        runtime_generation: homeboy_core::build_identity::current().display,
        sensitive_mappings: sensitive_mappings(&options.initial_plan)?,
        harvest_context: options.harvest_context.clone(),
    };
    validate_recipe(&recipe)?;
    if recipe_exists(&recipe.cook_id)? {
        let existing = load_recipe(&recipe.cook_id)?;
        let mut expected = recipe.clone();
        expected.attempts = existing.attempts.clone();
        expected.sensitive_mappings = existing.sensitive_mappings.clone();
        if existing != expected || existing.attempts.first() != recipe.attempts.first() {
            return Err(Error::validation_invalid_argument(
                "cook_recipe",
                "durable cook recipe already exists with different execution inputs",
                Some(recipe.cook_id.clone()),
                None,
            ));
        }
        return Ok(existing);
    }
    write_recipe(&recipe)?;
    Ok(recipe)
}

pub fn record_recipe_attempt(
    cook_id: &str,
    attempt: u32,
    run_id: &str,
    plan: &AgentTaskPlan,
) -> Result<AgentTaskCookRecipe> {
    let mut recipe = load_recipe(cook_id)?;
    let candidate = AgentTaskCookRecipeAttempt {
        attempt,
        run_id: run_id.to_string(),
        plan: plan.clone(),
    };
    if let Some(existing) = recipe
        .attempts
        .iter()
        .find(|existing| existing.attempt == attempt || existing.run_id == run_id)
    {
        let existing_value = serde_json::to_value(existing)
            .map_err(|error| Error::internal_json(error.to_string(), None))?;
        let candidate_value = serde_json::to_value(&candidate)
            .map_err(|error| Error::internal_json(error.to_string(), None))?;
        if existing_value == candidate_value {
            return Ok(recipe);
        }
        return Err(Error::validation_invalid_argument(
            "cook_recipe.attempts",
            "durable cook attempt identity is already bound to different inputs",
            Some(run_id.to_string()),
            None,
        ));
    }
    if attempt != recipe.attempts.len() as u32 + 1 {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.attempts",
            "durable cook attempts must be appended in order",
            Some(run_id.to_string()),
            None,
        ));
    }
    recipe.attempts.push(candidate);
    recipe.sensitive_mappings = recipe
        .attempts
        .iter()
        .map(|attempt| sensitive_mappings(&attempt.plan))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    recipe.sensitive_mappings.sort();
    recipe.sensitive_mappings.dedup();
    validate_recipe(&recipe)?;
    write_recipe(&recipe)?;
    Ok(recipe)
}

pub fn load_recipe(cook_id: &str) -> Result<AgentTaskCookRecipe> {
    let path = recipe_path(cook_id)?;
    let raw = fs::read_to_string(&path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    let recipe = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_argument(
            "cook_recipe",
            format!("malformed durable cook recipe: {error}"),
            Some(cook_id.to_string()),
            None,
        )
    })?;
    validate_recipe(&recipe)?;
    Ok(recipe)
}

/// Legacy cook indexes predate this durable scheduler contract. Their status
/// projection remains read-only and preserves the previous orphan behavior.
pub fn recipe_exists(cook_id: &str) -> Result<bool> {
    Ok(recipe_path(cook_id)?.exists())
}

pub fn enqueue_terminal_continuation(cook_id: &str, run_id: &str) -> Result<bool> {
    let recipe = load_recipe(cook_id)?;
    if !recipe
        .attempts
        .iter()
        .any(|attempt| attempt.run_id == run_id)
    {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.attempts",
            "terminal run is not declared by the durable cook recipe",
            Some(run_id.to_string()),
            None,
        ));
    }
    let continuation = AgentTaskCookContinuation {
        schema: CONTINUATION_SCHEMA.to_string(),
        key: format!("{cook_id}:{run_id}"),
        cook_id: cook_id.to_string(),
        run_id: run_id.to_string(),
        retries: 0,
    };
    DurableCookContinuationQueue.enqueue(&continuation)
}

pub fn claim_continuation() -> Result<Option<ClaimedCookContinuation>> {
    let root = queue_root()?;
    fs::create_dir_all(&root)
        .map_err(|error| Error::internal_io(error.to_string(), Some(root.display().to_string())))?;
    reclaim_dead_claims(&root)?;
    for entry in fs::read_dir(&root)
        .map_err(|error| Error::internal_io(error.to_string(), Some(root.display().to_string())))?
    {
        let path = entry
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(root.display().to_string()))
            })?
            .path();
        if path.extension().and_then(|value| value.to_str()) != Some("pending") {
            continue;
        }
        let claimed = path.with_extension(format!("claimed.{}", std::process::id()));
        if fs::rename(&path, &claimed).is_err() {
            continue;
        }
        let raw = fs::read_to_string(&claimed).map_err(|error| {
            Error::internal_io(error.to_string(), Some(claimed.display().to_string()))
        })?;
        let continuation = match serde_json::from_str(&raw) {
            Ok(continuation) => continuation,
            Err(error) => {
                let error = Error::validation_invalid_argument(
                    "cook_continuation",
                    format!("malformed durable continuation: {error}"),
                    Some(claimed.display().to_string()),
                    None,
                );
                fail_claimed_path(&claimed, &error.message)?;
                return Err(error);
            }
        };
        if let Err(error) = validate_continuation(&continuation) {
            fail_claimed_path(&claimed, &error.message)?;
            return Err(error);
        }
        return Ok(Some(ClaimedCookContinuation {
            continuation,
            path: claimed,
        }));
    }
    Ok(None)
}

pub fn reconstruct_options(recipe: &AgentTaskCookRecipe) -> Result<AgentTaskCookServiceOptions> {
    reconstruct_options_with_dispatcher(recipe, None)
}

pub fn reconstruct_options_with_dispatcher(
    recipe: &AgentTaskCookRecipe,
    attempt_dispatcher: Option<Arc<dyn AgentTaskCookAttemptDispatcher>>,
) -> Result<AgentTaskCookServiceOptions> {
    validate_recipe(recipe)?;
    if recipe.runtime_generation != homeboy_core::build_identity::current().display {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.runtime_generation",
            format!(
                "cook recipe is pinned to Homeboy runtime `{}` but this process is `{}`",
                recipe.runtime_generation,
                homeboy_core::build_identity::current().display
            ),
            Some(recipe.cook_id.clone()),
            None,
        ));
    }
    let initial = recipe
        .attempts
        .first()
        .expect("validated recipe has attempt");
    let gates = serde_json::from_value(recipe.gate_policy.clone()).map_err(|error| {
        Error::validation_invalid_argument(
            "cook_recipe.gate_policy",
            format!("malformed gate policy: {error}"),
            None,
            None,
        )
    })?;
    let provider_command = recipe
        .promotion_transport
        .get("provider_command")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok());
    let provider_invocation = recipe
        .promotion_transport
        .get("provider_invocation")
        .filter(|value| !value.is_null())
        .cloned()
        .map(serde_json::from_value::<CommandInvocation>)
        .transpose()
        .map_err(|error| {
            Error::validation_invalid_argument(
                "cook_recipe.promotion_transport",
                format!("malformed provider invocation: {error}"),
                None,
                None,
            )
        })?;
    let field = |name: &str| {
        recipe.finalization.get(name).cloned().ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.finalization",
                format!("missing finalization field `{name}`"),
                None,
                None,
            )
        })
    };
    let dispatch_kind = recipe
        .promotion_transport
        .pointer("/attempt_dispatch/kind")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.promotion_transport.attempt_dispatch",
                "durable cook recipe is missing its attempt dispatcher kind",
                Some(recipe.cook_id.clone()),
                None,
            )
        })?;
    if dispatch_kind == "local" && attempt_dispatcher.is_some() {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.promotion_transport.attempt_dispatch",
            "local cook recipe cannot be reconstructed with an external dispatcher",
            Some(recipe.cook_id.clone()),
            None,
        ));
    }
    if dispatch_kind != "local" && attempt_dispatcher.is_none() {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.promotion_transport.attempt_dispatch",
            format!("cook recipe requires `{dispatch_kind}` attempt dispatcher reconstruction"),
            Some(recipe.cook_id.clone()),
            None,
        ));
    }
    Ok(AgentTaskCookServiceOptions {
        cook_id: recipe.cook_id.clone(),
        initial_run_id: initial.run_id.clone(),
        initial_plan: initial.plan.clone(),
        to_worktree: serde_json::from_value(field("to_worktree")?)
            .map_err(recipe_value_error("to_worktree"))?,
        source_worktree_path: serde_json::from_value(field("source_worktree_path")?)
            .map_err(recipe_value_error("source_worktree_path"))?,
        provider_command,
        provider_invocation,
        gates,
        max_attempts: recipe
            .retry_budget
            .get("max_attempts")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "cook_recipe.retry_budget",
                    "missing max_attempts",
                    None,
                    None,
                )
            })? as u32,
        no_finalize: serde_json::from_value(field("no_finalize")?)
            .map_err(recipe_value_error("no_finalize"))?,
        base: serde_json::from_value(field("base")?).map_err(recipe_value_error("base"))?,
        task_base_sha: serde_json::from_value(field("task_base_sha")?)
            .map_err(recipe_value_error("task_base_sha"))?,
        head: serde_json::from_value(field("head")?).map_err(recipe_value_error("head"))?,
        title: serde_json::from_value(field("title")?).map_err(recipe_value_error("title"))?,
        commit_message: serde_json::from_value(field("commit_message")?)
            .map_err(recipe_value_error("commit_message"))?,
        source_refs: recipe.source_refs.clone(),
        protected_branches: serde_json::from_value(field("protected_branches")?)
            .map_err(recipe_value_error("protected_branches"))?,
        ai_tool: serde_json::from_value(field("ai_tool")?)
            .map_err(recipe_value_error("ai_tool"))?,
        ai_model: serde_json::from_value(field("ai_model")?)
            .map_err(recipe_value_error("ai_model"))?,
        ai_used_for: serde_json::from_value(field("ai_used_for")?)
            .map_err(recipe_value_error("ai_used_for"))?,
        attempt_dispatcher,
        harvest_context: recipe.harvest_context.clone(),
    })
}

/// Consume one durable continuation through an injected normal cook boundary.
/// Production supplies `run_cook`; tests use side-effect-free recorders.
pub fn consume_next_with(
    execute: impl FnOnce(AgentTaskCookServiceOptions) -> Result<i32>,
) -> Result<Option<i32>> {
    let Some(claim) = claim_continuation()? else {
        return Ok(None);
    };
    consume_claimed_with(claim, execute).map(Some)
}

pub fn consume_claimed_with(
    claim: ClaimedCookContinuation,
    execute: impl FnOnce(AgentTaskCookServiceOptions) -> Result<i32>,
) -> Result<i32> {
    consume_claimed_with_dispatcher(claim, |_| Ok(None), execute)
}

pub fn consume_claimed_with_dispatcher(
    claim: ClaimedCookContinuation,
    dispatcher: impl FnOnce(&Value) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
    execute: impl FnOnce(AgentTaskCookServiceOptions) -> Result<i32>,
) -> Result<i32> {
    let recipe = match load_recipe(&claim.continuation().cook_id) {
        Ok(recipe) => recipe,
        Err(error) => {
            claim.fail(&error.message)?;
            return Err(error);
        }
    };
    let attempt_dispatcher = match dispatcher(&recipe.promotion_transport["attempt_dispatch"]) {
        Ok(dispatcher) => dispatcher,
        Err(error) => {
            claim.fail(&error.message)?;
            return Err(error);
        }
    };
    let options = match reconstruct_options_with_dispatcher(&recipe, attempt_dispatcher) {
        Ok(options) => options,
        Err(error) => {
            claim.fail(&error.message)?;
            return Err(error);
        }
    };
    match execute(options) {
        Ok(exit_code) => {
            claim.complete()?;
            Ok(exit_code)
        }
        Err(error) if error.retryable == Some(true) => {
            claim.retry()?;
            Err(error)
        }
        Err(error) => {
            claim.fail(&error.message)?;
            Err(error)
        }
    }
}

fn recipe_value_error(field: &'static str) -> impl FnOnce(serde_json::Error) -> Error {
    move |error| {
        Error::validation_invalid_argument(
            "cook_recipe.finalization",
            format!("malformed finalization field `{field}`: {error}"),
            None,
            None,
        )
    }
}

struct DurableCookContinuationQueue;
impl AgentTaskCookContinuationScheduler for DurableCookContinuationQueue {
    fn enqueue(&self, continuation: &AgentTaskCookContinuation) -> Result<bool> {
        validate_continuation(continuation)?;
        let root = queue_root()?;
        fs::create_dir_all(&root).map_err(|error| {
            Error::internal_io(error.to_string(), Some(root.display().to_string()))
        })?;
        let hash = format!("{:x}", sha2::Sha256::digest(continuation.key.as_bytes()));
        let path = root.join(format!("{hash}.pending"));
        if root.join(format!("{hash}.completed")).exists()
            || root.join(format!("{hash}.failed")).exists()
            || fs::read_dir(&root)
                .map_err(|error| {
                    Error::internal_io(error.to_string(), Some(root.display().to_string()))
                })?
                .filter_map(std::result::Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .any(|name| name.starts_with(&format!("{hash}.claimed.")))
        {
            return Ok(false);
        }
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(error) => {
                return Err(Error::internal_io(
                    error.to_string(),
                    Some(path.display().to_string()),
                ))
            }
        };
        file.write_all(
            &serde_json::to_vec(continuation)
                .map_err(|error| Error::internal_json(error.to_string(), None))?,
        )
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
        file.sync_all().map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
        Ok(true)
    }
}

fn validate_recipe(recipe: &AgentTaskCookRecipe) -> Result<()> {
    if recipe.schema != COOK_RECIPE_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.schema",
            format!(
                "unsupported cook recipe schema `{}`; supported schema is `{COOK_RECIPE_SCHEMA}`",
                recipe.schema
            ),
            Some(recipe.schema.clone()),
            None,
        ));
    }
    if recipe.cook_id.is_empty()
        || recipe.attempts.is_empty()
        || recipe.runtime_generation.is_empty()
    {
        return Err(Error::validation_invalid_argument("cook_recipe", "cook recipe requires cook_id, at least one exact attempt, and pinned runtime generation", None, None));
    }
    for attempt in &recipe.attempts {
        if attempt.run_id.is_empty() || attempt.plan.tasks.is_empty() {
            return Err(Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "each cook attempt requires an exact run id and compiled non-empty plan",
                Some(attempt.run_id.clone()),
                None,
            ));
        }
    }
    if recipe
        .sensitive_mappings
        .iter()
        .any(|mapping| mapping.trim().is_empty())
    {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.sensitive_mappings",
            "sensitive mappings must be explicit non-empty durable identifiers",
            None,
            None,
        ));
    }
    let declared = recipe
        .sensitive_mappings
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    let mut required_mappings = Vec::new();
    for attempt in &recipe.attempts {
        required_mappings.extend(sensitive_mappings(&attempt.plan)?);
    }
    let required = required_mappings
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    if declared != required {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.sensitive_mappings",
            "durable sensitive mappings do not exactly match the compiled attempt plans",
            None,
            None,
        ));
    }
    Ok(())
}

fn validate_continuation(continuation: &AgentTaskCookContinuation) -> Result<()> {
    if continuation.schema != CONTINUATION_SCHEMA
        || continuation.key.is_empty()
        || continuation.cook_id.is_empty()
        || continuation.run_id.is_empty()
    {
        return Err(Error::validation_invalid_argument(
            "cook_continuation",
            "unknown or malformed cook continuation; inspect the durable queue entry",
            None,
            None,
        ));
    }
    Ok(())
}

fn sensitive_mappings(plan: &AgentTaskPlan) -> Result<Vec<String>> {
    let mappings = plan
        .tasks
        .iter()
        .flat_map(|task| task.executor.secret_env.iter().cloned())
        .collect::<Vec<_>>();
    if mappings.iter().any(|mapping| mapping.trim().is_empty()) {
        return Err(Error::validation_invalid_argument(
            "executor.secret_env",
            "cook recipes require explicit non-empty sensitive mappings",
            None,
            None,
        ));
    }
    Ok(mappings)
}

fn write_recipe(recipe: &AgentTaskCookRecipe) -> Result<()> {
    let path = recipe_path(&recipe.cook_id)?;
    fs::create_dir_all(path.parent().expect("recipe path has parent"))
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    homeboy_core::engine::local_files::write_json_file_owner_only(&path, recipe)
}

fn recipe_path(cook_id: &str) -> Result<PathBuf> {
    Ok(paths::homeboy_data()?
        .join("agent-task-cooks")
        .join(paths::sanitize_path_segment(cook_id))
        .join("recipe.json"))
}
fn queue_root() -> Result<PathBuf> {
    Ok(paths::homeboy_data()?.join("agent-task-cook-continuations"))
}

fn continuation_base_path(path: &std::path::Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let base = name
        .split_once(".claimed.")
        .map(|(base, _)| base)
        .or_else(|| name.rsplit_once('.').map(|(base, _)| base))
        .unwrap_or(name);
    path.with_file_name(base)
}

fn continuation_state_path(path: &std::path::Path, state: &str) -> PathBuf {
    continuation_base_path(path).with_extension(state)
}

fn fail_claimed_path(path: &std::path::Path, diagnostic: &str) -> Result<()> {
    let failed = continuation_state_path(path, "failed");
    fs::rename(path, &failed)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    let diagnostic_path = continuation_state_path(path, "diagnostic");
    fs::write(&diagnostic_path, diagnostic).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(diagnostic_path.display().to_string()),
        )
    })
}

fn reclaim_dead_claims(root: &std::path::Path) -> Result<()> {
    for entry in fs::read_dir(root)
        .map_err(|error| Error::internal_io(error.to_string(), Some(root.display().to_string())))?
    {
        let path = entry
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(root.display().to_string()))
            })?
            .path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let Some(pid) = name
            .rsplit_once(".claimed.")
            .map(|(_, pid)| pid)
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        if !homeboy_core::process::pid_is_running(pid) {
            fs::rename(&path, continuation_state_path(&path, "pending")).map_err(|error| {
                Error::internal_io(error.to_string(), Some(path.display().to_string()))
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus,
        AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace,
    };
    use crate::agent_task_scheduler::{
        AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
        AgentTaskProgressEvent, AgentTaskState,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct ReconstructedDispatcher;

    impl AgentTaskCookAttemptDispatcher for ReconstructedDispatcher {
        fn durable_recipe(&self) -> Result<Value> {
            Ok(serde_json::json!({ "kind": "test" }))
        }

        fn dispatch_attempt(
            &self,
            _plan: AgentTaskPlan,
            _run_id: &str,
            _derived_cook_baseline: Option<
                &crate::agent_task_service::cook::DerivedCookBaselineCapability,
            >,
        ) -> Result<()> {
            Ok(())
        }
    }

    fn recipe() -> AgentTaskCookRecipe {
        let request = AgentTaskRequest {
            schema: crate::agent_task::AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "task".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: vec!["TEST_TOKEN".to_string()],
                model: None,
                config: Value::Null,
            },
            instructions: "test".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: Value::Null,
        };
        let plan = AgentTaskPlan::new("plan", vec![request]);
        AgentTaskCookRecipe {
            schema: COOK_RECIPE_SCHEMA.to_string(),
            cook_id: "cook".to_string(),
            attempts: vec![AgentTaskCookRecipeAttempt {
                attempt: 1,
                run_id: "run".to_string(),
                plan: plan.clone(),
            }],
            promotion_transport: serde_json::json!({"provider_command": null, "provider_invocation": null, "attempt_dispatch": { "kind": "local" }}),
            gate_policy: serde_json::json!({"verify": [], "private_verify": [], "private_gate_reveal": "summary_only"}),
            retry_budget: serde_json::json!({"max_attempts": 1, "execution_budget": plan.options.execution_budget}),
            finalization: serde_json::json!({"no_finalize": true, "base": "main", "head": null, "title": "title", "commit_message": "message", "protected_branches": [], "ai_tool": "test", "ai_model": null, "ai_used_for": "test", "to_worktree": "target", "source_worktree_path": null, "task_base_sha": null}),
            source_refs: vec!["issue".to_string()],
            runtime_generation: homeboy_core::build_identity::current().display,
            sensitive_mappings: vec!["TEST_TOKEN".to_string()],
            harvest_context: Default::default(),
        }
    }

    fn persist_recipe_run() -> (AgentTaskCookRecipe, AgentTaskPlan) {
        let recipe = recipe();
        let plan = recipe.attempts[0].plan.clone();
        write_recipe(&recipe).unwrap();
        crate::agent_task_lifecycle::submit_plan(&plan, Some("run")).unwrap();
        crate::agent_task_lifecycle::record_cook_attempt("cook", 1, "run").unwrap();
        (recipe, plan)
    }

    fn succeeded_aggregate(plan: &AgentTaskPlan) -> AgentTaskAggregate {
        AgentTaskAggregate {
            schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            status: AgentTaskAggregateStatus::Succeeded,
            totals: AgentTaskAggregateTotals {
                queued: 1,
                succeeded: 1,
                ..Default::default()
            },
            outcomes: vec![AgentTaskOutcome {
                schema: crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "task".to_string(),
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }],
            events: vec![AgentTaskProgressEvent {
                task_id: "task".to_string(),
                state: AgentTaskState::Succeeded,
                attempt: 1,
                message: Some("ok".to_string()),
            }],
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: Default::default(),
        }
    }

    #[test]
    fn recipe_schema_fails_closed_for_unknown_versions_and_missing_mappings() {
        let mut invalid = recipe();
        invalid.schema = "homeboy/agent-task-cook-recipe/v2".to_string();
        assert!(validate_recipe(&invalid)
            .unwrap_err()
            .message
            .contains("unsupported"));
        invalid.schema = COOK_RECIPE_SCHEMA.to_string();
        invalid.sensitive_mappings = vec![String::new()];
        assert!(validate_recipe(&invalid)
            .unwrap_err()
            .message
            .contains("sensitive mappings"));
    }

    #[test]
    fn external_dispatcher_recipe_requires_and_accepts_durable_reconstruction() {
        let mut remote_recipe = recipe();
        remote_recipe.promotion_transport["attempt_dispatch"] = serde_json::json!({
            "kind": "remote"
        });

        let error = reconstruct_options(&remote_recipe).expect_err("missing dispatcher blocks");
        assert_eq!(
            error.details["field"],
            "cook_recipe.promotion_transport.attempt_dispatch"
        );
        assert_eq!(
            error.details["problem"],
            "cook recipe requires `remote` attempt dispatcher reconstruction"
        );

        let options = reconstruct_options_with_dispatcher(
            &remote_recipe,
            Some(Arc::new(ReconstructedDispatcher)),
        )
        .expect("durable dispatcher reconstruction permits normal cook gates");
        assert!(options.attempt_dispatcher.is_some());
        assert_eq!(options.gates.verify, Vec::<String>::new());
        assert_eq!(options.to_worktree, "target");
        assert_eq!(options.base, "main");
    }

    #[test]
    fn recipe_reconstruction_reports_missing_finalization_field() {
        let mut incomplete = recipe();
        incomplete
            .finalization
            .as_object_mut()
            .expect("finalization object")
            .remove("to_worktree");

        let error = reconstruct_options(&incomplete).expect_err("missing target blocks adoption");
        assert_eq!(error.details["field"], "cook_recipe.finalization");
        assert_eq!(
            error.details["problem"],
            "missing finalization field `to_worktree`"
        );
    }

    #[test]
    fn durable_queue_deduplicates_and_survives_consumer_restart() {
        homeboy_core::test_support::with_isolated_home(|_| {
            write_recipe(&recipe()).unwrap();
            assert!(enqueue_terminal_continuation("cook", "run").unwrap());
            assert!(!enqueue_terminal_continuation("cook", "run").unwrap());
            let first = claim_continuation()
                .unwrap()
                .expect("durable queued continuation");
            assert_eq!(first.continuation().key, "cook:run");
            assert!(claim_continuation().unwrap().is_none());
        });
    }

    #[test]
    fn consumer_reconstructs_options_once_and_completed_work_never_replays() {
        homeboy_core::test_support::with_isolated_home(|_| {
            write_recipe(&recipe()).unwrap();
            enqueue_terminal_continuation("cook", "run").unwrap();
            let mut observed = None;
            assert_eq!(
                consume_next_with(|options| {
                    observed = Some(options);
                    Ok(0)
                })
                .unwrap(),
                Some(0)
            );
            let options = observed.expect("normal cook hook received options");
            assert_eq!(options.cook_id, "cook");
            assert_eq!(options.initial_run_id, "run");
            assert!(!enqueue_terminal_continuation("cook", "run").unwrap());
            assert!(
                consume_next_with(|_| panic!("completed continuation replayed"))
                    .unwrap()
                    .is_none()
            );
        });
    }

    #[test]
    fn dead_claim_is_reclaimed_and_retry_is_bounded() {
        homeboy_core::test_support::with_isolated_home(|_| {
            write_recipe(&recipe()).unwrap();
            enqueue_terminal_continuation("cook", "run").unwrap();
            let claim = claim_continuation().unwrap().unwrap();
            let dead = continuation_base_path(&claim.path).with_extension("claimed.4294967295");
            fs::rename(&claim.path, &dead).unwrap();
            claim_continuation().unwrap().unwrap().retry().unwrap();
            for _ in 0..3 {
                let claim = claim_continuation().unwrap().unwrap();
                assert!(
                    consume_claimed_with(claim, |_| Err(Error::internal_unexpected(
                        "retry".to_string()
                    )
                    .with_retryable(true)))
                    .is_err()
                );
            }
            assert!(claim_continuation().unwrap().is_none());
        });
    }

    #[test]
    fn malformed_continuation_is_terminalized_with_a_diagnostic() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let root = queue_root().unwrap();
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("malformed.pending"), b"not json").unwrap();

            let error = claim_continuation().unwrap_err();

            assert!(error.message.contains("malformed durable continuation"));
            assert!(root.join("malformed.failed").exists());
            assert!(fs::read_to_string(root.join("malformed.diagnostic"))
                .unwrap()
                .contains("malformed durable continuation"));
            assert!(!root.join("malformed.pending").exists());
        });
    }

    #[test]
    fn failed_and_cancelled_attempts_do_not_enqueue_continuations() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let (_, plan) = persist_recipe_run();
            crate::agent_task_lifecycle::record_pre_execution_failure(
                "run",
                &plan,
                "test",
                &Error::internal_unexpected("failed"),
            )
            .unwrap();

            crate::agent_task_lifecycle::status("run").unwrap();

            assert!(claim_continuation().unwrap().is_none());
        });
        homeboy_core::test_support::with_isolated_home(|_| {
            persist_recipe_run();
            crate::agent_task_lifecycle::cancel_run("run", Some("cancelled")).unwrap();

            crate::agent_task_lifecycle::status("run").unwrap();

            assert!(claim_continuation().unwrap().is_none());
        });
    }

    #[test]
    fn status_only_enqueues_and_never_invokes_the_consumer() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let (_, plan) = persist_recipe_run();
            let aggregate = succeeded_aggregate(&plan);
            crate::agent_task_lifecycle::record_run_aggregate("run", &plan, &aggregate).unwrap();
            let executions = AtomicUsize::new(0);

            crate::agent_task_lifecycle::status("run").unwrap();

            let record = crate::agent_task_lifecycle::status("run").unwrap();
            assert_eq!(
                record.metadata["cook_continuation_scheduler"]["status"],
                "queued"
            );
            assert_eq!(
                record.metadata["cook_continuation_scheduler"]["run_id"],
                "run"
            );

            assert_eq!(executions.load(Ordering::SeqCst), 0);
            assert_eq!(
                consume_next_with(|_| {
                    executions.fetch_add(1, Ordering::SeqCst);
                    Ok(0)
                })
                .unwrap(),
                Some(0)
            );
            assert_eq!(executions.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn concurrent_consumers_execute_one_continuation_once() {
        homeboy_core::test_support::with_isolated_home(|_| {
            write_recipe(&recipe()).unwrap();
            enqueue_terminal_continuation("cook", "run").unwrap();
            let executions = AtomicUsize::new(0);

            let results = std::thread::scope(|scope| {
                let first = scope.spawn(|| {
                    consume_next_with(|_| {
                        executions.fetch_add(1, Ordering::SeqCst);
                        Ok(0)
                    })
                    .unwrap()
                });
                let second = scope.spawn(|| {
                    consume_next_with(|_| {
                        executions.fetch_add(1, Ordering::SeqCst);
                        Ok(0)
                    })
                    .unwrap()
                });
                [first.join().unwrap(), second.join().unwrap()]
            });

            assert_eq!(executions.load(Ordering::SeqCst), 1);
            assert_eq!(
                results.iter().filter(|result| **result == Some(0)).count(),
                1
            );
            assert_eq!(results.iter().filter(|result| result.is_none()).count(), 1);
        });
    }

    #[test]
    fn retry_attempts_are_appended_idempotently_before_scheduling() {
        homeboy_core::test_support::with_isolated_home(|_| {
            write_recipe(&recipe()).unwrap();
            let mut retry_plan = recipe().attempts[0].plan.clone();
            retry_plan.plan_id = "retry-plan".to_string();

            record_recipe_attempt("cook", 2, "run-2", &retry_plan).unwrap();
            record_recipe_attempt("cook", 2, "run-2", &retry_plan).unwrap();

            let persisted = load_recipe("cook").unwrap();
            assert_eq!(persisted.attempts.len(), 2);
            assert_eq!(persisted.attempts[1].run_id, "run-2");
            let resumed = reconstruct_options(&persisted).unwrap();
            assert_eq!(persist_initial_recipe(&resumed).unwrap(), persisted);
            assert!(enqueue_terminal_continuation("cook", "run-2").unwrap());

            let mut conflicting = retry_plan;
            conflicting.plan_id = "different".to_string();
            assert!(record_recipe_attempt("cook", 2, "run-2", &conflicting).is_err());
        });
    }

    #[test]
    fn malformed_recipe_is_reported_by_status_without_executing_work() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let (_, plan) = persist_recipe_run();
            let aggregate = succeeded_aggregate(&plan);
            crate::agent_task_lifecycle::record_run_aggregate("run", &plan, &aggregate).unwrap();
            fs::write(recipe_path("cook").unwrap(), b"not json").unwrap();

            let record = crate::agent_task_lifecycle::status("run").unwrap();

            assert_eq!(
                record.metadata["cook_continuation_scheduler"]["status"],
                "failed"
            );
            assert!(record.metadata["cook_continuation_scheduler"]["message"]
                .as_str()
                .unwrap()
                .contains("malformed durable cook recipe"));
            assert!(claim_continuation().unwrap().is_none());
        });
    }
}
