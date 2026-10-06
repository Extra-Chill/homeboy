//! Durable, versioned input boundary for cook continuation scheduling.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::{Barrier, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::agent_task_lifecycle;
use crate::agent_task_scheduler::{AgentTaskAggregate, AgentTaskPlan};
use crate::agent_task_service::cook::{
    AgentTaskCookAttemptDispatcher, CookAiDisclosure, CookFinalization, CookIdentity, CookMode,
    CookProviderTransport, CookRequest, CookRetryPolicy, CookWorkspace,
};
use homeboy_core::command_invocation::CommandInvocation;

use homeboy_core::{paths, Error, Result};

/// Cook driver locks this process holds, keyed by lock path: the owning
/// thread, a reentrancy count, and the open file that keeps the OS lock alive.
type HeldCookDrivers = std::collections::HashMap<PathBuf, (std::thread::ThreadId, usize, File)>;

fn held_cook_drivers() -> &'static std::sync::Mutex<HeldCookDrivers> {
    static HELD: std::sync::OnceLock<std::sync::Mutex<HeldCookDrivers>> =
        std::sync::OnceLock::new();
    HELD.get_or_init(Default::default)
}

/// Ownership of driving one Cook; released when the last guard drops.
#[derive(Debug)]
pub(crate) struct CookDriverGuard {
    key: PathBuf,
}

impl Drop for CookDriverGuard {
    fn drop(&mut self) {
        let mut held = held_cook_drivers().lock().expect("cook driver registry");
        if let Some(entry) = held.get_mut(&self.key) {
            entry.1 -= 1;
            if entry.1 == 0 {
                // Dropping the file releases the OS lock.
                held.remove(&self.key);
            }
        }
    }
}

pub const COOK_RECIPE_SCHEMA: &str = "homeboy/agent-task-cook-recipe/v1";
const CONTINUATION_SCHEMA: &str = "homeboy/agent-task-cook-continuation/v1";
// Base capture reaches the network while holding this lock. It must always
// surface a wedged peer rather than inherit an operator-configured unbounded
// config-lock wait.
const WORKSPACE_BASE_CAPTURE_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
static INITIAL_RECIPE_CREATION_BARRIER: LazyLock<Mutex<(Option<Arc<Barrier>>, usize)>> =
    LazyLock::new(|| Mutex::new((None, 0)));

#[cfg(test)]
pub(crate) fn set_initial_recipe_creation_barrier_for_test(barrier: Option<Arc<Barrier>>) {
    let mut hook = INITIAL_RECIPE_CREATION_BARRIER
        .lock()
        .expect("initial recipe creation barrier");
    *hook = (barrier, 0);
}

/// Durable Cook storage bound to explicit filesystem roots.
#[derive(Clone, Debug)]
pub struct CookRecipeStore {
    data_root: PathBuf,
}

/// Result of publishing Cook's initial durable recipe.
///
/// `created` is elected by the exclusive recipe write, not by a caller's
/// observation of the store before it starts materializing an attempt.
pub struct InitialRecipeMaterialization {
    pub recipe: AgentTaskCookRecipe,
    pub created: bool,
}

impl InitialRecipeMaterialization {
    pub(crate) fn reused(recipe: AgentTaskCookRecipe) -> Self {
        Self {
            recipe,
            created: false,
        }
    }
}

impl CookRecipeStore {
    pub fn new(roots: paths::PathRoots) -> Self {
        Self::from_data_root(roots.data().to_path_buf())
    }

    pub fn from_environment() -> Result<Self> {
        Ok(Self::new(paths::PathRoots::from_environment()?))
    }

    /// Bind Cook's data-only storage without requiring unrelated config or
    /// artifact roots. Legacy Cook entry points use this to preserve their
    /// historical `HOMEBOY_DATA_DIR`-only contract.
    pub fn from_data_root(data: PathBuf) -> Self {
        Self { data_root: data }
    }

    pub fn from_current_data_root() -> Result<Self> {
        Ok(Self::from_data_root(paths::homeboy_data()?))
    }

    pub fn data_root(&self) -> PathBuf {
        self.data_root.clone()
    }

    fn recipe_root(&self) -> PathBuf {
        self.data_root.join("agent-task-cooks")
    }

    fn recipe_path(&self, cook_id: &str) -> PathBuf {
        self.recipe_root()
            .join(paths::sanitize_path_segment(cook_id))
            .join("recipe.json")
    }

    fn driver_lock_path(&self, cook_id: &str) -> PathBuf {
        self.recipe_path(cook_id).with_file_name("driver.lock")
    }

    /// Take exclusive ownership of driving one Cook (#15566).
    ///
    /// Exactly one controller may advance a Cook at a time: a foreground
    /// supervisor, a detached child, a continuation consumer or a
    /// `cook-continue`. Before this, ownership was inferred per attempt from
    /// metadata — a timed `local_cook_supervisor` lease that a detached
    /// handoff never writes — so the continuation scheduler judged a
    /// supervised Cook unowned and drove it beside its live supervisor,
    /// double-dispatching the same gate fix (#15562).
    ///
    /// The lock is an OS advisory lock on one file per Cook, so it needs no
    /// renewal, cannot outlive its holder, and cannot be fooled by PID reuse.
    /// Ownership is reentrant on the owning thread, so nested calls made by
    /// the driver itself keep driving. Another thread is another controller
    /// even in the same process — daemon jobs run as threads — and is refused
    /// exactly like another process.
    ///
    /// `Ok(None)` means another live controller holds it.
    pub(crate) fn try_acquire_cook_driver(&self, cook_id: &str) -> Result<Option<CookDriverGuard>> {
        use fs4::fs_std::FileExt;

        let key = self.driver_lock_path(cook_id);
        {
            let mut held = held_cook_drivers().lock().expect("cook driver registry");
            if let Some(entry) = held.get_mut(&key) {
                if entry.0 != std::thread::current().id() {
                    return Ok(None);
                }
                entry.1 += 1;
                return Ok(Some(CookDriverGuard { key }));
            }
        }
        let parent = key.parent().expect("driver lock has parent");
        fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(error.to_string(), Some(parent.display().to_string()))
        })?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&key)
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(key.display().to_string()))
            })?;
        match file.try_lock_exclusive() {
            Ok(true) => {}
            Ok(false) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
            Err(error) => {
                return Err(Error::internal_io(
                    error.to_string(),
                    Some(key.display().to_string()),
                ))
            }
        }
        // Diagnostic only; the lock, not this content, is the authority.
        let _ = file.set_len(0);
        let _ = write!(file, "{}", std::process::id());
        held_cook_drivers()
            .lock()
            .expect("cook driver registry")
            .insert(key.clone(), (std::thread::current().id(), 1, file));
        Ok(Some(CookDriverGuard { key }))
    }

    /// The PID of another controller currently driving `cook_id` (another
    /// process, or another thread of this one), or `None` when nobody else
    /// drives it. Never takes ownership.
    pub(crate) fn foreign_cook_driver(&self, cook_id: &str) -> Result<Option<String>> {
        use fs4::fs_std::FileExt;

        let key = self.driver_lock_path(cook_id);
        if let Some((owner, _, _)) = held_cook_drivers()
            .lock()
            .expect("cook driver registry")
            .get(&key)
        {
            return Ok(
                (*owner != std::thread::current().id()).then(|| std::process::id().to_string())
            );
        }
        if !key.exists() {
            return Ok(None);
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&key)
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(key.display().to_string()))
            })?;
        match file.try_lock_exclusive() {
            Ok(true) => {
                let _ = file.unlock();
                Ok(None)
            }
            Ok(false) => Ok(Some(fs::read_to_string(&key).unwrap_or_default())),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                Ok(Some(fs::read_to_string(&key).unwrap_or_default()))
            }
            Err(error) => Err(Error::internal_io(
                error.to_string(),
                Some(key.display().to_string()),
            )),
        }
    }

    /// Serialize the bounded base capture transaction for one durable recipe.
    /// Advisory lock ownership is tied to this open file, so the operating
    /// system releases it if the controller process exits before completing
    /// either persistence step.
    pub(crate) fn with_workspace_base_capture_lock<T>(
        &self,
        cook_id: &str,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.with_workspace_base_capture_lock_for(
            cook_id,
            WORKSPACE_BASE_CAPTURE_LOCK_TIMEOUT,
            operation,
        )
    }

    #[cfg(test)]
    pub(crate) fn with_workspace_base_capture_lock_for_test<T>(
        &self,
        cook_id: &str,
        timeout: Duration,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.with_workspace_base_capture_lock_for(cook_id, timeout, operation)
    }

    fn with_workspace_base_capture_lock_for<T>(
        &self,
        cook_id: &str,
        timeout: Duration,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let lock_path = self
            .recipe_path(cook_id)
            .with_file_name("workspace-base-capture.lock");
        let parent = lock_path.parent().expect("recipe lock has parent");
        fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(error.to_string(), Some(parent.display().to_string()))
        })?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("open Cook workspace base capture lock".to_string()),
                )
            })?;
        lock_workspace_base_capture(&lock, &lock_path, timeout)?;
        let _lock = lock;
        operation()
    }

    fn supersession_path(&self, cook_id: &str) -> PathBuf {
        self.recipe_path(cook_id)
            .with_file_name("supersession.json")
    }

    pub fn persist_recipe(&self, recipe: &AgentTaskCookRecipe) -> Result<()> {
        validate_recipe(recipe)?;
        let path = self.recipe_path(&recipe.cook_id);
        fs::create_dir_all(path.parent().expect("recipe path has parent")).map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
        homeboy_core::engine::local_files::write_json_file_owner_only(&path, recipe)
    }

    pub fn load_recipe(&self, cook_id: &str) -> Result<AgentTaskCookRecipe> {
        load_recipe_at(self.recipe_path(cook_id), cook_id)
    }

    pub fn recipe_exists(&self, cook_id: &str) -> bool {
        self.recipe_path(cook_id).exists()
    }

    pub fn load_recipe_for_attempt(&self, run_id: &str) -> Result<Option<AgentTaskCookRecipe>> {
        load_recipe_for_attempt_from(&self.recipe_root(), run_id)
    }

    pub fn persist_initial_recipe(&self, options: &CookRequest) -> Result<AgentTaskCookRecipe> {
        self.persist_initial_recipe_with_outcome(options)
            .map(|materialization| materialization.recipe)
    }

    pub(crate) fn persist_initial_recipe_with_outcome(
        &self,
        options: &CookRequest,
    ) -> Result<InitialRecipeMaterialization> {
        persist_initial_recipe_in_store(self, options)
    }

    pub fn validate_initial_recipe_compatibility(&self, options: &CookRequest) -> Result<()> {
        validate_initial_recipe_compatibility_in_store(self, options)
    }

    pub fn record_recipe_attempt(
        &self,
        cook_id: &str,
        attempt: u32,
        run_id: &str,
        plan: &AgentTaskPlan,
    ) -> Result<AgentTaskCookRecipe> {
        record_recipe_attempt_in_store(self, cook_id, attempt, run_id, plan)
    }

    pub fn record_recipe_attempt_replacement(
        &self,
        cook_id: &str,
        replaced_run_id: &str,
        replacement_run_id: &str,
    ) -> Result<AgentTaskCookRecipe> {
        let recipe = record_recipe_attempt_replacement_in_store(
            self,
            cook_id,
            replaced_run_id,
            replacement_run_id,
        )?;
        sync_fanout_replacement(self, &recipe, replaced_run_id, replacement_run_id)?;
        Ok(recipe)
    }

    pub(crate) fn record_recipe_attempt_replacement_with_plan(
        &self,
        cook_id: &str,
        replaced_run_id: &str,
        replacement_run_id: &str,
        plan: &AgentTaskPlan,
    ) -> Result<AgentTaskCookRecipe> {
        let recipe = record_recipe_attempt_replacement_in_store_with_plan(
            self,
            cook_id,
            replaced_run_id,
            replacement_run_id,
            plan,
        )?;
        sync_fanout_replacement(self, &recipe, replaced_run_id, replacement_run_id)?;
        Ok(recipe)
    }

    pub fn enqueue_continuation(
        &self,
        continuation: &AgentTaskCookContinuation,
        rearm_failed: bool,
    ) -> Result<bool> {
        enqueue_lifecycle_continuation(self, continuation, rearm_failed)
    }

    pub fn enqueue_terminal_continuation(&self, cook_id: &str, run_id: &str) -> Result<bool> {
        self.enqueue_terminal_continuation_with_recovery(cook_id, run_id, false)
    }

    /// Queue feedback remediation against the existing terminal Cook attempt.
    /// Failed continuation records may be rearmed, but no new attempt or budget
    /// is created here; normal continuation admission remains authoritative.
    pub fn enqueue_feedback_remediation(&self, cook_id: &str, run_id: &str) -> Result<bool> {
        let lifecycle_store =
            agent_task_lifecycle::AgentTaskLifecycleStore::from_data_root(self.data_root());
        let record = lifecycle_store.read_record(run_id)?;
        if !record.state.is_terminal() {
            return Ok(false);
        }
        let rearm_failed = matches!(
            continuation_state_in_store(self, cook_id, run_id)?,
            CookContinuationState::Failed
        );
        self.enqueue_terminal_continuation_with_recovery(cook_id, run_id, rearm_failed)
    }

    /// Publish continuation work for a terminal attempt. `rearm_failed` is the
    /// single authority that re-opens an already failed continuation, so a
    /// rearm and its controller-failure clear land in one record write.
    fn enqueue_terminal_continuation_with_recovery(
        &self,
        cook_id: &str,
        run_id: &str,
        rearm_failed: bool,
    ) -> Result<bool> {
        let recipe = self.load_recipe(cook_id)?;
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
        self.enqueue_continuation(&continuation, rearm_failed)
    }

    pub fn claim_continuation_with_budget(&self, budget: usize) -> Result<CookContinuationClaim> {
        claim_lifecycle_continuation_with_budget(self, budget)
    }

    pub fn claim_continuation_for(
        &self,
        cook_id: &str,
        run_id: &str,
    ) -> Result<Option<ClaimedCookContinuation>> {
        claim_lifecycle_continuation_for(self, cook_id, run_id)
    }

    pub fn consume_claimed_with_dispatcher(
        &self,
        claim: ClaimedCookContinuation,
        dispatcher: impl FnOnce(&Value) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
        execute: impl FnOnce(CookRequest) -> Result<i32>,
    ) -> Result<i32> {
        if claim.data_root != self.data_root() {
            return Err(Error::validation_invalid_argument(
                "cook_continuation.store",
                "claimed Cook continuation belongs to a different durable store",
                Some(claim.data_root.display().to_string()),
                None,
            ));
        }
        consume_claimed_with_dispatcher_policy(self, claim, dispatcher, execute, CookMode::Resume)
    }
}

fn sync_fanout_replacement(
    store: &CookRecipeStore,
    recipe: &AgentTaskCookRecipe,
    replaced_run_id: &str,
    replacement_run_id: &str,
) -> Result<()> {
    let Some(attempt) = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == replacement_run_id)
    else {
        return Ok(());
    };
    let Some(batch_id) = attempt.plan.metadata["batch_id"].as_str() else {
        return Ok(());
    };
    let batch_store =
        crate::agent_task_batch::AgentTaskBatchStore::from_data_root(store.data_root());
    crate::agent_task_batch::record_fanout_child_run_replacement_in_store(
        &batch_store,
        batch_id,
        replaced_run_id,
        replacement_run_id,
    )
}

fn lock_workspace_base_capture(lock: &File, lock_path: &Path, timeout: Duration) -> Result<()> {
    use fs4::fs_std::FileExt;

    let started = Instant::now();
    let mut backoff = Duration::from_millis(1);
    loop {
        match lock.try_lock_exclusive() {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => {
                return Err(Error::internal_io(
                    error.to_string(),
                    Some("lock Cook workspace base capture".to_string()),
                ));
            }
        }

        let waited = started.elapsed();
        if waited >= timeout {
            let mut error = Error::internal_io(
                format!(
                    "timed out after {}ms waiting for Cook workspace base capture lock at {}",
                    waited.as_millis(),
                    lock_path.display()
                ),
                Some("lock Cook workspace base capture".to_string()),
            );
            error.details = serde_json::json!({
                "kind": "workspace_base_capture_lock_timeout",
                "path": lock_path,
                "timeout_ms": timeout.as_millis(),
                "waited_ms": waited.as_millis(),
            });
            error.retryable = Some(true);
            return Err(error);
        }

        std::thread::sleep(backoff.min(timeout - waited));
        backoff = (backoff * 2).min(Duration::from_millis(50));
    }
}

pub(crate) fn default_store() -> Result<CookRecipeStore> {
    CookRecipeStore::from_current_data_root()
}

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
struct AgentTaskCookRecipeSupersession {
    schema: String,
    previous: AgentTaskCookRecipe,
    replacement: AgentTaskCookRecipe,
    changed_fields: Vec<String>,
}

const SUPERSESSION_SCHEMA: &str = "homeboy/agent-task-cook-recipe-supersession/v1";

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

/// A continuation claim owned by exactly one lifecycle record. `claim_identity`
/// is the authority every transition is checked against, so a claim that was
/// reclaimed by another consumer can no longer complete, retry, or fail it.
#[derive(Debug)]
pub struct ClaimedCookContinuation {
    continuation: AgentTaskCookContinuation,
    lifecycle_store: agent_task_lifecycle::AgentTaskLifecycleStore,
    claim_identity: String,
    /// Durable home the claim was taken from, so a consuming store can reject a
    /// claim that belongs to a different data root.
    data_root: PathBuf,
    active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookContinuationState {
    Absent,
    Pending,
    Claimed,
    Failed,
    Completed,
}

impl ClaimedCookContinuation {
    pub fn continuation(&self) -> &AgentTaskCookContinuation {
        &self.continuation
    }

    pub fn complete(self) -> Result<()> {
        self.transition(LifecycleContinuationTransition::Complete)
    }

    /// Return the claim to `pending`, or terminalize it once the retry budget
    /// is spent. The budget lives in the record, so a resumed consumer observes
    /// the same count the previous owner left behind.
    pub fn retry(self) -> Result<()> {
        self.transition(LifecycleContinuationTransition::Retry)
    }

    pub fn fail(self, diagnostic: &str) -> Result<()> {
        self.transition(LifecycleContinuationTransition::Fail(diagnostic))
    }

    fn transition(mut self, transition: LifecycleContinuationTransition<'_>) -> Result<()> {
        let result = transition_lifecycle_continuation(
            &self.lifecycle_store,
            &self.continuation.run_id,
            &self.claim_identity,
            transition,
        );
        if result.is_ok() {
            self.active = false;
        }
        result
    }
}

impl Drop for ClaimedCookContinuation {
    fn drop(&mut self) {
        if self.active {
            let _ = transition_lifecycle_continuation(
                &self.lifecycle_store,
                &self.continuation.run_id,
                &self.claim_identity,
                LifecycleContinuationTransition::RetryWithDiagnostic(
                    "Cook continuation worker exited before recording a result; retry scheduled",
                ),
            );
            self.active = false;
        }
    }
}

pub fn persist_initial_recipe(options: &CookRequest) -> Result<AgentTaskCookRecipe> {
    default_store()?.persist_initial_recipe(options)
}

pub fn persist_initial_recipe_in_store(
    store: &CookRecipeStore,
    options: &CookRequest,
) -> Result<InitialRecipeMaterialization> {
    recover_pending_supersession(store, &options.identity.cook_id)?;
    let mut recipe = initial_recipe(options)?;
    validate_recipe(&recipe)?;
    #[cfg(test)]
    let barrier = {
        let mut hook = INITIAL_RECIPE_CREATION_BARRIER
            .lock()
            .expect("initial recipe creation barrier");
        if hook.1 < 2 {
            hook.1 += 1;
            hook.0.clone()
        } else {
            None
        }
    };
    #[cfg(test)]
    if let Some(barrier) = barrier {
        barrier.wait();
    }
    let recipe_existed_before_admission = store.recipe_exists(&recipe.cook_id);
    if let Some(existing) = compatible_existing_recipe(store, &recipe)? {
        return Ok(InitialRecipeMaterialization {
            recipe: existing,
            created: false,
        });
    }
    if store.recipe_exists(&recipe.cook_id) {
        let existing = store.load_recipe(&recipe.cook_id)?;
        if !recipe_existed_before_admission {
            let mut error = Error::validation_invalid_argument(
                "cook_recipe",
                "concurrent Cook creation conflicts with the durable recipe",
                Some(recipe.cook_id),
                None,
            );
            error.details["concurrent_cook_creation_loser"] = serde_json::Value::Bool(true);
            return Err(error);
        }
        let mismatches = recipe_mismatch_fields(&existing, &recipe);
        let lifecycle_store =
            agent_task_lifecycle::AgentTaskLifecycleStore::from_data_root(store.data_root());
        ensure_correction_is_safe_in_store(&lifecycle_store, &existing, &recipe, &mismatches)?;
        let requested_attempt = recipe.attempts.pop().expect("validated recipe has attempt");
        let next_attempt = existing
            .attempts
            .iter()
            .map(|attempt| attempt.attempt)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        recipe.attempts = existing.attempts.clone();
        recipe.attempts.push(AgentTaskCookRecipeAttempt {
            attempt: next_attempt,
            ..requested_attempt
        });
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
        let supersession = AgentTaskCookRecipeSupersession {
            schema: SUPERSESSION_SCHEMA.to_string(),
            previous: existing,
            replacement: recipe.clone(),
            changed_fields: mismatches
                .iter()
                .map(|field| (*field).to_string())
                .collect(),
        };
        write_supersession(store, &supersession)?;
        complete_supersession(store, &supersession)?;
        return Ok(InitialRecipeMaterialization {
            recipe,
            created: false,
        });
    }
    if persist_recipe_exclusively(store, &recipe)? {
        return Ok(InitialRecipeMaterialization {
            recipe,
            created: true,
        });
    }
    // Another controller won creation after our read. Its immutable recipe is
    // authoritative: a concurrent loser may reuse it only when compatible,
    // never enter the normal correction/supersession path.
    let winner = store.load_recipe(&recipe.cook_id)?;
    if recipe_mismatch_fields(&winner, &recipe).is_empty() {
        return Ok(InitialRecipeMaterialization::reused(winner));
    }
    let mut error = Error::validation_invalid_argument(
        "cook_recipe",
        "concurrent Cook creation conflicts with the durable recipe",
        Some(recipe.cook_id),
        None,
    );
    error.details["concurrent_cook_creation_loser"] = serde_json::Value::Bool(true);
    Err(error)
}

fn persist_recipe_exclusively(
    store: &CookRecipeStore,
    recipe: &AgentTaskCookRecipe,
) -> Result<bool> {
    let path = store.recipe_path(&recipe.cook_id);
    let directory = path.parent().expect("recipe path has parent");
    fs::create_dir_all(directory)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    let json = serde_json::to_string_pretty(recipe).map_err(|error| {
        Error::internal_json(error.to_string(), Some(path.display().to_string()))
    })?;
    let staging = write_recipe_staging_file(directory, format!("{json}\n").as_bytes())?;
    let installed = match fs::hard_link(&staging, &path) {
        Ok(()) => {
            sync_directory(directory)?;
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => {
            let _ = fs::remove_file(&staging);
            return Err(Error::internal_io(
                error.to_string(),
                Some(path.display().to_string()),
            ));
        }
    };
    fs::remove_file(&staging).map_err(|error| {
        Error::internal_io(error.to_string(), Some(staging.display().to_string()))
    })?;
    if installed {
        sync_directory(directory)?;
    }
    Ok(installed)
}

fn write_recipe_staging_file(directory: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let staging = directory.join(format!(".recipe-{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&staging).map_err(|error| {
        Error::internal_io(error.to_string(), Some(staging.display().to_string()))
    })?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&staging);
        return Err(Error::internal_io(
            error.to_string(),
            Some(staging.display().to_string()),
        ));
    }
    Ok(staging)
}

fn sync_directory(directory: &Path) -> Result<()> {
    File::open(directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(directory.display().to_string()))
        })
}

/// Validate a Cook recipe against durable state without writing it. Fanout
/// uses this before creating worktrees so incompatible replays fail before any
/// lifecycle resources are mutated.
pub fn validate_initial_recipe_compatibility(options: &CookRequest) -> Result<()> {
    default_store()?.validate_initial_recipe_compatibility(options)
}

pub fn validate_initial_recipe_compatibility_in_store(
    store: &CookRecipeStore,
    options: &CookRequest,
) -> Result<()> {
    if !store.recipe_exists(&options.identity.cook_id) {
        return Ok(());
    }
    let existing = store.load_recipe(&options.identity.cook_id)?;
    let mut recipe = initial_recipe(options)?;
    // The attempt plan is compiled only after the target worktree exists. Use
    // the durable plan here so preflight compares every input already known
    // without weakening persistence-time plan validation.
    recipe.attempts = existing.attempts.clone();
    recipe.retry_budget["execution_budget"] = existing.retry_budget["execution_budget"].clone();
    // Recipes written before retry-policy provenance was introduced remain
    // resumable. Their resolved execution budget is already immutable; omit
    // the new descriptive field from the compatibility comparison until a
    // pre-provider correction writes the current recipe shape.
    if existing.retry_budget.get("policy").is_none() {
        recipe
            .retry_budget
            .as_object_mut()
            .expect("retry budget is an object")
            .remove("policy");
    }
    let mismatches = recipe_mismatch_fields(&existing, &recipe);
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_data_root(store.data_root());
    ensure_correction_is_safe_in_store(&lifecycle_store, &existing, &recipe, &mismatches)?;
    Ok(())
}

fn initial_recipe(options: &CookRequest) -> Result<AgentTaskCookRecipe> {
    let attempt_dispatch = options
        .provider_transport
        .attempt_dispatcher
        .as_ref()
        .map(|dispatcher| dispatcher.durable_recipe())
        .transpose()?
        .unwrap_or_else(|| serde_json::json!({ "kind": "local" }));
    let recipe = AgentTaskCookRecipe {
        schema: COOK_RECIPE_SCHEMA.to_string(),
        cook_id: options.identity.cook_id.clone(),
        attempts: vec![AgentTaskCookRecipeAttempt {
            attempt: 1,
            run_id: options.identity.initial_run_id.clone(),
            plan: options.identity.initial_plan.clone(),
        }],
        promotion_transport: serde_json::json!({
            "provider_command": options.provider_transport.provider_command,
            "provider_invocation": options.provider_transport.provider_invocation,
            "attempt_dispatch": attempt_dispatch,
        }),
        gate_policy: serde_json::to_value(&options.gates).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize cook gate policy".to_string()),
            )
        })?,
        retry_budget: serde_json::json!({
            "max_attempts": options.retry_policy.max_attempts,
            "execution_budget": options.identity.initial_plan.options.execution_budget,
            "policy": options.identity.initial_plan.metadata["cook_retry_policy"],
            "timeouts": cook_recipe_timeout_disclosure(&options.identity.initial_plan),
        }),
        finalization: serde_json::json!({
            "no_finalize": options.finalization.no_finalize,
            "draft_pr": options.finalization.draft_pr,
            "provider_ci": options.finalization.provider_ci,
            "base": options.finalization.base,
            "head": options.finalization.head,
            "title": options.finalization.title,
            "commit_message": options.finalization.commit_message,
            "protected_branches": options.finalization.protected_branches,
            "ai_tool": options.ai_disclosure.ai_tool,
            "ai_model": options.ai_disclosure.ai_model,
            "ai_used_for": options.ai_disclosure.ai_used_for,
            "to_worktree": options.workspace.to_worktree,
            "source_worktree_path": options.workspace.source_worktree_path,
            "task_base_sha": options.workspace.task_base_sha,
        }),
        source_refs: options.workspace.source_refs.clone(),
        runtime_generation: homeboy_core::build_identity::current().display,
        sensitive_mappings: sensitive_mappings(&options.identity.initial_plan)?,
        harvest_context: options.harvest_context.clone(),
    };
    Ok(recipe)
}

fn compatible_existing_recipe(
    store: &CookRecipeStore,
    recipe: &AgentTaskCookRecipe,
) -> Result<Option<AgentTaskCookRecipe>> {
    if store.recipe_exists(&recipe.cook_id) {
        let existing = store.load_recipe(&recipe.cook_id)?;
        let mut expected = recipe.clone();
        expected.attempts = existing.attempts.clone();
        expected.sensitive_mappings = existing.sensitive_mappings.clone();
        // Harvest transport belongs to the original controller execution. A
        // replay must use that persisted context rather than ambient state.
        expected.harvest_context = existing.harvest_context.clone();
        // Base capture can finish between two concurrent initial admissions.
        // A request that has not captured a base yet must adopt the durable
        // recipe boundary instead of superseding the recipe another controller
        // is already continuing.
        if existing.finalization["task_base_sha"].is_string()
            && expected.finalization["task_base_sha"].is_null()
        {
            expected.finalization["task_base_sha"] = existing.finalization["task_base_sha"].clone();
        }
        let requested_attempt = recipe
            .attempts
            .first()
            .expect("validated recipe has attempt");
        let recorded_attempt = existing
            .attempts
            .iter()
            .find(|attempt| attempt.run_id == requested_attempt.run_id);
        let inputs_match = recorded_attempt
            .map(|attempt| {
                attempt.plan == requested_attempt.plan
                    && ((existing.attempts.len() > 1 && attempt.attempt == 1)
                        || recipes_match(&existing, &expected))
            })
            .unwrap_or_else(|| {
                recipes_match(&existing, &expected)
                    && initial_attempt_inputs_match(
                        existing
                            .attempts
                            .first()
                            .expect("validated recipe has attempt"),
                        requested_attempt,
                    )
            });
        if !inputs_match {
            return Ok(None);
        }
        return Ok(Some(existing));
    }
    Ok(None)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RecipeFreezeBoundary {
    Provider,
    Candidate,
    Promotion,
    Gate,
    Finalization,
}

impl RecipeFreezeBoundary {
    fn name(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Candidate => "candidate",
            Self::Promotion => "promotion",
            Self::Gate => "gate",
            Self::Finalization => "finalization",
        }
    }
}

fn mismatch_freeze_boundary(field: &str) -> RecipeFreezeBoundary {
    match field {
        "promotion_transport" | "retry_budget" | "runtime_generation" | "harvest_context" => {
            RecipeFreezeBoundary::Provider
        }
        "source_refs" | "sensitive_mappings" | "attempts" => RecipeFreezeBoundary::Candidate,
        "finalization.to_worktree"
        | "finalization.source_worktree_path"
        | "finalization.task_base_sha" => RecipeFreezeBoundary::Promotion,
        "gate_policy" => RecipeFreezeBoundary::Gate,
        "finalization" => RecipeFreezeBoundary::Finalization,
        _ => RecipeFreezeBoundary::Provider,
    }
}

/// [`reached_freeze_boundary`] against an explicitly injected lifecycle root.
///
/// The freeze boundary is decided entirely by what the attempts' own records
/// say: provider executions authenticate the candidate, an applied promotion
/// locks the destination. Reading those ambiently lets another home's record of
/// the same run id decide whether this recipe is correctable (#7505).
fn reached_freeze_boundary_in_store(
    lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore,
    recipe: &AgentTaskCookRecipe,
) -> Result<Option<RecipeFreezeBoundary>> {
    let mut reached = None;
    for attempt in &recipe.attempts {
        let Ok(record) = crate::agent_task_lifecycle::reconcile_status_in_store(
            lifecycle_store,
            &attempt.run_id,
            crate::agent_task_lifecycle::AgentTaskStatusOptions::default(),
            false,
        )
        .map(|outcome| outcome.record) else {
            continue;
        };
        if record.metadata["provider_executions_consumed"]
            .as_u64()
            .unwrap_or_default()
            > 0
            || record.metadata["provider_executions"]
                .as_array()
                .is_some_and(|executions| !executions.is_empty())
        {
            // Provider output authenticates the source candidate. This locks
            // dispatch and candidate inputs, while destination inputs remain
            // correctable until an applied promotion exists.
            reached = Some(RecipeFreezeBoundary::Candidate);
        }
        let promotion = serde_json::from_value::<
            crate::agent_task_promotion::AgentTaskPromotionReport,
        >(record.metadata["latest_promotion"].clone())
        .ok();
        if promotion
            .as_ref()
            .is_some_and(|promotion| promotion.status.patch_promoted())
        {
            reached = Some(RecipeFreezeBoundary::Promotion);
            if promotion.is_some_and(|promotion| {
                !promotion.gate_results.is_empty() || !promotion.deterministic_gates.is_empty()
            }) {
                reached = Some(RecipeFreezeBoundary::Gate);
            }
            if !record.metadata["cook_finalization"].is_null() {
                reached = Some(RecipeFreezeBoundary::Finalization);
            }
        }
    }
    Ok(reached)
}

#[cfg(test)]
fn ensure_correction_is_safe(
    existing: &AgentTaskCookRecipe,
    requested: &AgentTaskCookRecipe,
    mismatches: &[&str],
) -> Result<()> {
    ensure_correction_is_safe_in_store(
        &crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?,
        existing,
        requested,
        mismatches,
    )
}

/// [`ensure_correction_is_safe`] against an explicitly injected lifecycle root.
fn ensure_correction_is_safe_in_store(
    lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore,
    existing: &AgentTaskCookRecipe,
    requested: &AgentTaskCookRecipe,
    mismatches: &[&str],
) -> Result<()> {
    let reached = reached_freeze_boundary_in_store(lifecycle_store, existing)?;
    let frozen = mismatches
        .iter()
        .copied()
        .filter(|field| reached.is_some_and(|boundary| mismatch_freeze_boundary(field) <= boundary))
        .collect::<Vec<_>>();
    if frozen.is_empty() {
        return Ok(());
    }
    let boundary = reached.expect("frozen fields require a reached boundary");
    Err(Error::validation_invalid_argument(
        "cook_recipe",
        format!(
            "durable cook recipe correction is unsafe after {} execution: {}. Resume the existing recipe; corrected inputs require a new Cook ID.",
            boundary.name(),
            frozen.join(", ")
        ),
        Some(requested.cook_id.clone()),
        Some(vec![format!(
            "Resume `{}` with its immutable {}-boundary inputs.",
            existing.cook_id,
            boundary.name()
        )]),
    ))
}

fn recover_pending_supersession(store: &CookRecipeStore, cook_id: &str) -> Result<()> {
    let path = store.supersession_path(cook_id);
    if !path.exists() {
        return Ok(());
    }
    let supersession: AgentTaskCookRecipeSupersession =
        serde_json::from_slice(&fs::read(&path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?)
        .map_err(|error| {
            Error::validation_invalid_argument(
                "cook_recipe",
                format!("malformed durable cook recipe supersession: {error}"),
                Some(cook_id.to_string()),
                None,
            )
        })?;
    if supersession.schema != SUPERSESSION_SCHEMA || supersession.previous.cook_id != cook_id {
        return Err(Error::validation_invalid_argument(
            "cook_recipe",
            "durable cook recipe supersession does not match its Cook",
            Some(cook_id.to_string()),
            None,
        ));
    }
    complete_supersession(store, &supersession)
}

fn write_supersession(
    store: &CookRecipeStore,
    supersession: &AgentTaskCookRecipeSupersession,
) -> Result<()> {
    let path = store.supersession_path(&supersession.previous.cook_id);
    homeboy_core::engine::local_files::write_json_file_owner_only(&path, supersession)
}

fn complete_supersession(
    store: &CookRecipeStore,
    supersession: &AgentTaskCookRecipeSupersession,
) -> Result<()> {
    // The intent is durable before either result. Replaying always replaces the
    // active recipe first, then records immutable history, so either crash point
    // converges on the same state without blocking the corrected retry.
    store.persist_recipe(&supersession.replacement)?;
    archive_recipe_revision(store, supersession)?;
    let path = store.supersession_path(&supersession.previous.cook_id);
    fs::remove_file(&path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))
}

fn archive_recipe_revision(
    store: &CookRecipeStore,
    supersession: &AgentTaskCookRecipeSupersession,
) -> Result<()> {
    let root = store
        .recipe_path(&supersession.previous.cook_id)
        .parent()
        .expect("recipe path has parent")
        .join("recipe-history");
    fs::create_dir_all(&root)
        .map_err(|error| Error::internal_io(error.to_string(), Some(root.display().to_string())))?;
    let revision = format!("{:04}", supersession.previous.attempts.len());
    let recipe_path = root.join(format!("{revision}.recipe.json"));
    if recipe_path.exists() {
        let archived: AgentTaskCookRecipe =
            serde_json::from_slice(&fs::read(&recipe_path).map_err(|error| {
                Error::internal_io(error.to_string(), Some(recipe_path.display().to_string()))
            })?)
            .map_err(|error| {
                Error::validation_invalid_argument(
                    "cook_recipe",
                    format!("malformed archived cook recipe: {error}"),
                    Some(supersession.previous.cook_id.clone()),
                    None,
                )
            })?;
        if archived != supersession.previous {
            return Err(Error::validation_invalid_argument(
                "cook_recipe",
                "durable recipe revision conflicts with a different immutable history entry",
                Some(supersession.previous.cook_id.clone()),
                None,
            ));
        }
    } else {
        homeboy_core::engine::local_files::write_json_file_owner_only(
            &recipe_path,
            &supersession.previous,
        )?;
    }
    homeboy_core::engine::local_files::write_json_file_owner_only(
        &root.join(format!("{revision}.supersession.json")),
        &serde_json::json!({
            "schema": SUPERSESSION_SCHEMA,
            "cook_id": supersession.previous.cook_id,
            "replaced_attempt_run_id": supersession.previous.attempts.last().map(|attempt| &attempt.run_id),
            "replacement_attempt_run_id": supersession.replacement.attempts.last().map(|attempt| &attempt.run_id),
            "changed_fields": supersession.changed_fields,
        }),
    )?;
    Ok(())
}

/// `homeboy_plan` is a derived execution projection rebuilt by each controller
/// compile. Compare its typed source inputs, not transient projection details,
/// when deciding whether a persisted recipe may resume.
fn recipes_match(left: &AgentTaskCookRecipe, right: &AgentTaskCookRecipe) -> bool {
    recipe_mismatch_fields(left, right).is_empty()
}

fn recipe_mismatch_fields(
    left: &AgentTaskCookRecipe,
    right: &AgentTaskCookRecipe,
) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if left.schema != right.schema {
        fields.push("schema");
    }
    if left.cook_id != right.cook_id {
        fields.push("cook_id");
    }
    if left.promotion_transport != right.promotion_transport {
        fields.push("promotion_transport");
    }
    if left.gate_policy != right.gate_policy {
        fields.push("gate_policy");
    }
    if !retry_budgets_match(&left.retry_budget, &right.retry_budget) {
        fields.push("retry_budget");
    }
    if left.finalization != right.finalization {
        for field in ["to_worktree", "source_worktree_path", "task_base_sha"] {
            if left.finalization.get(field) != right.finalization.get(field) {
                fields.push(match field {
                    "to_worktree" => "finalization.to_worktree",
                    "source_worktree_path" => "finalization.source_worktree_path",
                    "task_base_sha" => "finalization.task_base_sha",
                    _ => unreachable!(),
                });
            }
        }
        if [
            "no_finalize",
            "base",
            "head",
            "title",
            "commit_message",
            "protected_branches",
            "ai_tool",
            "ai_model",
            "ai_used_for",
        ]
        .iter()
        .any(|field| left.finalization.get(field) != right.finalization.get(field))
        {
            fields.push("finalization");
        }
    }
    if left.source_refs != right.source_refs {
        fields.push("source_refs");
    }
    if left.runtime_generation != right.runtime_generation {
        fields.push("runtime_generation");
    }
    if left.sensitive_mappings != right.sensitive_mappings {
        fields.push("sensitive_mappings");
    }
    if left.harvest_context != right.harvest_context {
        fields.push("harvest_context");
    }
    if left.attempts.len() != right.attempts.len()
        || !left
            .attempts
            .iter()
            .zip(&right.attempts)
            .all(|(left, right)| attempt_inputs_match(left, right))
    {
        fields.push("attempts");
    }
    fields
}

/// Retry-policy provenance may gain these descriptive views as the controller
/// learns to explain a resolved budget more completely. Every other policy
/// field remains immutable, including future policy-bearing fields.
fn retry_budgets_match(left: &Value, right: &Value) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    for budget in [&mut left, &mut right] {
        if let Some(policy) = budget.get_mut("policy").and_then(Value::as_object_mut) {
            for field in ["requested", "effective", "truncated", "timeouts"] {
                policy.remove(field);
            }
        }
        if let Some(budget) = budget.as_object_mut() {
            budget.remove("timeouts");
        }
    }
    left == right
}

fn cook_recipe_timeout_disclosure(plan: &AgentTaskPlan) -> Value {
    let limits = plan.tasks.first().map(|task| &task.limits);
    let provider_timeout_ms = crate::agent_task_timeout::effective_provider_timeout_ms(
        limits
            .and_then(|limits| limits.timeout_ms)
            .or(plan.options.timeout_ms),
        limits.and_then(|limits| limits.max_runtime_ms),
    );
    let review_form_timeout_ms = plan
        .tasks
        .first()
        .map(crate::agent_task_cook_loop::review_form_timeout_ms)
        .unwrap_or(crate::agent_task_cook_loop::DEFAULT_REVIEW_FORM_TIMEOUT_MS);
    serde_json::json!({
        "provider_timeout_ms": provider_timeout_ms,
        "review_form_timeout_ms": review_form_timeout_ms,
        "review_form_timeout_cap_ms": crate::agent_task_cook_loop::MAX_REVIEW_FORM_TIMEOUT_MS,
        "review_form_provider_budget_scope": "fresh_cook_review",
        "review_form_provider_budget_is_distinct": true,
    })
}

fn attempt_inputs_match(
    left: &AgentTaskCookRecipeAttempt,
    right: &AgentTaskCookRecipeAttempt,
) -> bool {
    left == right
}

fn initial_attempt_inputs_match(
    left: &AgentTaskCookRecipeAttempt,
    right: &AgentTaskCookRecipeAttempt,
) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    // A CLI retry compiles fresh random run and plan identifiers. The persisted
    // recipe remains authoritative for those identities; user-supplied plan
    // inputs stay subject to the strict comparison below.
    left.run_id.clear();
    right.run_id.clear();
    left.plan.plan_id.clear();
    right.plan.plan_id.clear();
    left.plan.rebuild_homeboy_plan();
    right.plan.rebuild_homeboy_plan();
    attempt_inputs_match(&left, &right)
}

pub(crate) fn record_recipe_attempt(
    cook_id: &str,
    attempt: u32,
    run_id: &str,
    plan: &AgentTaskPlan,
) -> Result<AgentTaskCookRecipe> {
    default_store()?.record_recipe_attempt(cook_id, attempt, run_id, plan)
}

pub(crate) fn record_recipe_attempt_replacement_with_plan(
    cook_id: &str,
    replaced_run_id: &str,
    replacement_run_id: &str,
    plan: &AgentTaskPlan,
) -> Result<AgentTaskCookRecipe> {
    default_store()?.record_recipe_attempt_replacement_with_plan(
        cook_id,
        replaced_run_id,
        replacement_run_id,
        plan,
    )
}

pub fn record_recipe_attempt_in_store(
    store: &CookRecipeStore,
    cook_id: &str,
    attempt: u32,
    run_id: &str,
    plan: &AgentTaskPlan,
) -> Result<AgentTaskCookRecipe> {
    let mut recipe = store.load_recipe(cook_id)?;
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
    let next_attempt = recipe
        .attempts
        .iter()
        .map(|attempt| attempt.attempt)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    if attempt != next_attempt {
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
    store.persist_recipe(&recipe)?;
    Ok(recipe)
}

pub fn record_recipe_attempt_replacement_in_store(
    store: &CookRecipeStore,
    cook_id: &str,
    replaced_run_id: &str,
    replacement_run_id: &str,
) -> Result<AgentTaskCookRecipe> {
    let plan = store
        .load_recipe(cook_id)?
        .attempts
        .last()
        .map(|attempt| attempt.plan.clone())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "durable cook recipe has no attempt to replace",
                Some(cook_id.to_string()),
                None,
            )
        })?;
    record_recipe_attempt_replacement_in_store_with_plan(
        store,
        cook_id,
        replaced_run_id,
        replacement_run_id,
        &plan,
    )
}

fn record_recipe_attempt_replacement_in_store_with_plan(
    store: &CookRecipeStore,
    cook_id: &str,
    replaced_run_id: &str,
    replacement_run_id: &str,
    replacement_plan: &AgentTaskPlan,
) -> Result<AgentTaskCookRecipe> {
    let mut recipe = store.load_recipe(cook_id)?;
    if let Some(existing) = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == replacement_run_id)
    {
        if recipe
            .attempts
            .iter()
            .any(|attempt| attempt.run_id == replaced_run_id && attempt.attempt == existing.attempt)
            && existing.plan == *replacement_plan
        {
            return Ok(recipe);
        }
        return Err(Error::validation_invalid_argument(
            "cook_recipe.attempts",
            "replacement run id is already bound to another durable cook attempt",
            Some(replacement_run_id.to_string()),
            None,
        ));
    }
    let replaced = recipe.attempts.last().cloned().ok_or_else(|| {
        Error::validation_invalid_argument(
            "cook_recipe.attempts",
            "durable cook recipe has no attempt to replace",
            Some(cook_id.to_string()),
            None,
        )
    })?;
    if replaced.run_id != replaced_run_id {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.attempts",
            "only the latest durable cook attempt can receive a replacement run",
            Some(replaced_run_id.to_string()),
            None,
        ));
    }
    recipe.attempts.push(AgentTaskCookRecipeAttempt {
        attempt: replaced.attempt,
        run_id: replacement_run_id.to_string(),
        plan: replacement_plan.clone(),
    });
    recipe.sensitive_mappings = canonical_sensitive_mappings(&recipe.attempts)?;
    validate_recipe(&recipe)?;
    store.persist_recipe(&recipe)?;
    Ok(recipe)
}

pub fn load_recipe(cook_id: &str) -> Result<AgentTaskCookRecipe> {
    default_store()?.load_recipe(cook_id)
}

fn load_recipe_at(path: PathBuf, cook_id: &str) -> Result<AgentTaskCookRecipe> {
    let raw = fs::read_to_string(&path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    let mut recipe: AgentTaskCookRecipe = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_argument(
            "cook_recipe",
            format!("malformed durable cook recipe: {error}"),
            Some(cook_id.to_string()),
            None,
        )
    })?;
    normalize_sensitive_mapping_projection(&mut recipe)?;
    validate_recipe(&recipe)?;
    if recipe.cook_id != cook_id {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.cook_id",
            "durable cook recipe identity does not match its storage location",
            Some(recipe.cook_id.clone()),
            None,
        ));
    }
    Ok(recipe)
}

/// Legacy cook indexes predate this durable scheduler contract. Their status
/// projection remains read-only and preserves the previous orphan behavior.
pub fn recipe_exists(cook_id: &str) -> Result<bool> {
    Ok(default_store()?.recipe_exists(cook_id))
}

/// Locate an orphaned attempt by its exact durable run ID. This permits an
/// operator to disambiguate a cook whose retry attempts have different plans.
pub fn load_recipe_for_attempt(run_id: &str) -> Result<Option<AgentTaskCookRecipe>> {
    default_store()?.load_recipe_for_attempt(run_id)
}

fn load_recipe_for_attempt_from(
    root: &std::path::Path,
    run_id: &str,
) -> Result<Option<AgentTaskCookRecipe>> {
    if !root.exists() {
        return Ok(None);
    }

    let mut matches = Vec::new();
    for entry in fs::read_dir(root)
        .map_err(|error| Error::internal_io(error.to_string(), Some(root.display().to_string())))?
    {
        let path = entry
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(root.display().to_string()))
            })?
            .path()
            .join("recipe.json");
        if !path.exists() {
            continue;
        }
        let raw = fs::read_to_string(&path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
        let mut recipe: AgentTaskCookRecipe = serde_json::from_str(&raw).map_err(|error| {
            Error::validation_invalid_argument(
                "cook_recipe",
                format!("malformed durable cook recipe: {error}"),
                Some(path.display().to_string()),
                None,
            )
        })?;
        normalize_sensitive_mapping_projection(&mut recipe)?;
        validate_recipe(&recipe)?;
        if recipe
            .attempts
            .iter()
            .any(|attempt| attempt.run_id == run_id)
        {
            matches.push(recipe);
        }
    }

    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        _ => Err(Error::validation_invalid_argument(
            "run_or_cook_id",
            "durable run id is declared by multiple cook recipes; inspect the recipe records before adoption",
            Some(run_id.to_string()),
            None,
        )),
    }
}

/// Prove that a lifecycle record is the exact attempt frozen by a Cook recipe.
/// Older runner mirrors may omit `metadata.cook_id`; immutable recipe membership
/// remains authoritative in that case, but an observed Cook identity may never
/// disagree with it.
pub fn validate_recipe_attempt_record(
    recipe: &AgentTaskCookRecipe,
    run_id: &str,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Result<()> {
    let controller_plan = agent_task_lifecycle::load_controller_plan(run_id)?;
    validate_recipe_attempt_record_with_controller_plan(recipe, run_id, record, &controller_plan)
}

fn validate_recipe_attempt_record_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    recipe: &AgentTaskCookRecipe,
    run_id: &str,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Result<()> {
    let controller_plan = lifecycle_store.read_controller_plan(run_id)?;
    validate_recipe_attempt_record_with_controller_plan(recipe, run_id, record, &controller_plan)
}

pub(crate) fn validate_recipe_attempt_record_with_controller_plan(
    recipe: &AgentTaskCookRecipe,
    run_id: &str,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
    controller_plan: &AgentTaskPlan,
) -> Result<()> {
    let attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.attempts",
                format!(
                    "immutable Cook recipe `{}` does not declare expected attempt `{run_id}`",
                    recipe.cook_id
                ),
                Some(run_id.to_string()),
                None,
            )
        })?;
    let observed_cook_id = record.metadata.get("cook_id").and_then(Value::as_str);
    if record.run_id != attempt.run_id
        || observed_cook_id.is_some_and(|cook_id| cook_id != recipe.cook_id)
    {
        return Err(Error::validation_invalid_argument(
            "cook_or_attempt_id",
            format!(
                "durable lifecycle identity mismatch: expected Cook `{}` attempt {} run `{}`; observed Cook `{}` run `{}`",
                recipe.cook_id,
                attempt.attempt,
                attempt.run_id,
                observed_cook_id.unwrap_or("<missing>"),
                record.run_id,
            ),
            Some(run_id.to_string()),
            Some(vec![
                "Inspect the immutable Cook recipe and lifecycle record; do not continue or promote across Cook identities."
                    .to_string(),
            ]),
        ));
    }
    let plan_identity_matches = controller_plan.tasks.len() == attempt.plan.tasks.len()
        && attempt.plan.tasks.iter().all(|expected| {
            controller_plan
                .tasks
                .iter()
                .find(|observed| observed.task_id == expected.task_id)
                .is_some_and(|observed| {
                    observed.source_refs == expected.source_refs
                        && observed.workspace.slug == expected.workspace.slug
                        && observed.workspace.component_id == expected.workspace.component_id
                        && observed.workspace.branch == expected.workspace.branch
                        && observed.workspace.base_ref == expected.workspace.base_ref
                        && observed.workspace.task_url == expected.workspace.task_url
                        && observed.workspace.attempt == expected.workspace.attempt
                })
        });
    if !plan_identity_matches {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.attempts.plan",
            format!(
                "durable controller plan for Cook `{}` attempt `{run_id}` does not match the immutable recipe repository, base, and candidate inputs",
                recipe.cook_id
            ),
            Some(run_id.to_string()),
            Some(vec![
                "Inspect the controller-owned recipe and run plan; do not continue across repository or base identities."
                    .to_string(),
            ]),
        ));
    }
    Ok(())
}

/// Refresh terminal runner state, harvest its aggregate/artifacts into the
/// controller store, and return only when promotion inputs are locally verified.
pub fn reconcile_recipe_attempt_for_continuation(
    recipe: &AgentTaskCookRecipe,
    run_id: &str,
) -> Result<agent_task_lifecycle::AgentTaskRunRecord> {
    let recipe_store = CookRecipeStore::from_current_data_root()?;
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    reconcile_recipe_attempt_for_continuation_in_stores(
        &recipe_store,
        &lifecycle_store,
        recipe,
        run_id,
    )
}

fn reconcile_recipe_attempt_for_continuation_in_stores(
    recipe_store: &CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    recipe: &AgentTaskCookRecipe,
    run_id: &str,
) -> Result<agent_task_lifecycle::AgentTaskRunRecord> {
    if lifecycle_store.record_exists(run_id)? {
        let existing = lifecycle_store.read_record_bounded(run_id)?;
        validate_recipe_attempt_record_in_store(lifecycle_store, recipe, run_id, &existing)?;
        if existing
            .metadata
            .get("cook_finalization")
            .is_some_and(|finalization| !finalization.is_null())
        {
            return Ok(existing);
        }
        if super::cook_promotion::persisted_promotion_for_attempt_in_store(lifecycle_store, run_id)?
            .is_some_and(|promotion| {
                promotion.status
                    != crate::agent_task_promotion::AgentTaskPromotionStatus::VerificationPending
            })
        {
            return Ok(existing);
        }
    }
    let record = super::cook_pre_execution::recover_recipe_attempt_with_stores(
        recipe_store,
        lifecycle_store,
        run_id,
    )?
    .ok_or_else(|| {
        Error::internal_unexpected(
            "Cook recipe unexpectedly disappeared during continuation recovery",
        )
    })?;
    validate_recipe_attempt_record_in_store(lifecycle_store, recipe, run_id, &record)?;
    // Promotion has copied and verified the selected artifact into the
    // controller-owned destination. Once its gates are green, finalization no
    // longer consumes the provider aggregate's artifact transport.
    if super::cook_promotion::persisted_promotion_for_attempt_in_store(lifecycle_store, run_id)?
        .is_some()
    {
        return Ok(record);
    }
    if record.state.is_terminal() {
        if let Ok(aggregate) = lifecycle_store.read_aggregate(run_id) {
            if !agent_task_lifecycle::terminal_artifact_projection_is_verified_in_store(
                lifecycle_store,
                &record,
                &aggregate,
            )? {
                let mut recoverable = record.clone();
                agent_task_lifecycle::record_terminal_artifact_projection_in_store(
                    lifecycle_store,
                    &mut recoverable,
                    &aggregate,
                )?;
            }
        }
    }
    let record = lifecycle_store.read_record(run_id)?;
    if let Some(reason) = agent_task_lifecycle::terminal_artifact_projection_readiness_in_store(
        lifecycle_store,
        run_id,
    )? {
        let continuation = super::cook_recovery_command_with_prefix(
            &super::cook_recovery_command_prefix_for_record(&record),
            &["cook-continue", run_id],
        );
        return Err(Error::validation_invalid_argument(
            "cook_continuation.artifact_projection",
            format!(
                "Cook `{}` attempt `{run_id}` is terminal but its controller-owned artifact projection is not ready: {reason}",
                recipe.cook_id
            ),
            Some(run_id.to_string()),
            Some(vec![format!(
                "Retry `{continuation}` after the runner artifact can be harvested."
            )]),
        )
        .with_retryable(true));
    }
    Ok(record)
}

pub fn preflight_recipe_attempt_for_continuation_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    recipe: &AgentTaskCookRecipe,
    run_id: &str,
) -> Result<(
    agent_task_lifecycle::AgentTaskRunRecord,
    Option<AgentTaskAggregate>,
)> {
    let record = lifecycle_store.read_record_bounded(run_id)?;
    validate_recipe_attempt_record_in_store(lifecycle_store, recipe, run_id, &record)?;
    if record
        .metadata
        .get("cook_finalization")
        .is_some_and(|finalization| !finalization.is_null())
    {
        return Ok((record, None));
    }
    let (record, aggregate) = lifecycle_store.read_record_with_aggregate_bounded(run_id)?;
    validate_recipe_attempt_record_in_store(lifecycle_store, recipe, run_id, &record)?;
    if super::cook_promotion::persisted_promotion_from_record(run_id, record.clone())?.is_some() {
        return Ok((record, aggregate));
    }
    if let Some(reason) =
        agent_task_lifecycle::terminal_artifact_projection_readiness_for_observation_readonly_in_store(
            lifecycle_store,
            &record,
            aggregate.as_ref(),
        )?
    {
        let continuation = super::cook_recovery_command_with_prefix(
            &super::cook_recovery_command_prefix_for_record(&record),
            &["cook-continue", run_id],
        );
        return Err(Error::validation_invalid_argument(
            "cook_continuation.artifact_projection",
            format!(
                "Cook `{}` attempt `{run_id}` is terminal but its controller-owned artifact projection is not ready: {reason}",
                recipe.cook_id
            ),
            Some(run_id.to_string()),
            Some(vec![format!(
                "Retry `{continuation}` after the runner artifact can be harvested."
            )]),
        )
        .with_retryable(true));
    }
    Ok((record, aggregate))
}

pub fn enqueue_terminal_continuation(cook_id: &str, run_id: &str) -> Result<bool> {
    default_store()?.enqueue_terminal_continuation(cook_id, run_id)
}

const LIFECYCLE_CONTINUATION_KEY: &str = "cook_continuation";
const LIFECYCLE_CONTINUATION_SCHEMA: &str = "homeboy/agent-task-cook-continuation-lifecycle/v1";
const MAX_CONTINUATION_RETRIES: u32 = 3;

#[derive(Clone, Copy)]
enum LifecycleContinuationTransition<'a> {
    Complete,
    Retry,
    RetryWithDiagnostic(&'a str),
    Fail(&'a str),
}

fn lifecycle_continuation(record: &agent_task_lifecycle::AgentTaskRunRecord) -> Option<&Value> {
    record.metadata.get(LIFECYCLE_CONTINUATION_KEY)
}

fn continuation_from_lifecycle(value: &Value) -> Result<AgentTaskCookContinuation> {
    let schema = value.get("schema").and_then(Value::as_str);
    let cook_id = value.get("cook_id").and_then(Value::as_str);
    let run_id = value.get("run_id").and_then(Value::as_str);
    let key = value.get("key").and_then(Value::as_str);
    if schema != Some(LIFECYCLE_CONTINUATION_SCHEMA)
        || cook_id.is_none_or(str::is_empty)
        || run_id.is_none_or(str::is_empty)
        || key != Some(format!("{}:{}", cook_id.unwrap(), run_id.unwrap()).as_str())
    {
        return Err(Error::validation_invalid_argument(
            "cook_continuation",
            "malformed lifecycle-owned Cook continuation",
            None,
            None,
        ));
    }
    Ok(AgentTaskCookContinuation {
        schema: CONTINUATION_SCHEMA.to_string(),
        key: key.expect("validated key").to_string(),
        cook_id: cook_id.expect("validated cook id").to_string(),
        run_id: run_id.expect("validated run id").to_string(),
        retries: value.get("retries").and_then(Value::as_u64).unwrap_or(0) as u32,
    })
}

fn lifecycle_continuation_state(value: Option<&Value>) -> CookContinuationState {
    match value
        .and_then(|value| value.get("state"))
        .and_then(Value::as_str)
    {
        Some("pending") => CookContinuationState::Pending,
        Some("claimed") => CookContinuationState::Claimed,
        Some("failed") => CookContinuationState::Failed,
        Some("completed") => CookContinuationState::Completed,
        _ => CookContinuationState::Absent,
    }
}

fn enqueue_lifecycle_continuation(
    recipe_store: &CookRecipeStore,
    continuation: &AgentTaskCookContinuation,
    rearm_failed: bool,
) -> Result<bool> {
    validate_continuation(continuation)?;
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_data_root(recipe_store.data_root());
    let recipe = recipe_store.load_recipe(&continuation.cook_id)?;
    if !recipe
        .attempts
        .iter()
        .any(|attempt| attempt.run_id == continuation.run_id)
    {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.attempts",
            "terminal run is not declared by the durable cook recipe",
            Some(continuation.run_id.clone()),
            None,
        ));
    }
    reconstruct_missing_attempt_record(&lifecycle_store, &recipe, &continuation.run_id)?;
    let mut enqueued = false;
    lifecycle_store.mutate_record(&continuation.run_id, |record| {
        let state = lifecycle_continuation_state(lifecycle_continuation(record));
        if state != CookContinuationState::Absent
            && !(rearm_failed && state == CookContinuationState::Failed)
        {
            return false;
        }
        let now = agent_task_lifecycle::now_timestamp();
        let previous_generation = lifecycle_continuation(record)
            .and_then(|value| value.get("generation"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let mut value = serde_json::json!({
            "schema": LIFECYCLE_CONTINUATION_SCHEMA,
            "key": continuation.key,
            "cook_id": continuation.cook_id,
            "run_id": continuation.run_id,
            "state": "pending",
            "retries": if rearm_failed { 0 } else { continuation.retries },
            "generation": if rearm_failed { previous_generation.saturating_add(1) } else { previous_generation },
            "enqueued_at": now,
        });
        if rearm_failed {
            value["rearmed_at"] = serde_json::json!(agent_task_lifecycle::now_timestamp());
            record
                .ensure_metadata_object()
                .remove("cook_controller_failure");
        }
        record
            .ensure_metadata_object()
            .insert(LIFECYCLE_CONTINUATION_KEY.to_string(), value);
        record.updated_at = Some(agent_task_lifecycle::now_timestamp());
        enqueued = true;
        true
    })?;
    Ok(enqueued)
}

fn claim_lifecycle_continuation_for(
    recipe_store: &CookRecipeStore,
    cook_id: &str,
    run_id: &str,
) -> Result<Option<ClaimedCookContinuation>> {
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_data_root(recipe_store.data_root());
    let recipe = recipe_store.load_recipe(cook_id)?;
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
    claim_lifecycle_record(&lifecycle_store, &recipe_store.data_root(), cook_id, run_id)
}

/// Terminalize a continuation whose durable value cannot be decoded, so the
/// queue makes progress and the failure stays inspectable on the record.
fn terminalize_malformed_continuation(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<()> {
    lifecycle_store.mutate_record(run_id, |record| {
        let Some(mut value) = lifecycle_continuation(record).cloned() else {
            return false;
        };
        value["state"] = serde_json::json!("failed");
        value["diagnostic"] = serde_json::json!("malformed durable continuation");
        record
            .ensure_metadata_object()
            .insert(LIFECYCLE_CONTINUATION_KEY.to_string(), value);
        record.updated_at = Some(agent_task_lifecycle::now_timestamp());
        true
    })?;
    Ok(())
}

/// Rebuild the authoritative lifecycle record for a Cook attempt whose record
/// is missing, using the durable recipe that already owns that attempt's plan.
///
/// Continuation state lives on the record, so a pruned record would otherwise
/// strand recoverable work. The recipe is reconstruction authority rather than
/// invention: the plan, cook identity, and attempt number all come from it, and
/// the record is stamped so status and discovery show it as reconstructed.
fn reconstruct_missing_attempt_record(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    recipe: &AgentTaskCookRecipe,
    run_id: &str,
) -> Result<bool> {
    if lifecycle_store.record_exists(run_id)? {
        return Ok(false);
    }
    let attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "terminal run is not declared by the durable cook recipe",
                Some(run_id.to_string()),
                None,
            )
        })?;
    agent_task_lifecycle::persist_controller_plan_in_store(lifecycle_store, run_id, &attempt.plan)?;
    let record: agent_task_lifecycle::AgentTaskRunRecord =
        serde_json::from_value(serde_json::json!({
            "schema": "homeboy/agent-task-run/v1",
            "run_id": run_id,
            "plan_id": attempt.plan.plan_id,
            // The attempt is terminal by construction: only a terminal attempt
            // publishes continuation work. Its exact outcome is unknowable from the
            // recipe, so the reconstruction declares the recoverable terminal state
            // the continuation is about to repair.
        "state": "failed",
        "submitted_at": agent_task_lifecycle::now_timestamp(),
        "plan_path": lifecycle_store.controller_plan_path(run_id).display().to_string(),
        // The execution projection must agree with `state`, or record health
        // classifies the reconstruction as a conflicting projection and drops
        // it from every observation scan.
        "lifecycle": {
            "execution": {
                "state": "failed",
                "finished_at": agent_task_lifecycle::now_timestamp(),
                "updated_at": agent_task_lifecycle::now_timestamp(),
            },
        },
            "metadata": {
                "cook_id": recipe.cook_id,
                "cook_attempt": attempt.attempt,
                "reconstructed_from_recipe": {
                    "schema": "homeboy/agent-task-run-record-reconstruction/v1",
                    "cook_id": recipe.cook_id,
                    "attempt": attempt.attempt,
                    "reconstructed_at": agent_task_lifecycle::now_timestamp(),
                    "reason": "continuation_requires_authoritative_record",
                },
            },
        }))
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some(format!("reconstruct lifecycle record for {run_id}")),
            )
        })?;
    lifecycle_store.write_record(&record)?;
    Ok(true)
}

/// A record is claimable when it is pending, or when it is still marked claimed
/// by a process that is gone. This replaces the sidecar reclaim sweep: recovery
/// is decided from the record itself at claim time.
fn lifecycle_continuation_is_claimable(value: Option<&Value>) -> bool {
    match lifecycle_continuation_state(value) {
        CookContinuationState::Pending => true,
        CookContinuationState::Claimed => value
            .and_then(|value| value.get("owner_pid"))
            .and_then(Value::as_u64)
            .is_some_and(|pid| !homeboy_core::process::pid_is_running(pid as u32)),
        _ => false,
    }
}

fn claim_lifecycle_continuation_with_budget(
    recipe_store: &CookRecipeStore,
    budget: usize,
) -> Result<CookContinuationClaim> {
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_data_root(recipe_store.data_root());
    let mut inspected = 0;
    if inspected >= budget {
        return Ok(CookContinuationClaim {
            claim: None,
            inspected,
            limit_reached: true,
        });
    }
    let mut records = lifecycle_store.read_records()?;
    records.sort_by(|left, right| left.run_id.cmp(&right.run_id));
    for record in records {
        if !lifecycle_continuation_is_claimable(lifecycle_continuation(&record)) {
            continue;
        }
        if inspected >= budget {
            return Ok(CookContinuationClaim {
                claim: None,
                inspected,
                limit_reached: true,
            });
        }
        inspected += 1;
        // One undecodable record must never poison the queue for every other
        // Cook. Terminalize it with a diagnostic and keep scanning.
        let continuation = match lifecycle_continuation(&record).map(continuation_from_lifecycle) {
            Some(Ok(continuation)) => continuation,
            _ => {
                terminalize_malformed_continuation(&lifecycle_store, &record.run_id)?;
                continue;
            }
        };
        if let Some(claim) = claim_lifecycle_record(
            &lifecycle_store,
            &recipe_store.data_root(),
            &continuation.cook_id,
            &continuation.run_id,
        )? {
            return Ok(CookContinuationClaim {
                claim: Some(claim),
                inspected,
                limit_reached: false,
            });
        }
    }
    Ok(CookContinuationClaim {
        claim: None,
        inspected,
        limit_reached: false,
    })
}

fn claim_lifecycle_record(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    data_root: &Path,
    cook_id: &str,
    run_id: &str,
) -> Result<Option<ClaimedCookContinuation>> {
    // A Cook another live controller is driving is not claimable: its driver
    // owns every transition, including the one this continuation would make.
    // The continuation stays pending and becomes claimable once that driver
    // exits (#15566).
    if CookRecipeStore::from_data_root(data_root.to_path_buf())
        .foreign_cook_driver(cook_id)?
        .is_some()
    {
        return Ok(None);
    }
    let claim_identity = format!("{}-{}", std::process::id(), Uuid::new_v4());
    let mut claimed = None;
    lifecycle_store.mutate_record(run_id, |record| {
        let Some(value) = lifecycle_continuation(record).cloned() else {
            return false;
        };
        let Ok(continuation) = continuation_from_lifecycle(&value) else {
            return false;
        };
        if continuation.cook_id != cook_id || continuation.run_id != run_id {
            return false;
        }
        let state = lifecycle_continuation_state(Some(&value));
        let dead_claim = state == CookContinuationState::Claimed
            && value
                .get("owner_pid")
                .and_then(Value::as_u64)
                .is_some_and(|pid| !homeboy_core::process::pid_is_running(pid as u32));
        if state != CookContinuationState::Pending && !dead_claim {
            return false;
        }
        let mut value = value;
        value["state"] = serde_json::json!("claimed");
        value["claim_identity"] = serde_json::json!(claim_identity);
        value["owner_pid"] = serde_json::json!(std::process::id());
        value["claimed_at"] = serde_json::json!(agent_task_lifecycle::now_timestamp());
        if dead_claim {
            value["recovered_dead_claim"] = serde_json::json!(true);
        }
        record
            .ensure_metadata_object()
            .insert(LIFECYCLE_CONTINUATION_KEY.to_string(), value);
        record.updated_at = Some(agent_task_lifecycle::now_timestamp());
        claimed = Some(continuation);
        true
    })?;
    Ok(claimed.map(|continuation| ClaimedCookContinuation {
        continuation,
        lifecycle_store: lifecycle_store.clone(),
        claim_identity,
        data_root: data_root.to_path_buf(),
        active: true,
    }))
}

fn transition_lifecycle_continuation(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
    claim_identity: &str,
    transition: LifecycleContinuationTransition<'_>,
) -> Result<()> {
    let mut transitioned = false;
    lifecycle_store.mutate_record(run_id, |record| {
        let Some(mut value) = lifecycle_continuation(record).cloned() else {
            return false;
        };
        if lifecycle_continuation_state(Some(&value)) != CookContinuationState::Claimed
            || value.get("claim_identity").and_then(Value::as_str) != Some(claim_identity)
        {
            return false;
        }
        let now = agent_task_lifecycle::now_timestamp();
        match transition {
            LifecycleContinuationTransition::Complete => {
                value["state"] = serde_json::json!("completed");
                value["completed_at"] = serde_json::json!(now);
            }
            LifecycleContinuationTransition::Retry => {
                let retries = value
                    .get("retries")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .saturating_add(1);
                value["retries"] = serde_json::json!(retries);
                value["generation"] =
                    serde_json::json!(value["generation"].as_u64().unwrap_or(0).saturating_add(1));
                if retries > MAX_CONTINUATION_RETRIES as u64 {
                    value["state"] = serde_json::json!("failed");
                    value["diagnostic"] =
                        serde_json::json!("cook continuation retry budget exhausted");
                } else {
                    value["state"] = serde_json::json!("pending");
                }
            }
            LifecycleContinuationTransition::RetryWithDiagnostic(diagnostic) => {
                let retries = value
                    .get("retries")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .saturating_add(1);
                value["retries"] = serde_json::json!(retries);
                value["generation"] =
                    serde_json::json!(value["generation"].as_u64().unwrap_or(0).saturating_add(1));
                if retries > MAX_CONTINUATION_RETRIES as u64 {
                    value["state"] = serde_json::json!("failed");
                    value["diagnostic"] = serde_json::json!(
                        "cook continuation retry budget exhausted after an abandoned worker"
                    );
                } else {
                    value["state"] = serde_json::json!("pending");
                    value["diagnostic"] = serde_json::json!(diagnostic);
                }
            }
            LifecycleContinuationTransition::Fail(diagnostic) => {
                value["state"] = serde_json::json!("failed");
                value["diagnostic"] = serde_json::json!(diagnostic);
            }
        }
        value
            .as_object_mut()
            .expect("continuation object")
            .remove("owner_pid");
        value
            .as_object_mut()
            .expect("continuation object")
            .remove("claim_identity");
        record
            .ensure_metadata_object()
            .insert(LIFECYCLE_CONTINUATION_KEY.to_string(), value);
        record.updated_at = Some(agent_task_lifecycle::now_timestamp());
        transitioned = true;
        true
    })?;
    if transitioned {
        Ok(())
    } else {
        Err(Error::internal_unexpected(
            "Cook continuation claim is no longer owned by this consumer",
        ))
    }
}

// The ambient `rearm_failed_terminal_continuation()` shim that used to sit
// here is gone. It had no callers and was reachable only through the
// `agent_tasks::lifecycle` facade, which nothing outside the crate used (#7505).

/// [`rearm_failed_terminal_continuation`] against explicitly injected durable
/// roots.
///
/// The rearm and the controller-failure clear are one operation across two
/// store kinds: the queue entry hangs off the recipe store's data root while
/// the cause lives in the lifecycle record. Both are parameters so the caller
/// pairs them; resolving either one here would let a rearm recorded in one home
/// leave the stale cause standing in the other (#7505).
#[cfg(test)]
fn rearm_failed_terminal_continuation_in_store(
    store: &CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    cook_id: &str,
    run_id: &str,
) -> Result<bool> {
    let rearmed = store.enqueue_terminal_continuation_with_recovery(cook_id, run_id, true)?;
    if rearmed && agent_task_lifecycle::run_record_exists_in_store(lifecycle_store, run_id)? {
        agent_task_lifecycle::clear_cook_controller_failure_in_store(lifecycle_store, run_id)?;
    }
    Ok(rearmed)
}

pub fn claim_continuation() -> Result<Option<ClaimedCookContinuation>> {
    Ok(claim_continuation_with_budget(usize::MAX)?.claim)
}

pub struct CookContinuationClaim {
    pub claim: Option<ClaimedCookContinuation>,
    pub inspected: usize,
    pub limit_reached: bool,
}

/// Claim one valid continuation after terminalizing malformed entries within a
/// caller-owned admission budget.
pub fn claim_continuation_with_budget(budget: usize) -> Result<CookContinuationClaim> {
    default_store()?.claim_continuation_with_budget(budget)
}

/// Claim one specific continuation without consuming another Cook's pending
/// lifecycle work. Interactive continuation uses this exact key while workers
/// continue to claim the next available entry.
pub fn claim_continuation_for(
    cook_id: &str,
    run_id: &str,
) -> Result<Option<ClaimedCookContinuation>> {
    default_store()?.claim_continuation_for(cook_id, run_id)
}

/// Continuation state for one Cook attempt from its authoritative lifecycle row.
pub fn continuation_state_in_store(
    store: &CookRecipeStore,
    cook_id: &str,
    run_id: &str,
) -> Result<CookContinuationState> {
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_data_root(store.data_root());
    observe_continuation_state(&lifecycle_store, cook_id, run_id)
}

/// The single continuation reader. Both the state query and the claim preflight
/// resolve through this, so they can never disagree about the same attempt.
fn observe_continuation_state(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    cook_id: &str,
    run_id: &str,
) -> Result<CookContinuationState> {
    if let Ok(record) = lifecycle_store.read_record(run_id) {
        let value = lifecycle_continuation(&record);
        let state = lifecycle_continuation_state(value);
        if state != CookContinuationState::Absent {
            if matches!(
                state,
                CookContinuationState::Pending | CookContinuationState::Claimed
            ) && value
                .and_then(|value| value.get("cook_id"))
                .and_then(Value::as_str)
                != Some(cook_id)
            {
                return Err(Error::validation_invalid_argument(
                    "cook_continuation.cook_id",
                    "continuation belongs to a different Cook",
                    Some(run_id.to_string()),
                    None,
                ));
            }
            // A claim whose owner is gone is recoverable work, not a live claim.
            if state == CookContinuationState::Claimed && lifecycle_continuation_is_claimable(value)
            {
                return Ok(CookContinuationState::Pending);
            }
            return Ok(state);
        }
    }
    Ok(CookContinuationState::Absent)
}

/// Observe whether the exact lifecycle continuation claim that `cook-continue`
/// will make is currently admissible without mutating the record.
pub fn preflight_continuation_claim(
    cook_id: &str,
    run_id: &str,
    rearm: bool,
) -> Result<CookContinuationState> {
    let store = default_store()?;
    preflight_continuation_claim_in_store(&store, cook_id, run_id, rearm)
}

pub fn preflight_continuation_claim_in_store(
    store: &CookRecipeStore,
    cook_id: &str,
    run_id: &str,
    rearm: bool,
) -> Result<CookContinuationState> {
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_data_root(store.data_root());
    if let Some(driver) = store.foreign_cook_driver(cook_id)? {
        return Err(Error::validation_invalid_argument(
            "cook_continuation.claim",
            format!(
                "Cook `{cook_id}` is being driven by another live controller (pid {}); it cannot be continued concurrently",
                if driver.trim().is_empty() { "unknown" } else { driver.trim() }
            ),
            Some(run_id.to_string()),
            Some(vec![format!(
                "Watch it with `homeboy agent-task status {cook_id}`, or retry `homeboy agent-task cook-continue {cook_id} --preflight` after that controller exits."
            )]),
        ));
    }
    let record = lifecycle_store.read_record_bounded(run_id)?;
    if !rearm && record.has_live_pending_local_cook_supervisor(chrono::Utc::now()) {
        return Err(Error::validation_invalid_argument(
            "cook_continuation.claim",
            format!(
                "Cook `{cook_id}` attempt `{run_id}` cannot be claimed while its controller supervisor is active"
            ),
            Some(run_id.to_string()),
            Some(vec![format!(
                "Retry `homeboy agent-task cook-continue {cook_id} --preflight` after the active controller finishes."
            )]),
        ));
    }
    let state = observe_continuation_state(&lifecycle_store, cook_id, run_id)?;
    let admitted = if rearm {
        state == CookContinuationState::Failed
    } else {
        matches!(
            state,
            CookContinuationState::Pending | CookContinuationState::Absent
        )
    };
    if admitted {
        Ok(state)
    } else if rearm {
        Err(rearm_state_error(cook_id, run_id, state))
    } else {
        Err(Error::validation_invalid_argument(
            "cook_continuation.claim",
            format!(
                "Cook `{cook_id}` attempt `{run_id}` cannot be claimed because its continuation is {state:?}"
            ),
            Some(run_id.to_string()),
            None,
        ))
    }
}

/// Rearm one failed continuation and clear its stale controller cause as one
/// recoverable operation. If the lifecycle update fails, ownership is returned
/// to the failed queue entry before the error is exposed.
pub fn claim_continuation_for_recovery_and_clear_failure_in_store(
    store: &CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    cook_id: &str,
    run_id: &str,
) -> Result<Option<ClaimedCookContinuation>> {
    let claim_identity = format!("{}-{}", std::process::id(), Uuid::new_v4());
    let mut claim = None;
    lifecycle_store.mutate_record(run_id, |record| {
        let Some(mut continuation) = lifecycle_continuation(record).cloned() else {
            return false;
        };
        if lifecycle_continuation_state(Some(&continuation)) != CookContinuationState::Failed
            || continuation.get("cook_id").and_then(Value::as_str) != Some(cook_id)
        {
            return false;
        }
        continuation["state"] = serde_json::json!("claimed");
        continuation["retries"] = serde_json::json!(0);
        continuation["claim_identity"] = serde_json::json!(claim_identity);
        continuation["owner_pid"] = serde_json::json!(std::process::id());
        continuation["rearmed_at"] = serde_json::json!(agent_task_lifecycle::now_timestamp());
        record
            .ensure_metadata_object()
            .remove("cook_controller_failure");
        record
            .ensure_metadata_object()
            .insert(LIFECYCLE_CONTINUATION_KEY.to_string(), continuation.clone());
        record.updated_at = Some(agent_task_lifecycle::now_timestamp());
        claim = continuation_from_lifecycle(&continuation).ok();
        true
    })?;
    Ok(claim.map(|continuation| ClaimedCookContinuation {
        continuation,
        lifecycle_store: lifecycle_store.clone(),
        claim_identity,
        data_root: store.data_root(),
        active: true,
    }))
}

fn rearm_state_error(cook_id: &str, run_id: &str, state: CookContinuationState) -> Error {
    Error::validation_invalid_argument(
        "cook_continuation.rearm",
        format!(
            "Cook `{cook_id}` attempt `{run_id}` cannot be rearmed because its continuation is {state:?}; only an existing failed continuation can be rearmed"
        ),
        Some(run_id.to_string()),
        None,
    )
}

pub fn reconstruct_options(recipe: &AgentTaskCookRecipe) -> Result<CookRequest> {
    reconstruct_options_with_dispatcher(recipe, None)
}

pub fn reconstruct_options_with_dispatcher(
    recipe: &AgentTaskCookRecipe,
    attempt_dispatcher: Option<Arc<dyn AgentTaskCookAttemptDispatcher>>,
) -> Result<CookRequest> {
    reconstruct_recipe_options(recipe, attempt_dispatcher, true, true)
}

/// Reconstruct immutable Cook inputs for a lifecycle-validated local placement
/// transition. The caller proves the transition against the durable run record;
/// this only relaxes reconstruction of the superseded Lab transport.
pub fn reconstruct_options_with_local_placement_override(
    recipe: &AgentTaskCookRecipe,
) -> Result<CookRequest> {
    reconstruct_recipe_options(recipe, None, true, false)
}

/// Reconstruct a pre-execution recovery that must revalidate its current
/// transport before provider work can resume.
pub fn reconstruct_options_for_pre_execution_recovery_with_dispatcher(
    recipe: &AgentTaskCookRecipe,
    attempt_dispatcher: Option<Arc<dyn AgentTaskCookAttemptDispatcher>>,
) -> Result<CookRequest> {
    reconstruct_recipe_options(recipe, attempt_dispatcher, false, true)
}

/// Compatibility authority for replacing a controller pin before execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreExecutionRuntimeRecovery {
    RetryablePreExecutionFailure,
    RetryablePreExecutionTransportFailure,
    QueuedRuntimeAdmission,
    ReboundZeroExecution,
}

impl PreExecutionRuntimeRecovery {
    pub(crate) fn provenance(self, previous: Value, current: Value) -> Value {
        serde_json::json!({
            "schema": "homeboy/controller-runtime-pre-execution-recovery/v1",
            "reason": self,
            "compatibility": "unambiguous_zero_provider_execution",
            "previous": previous,
            "current": current,
            "provider_executions_consumed": 0,
            "recovered_at": agent_task_lifecycle::now_timestamp(),
        })
    }
}

pub(crate) fn has_unambiguous_zero_execution(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> bool {
    record.metadata["provider_executions_consumed"].as_u64() == Some(0)
        && record.metadata["provider_run_ids"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && record
            .metadata
            .get("provider_executions")
            .is_none_or(|value| value.as_array().is_some_and(Vec::is_empty))
        && record.provider_handles.is_empty()
        && record.runner_job_id().is_none()
        && record.lab_handoff.as_ref().is_none_or(|handoff| {
            handoff.state != agent_task_lifecycle::AgentTaskLabHandoffState::Accepted
        })
}

pub fn pre_execution_runtime_recovery(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Option<PreExecutionRuntimeRecovery> {
    use agent_task_lifecycle::AgentTaskRunState;
    if !has_unambiguous_zero_execution(record) {
        return None;
    }
    if record.state.is_terminal()
        && super::cook_pre_execution::retryable_pre_execution_failure(record)
    {
        return Some(PreExecutionRuntimeRecovery::RetryablePreExecutionFailure);
    }
    let admission = &record.metadata["cook_runtime_admission"];
    if record.state == AgentTaskRunState::Queued
        && admission["schema"] == "homeboy/cook-runtime-admission/v1"
        && admission["state"] == "queued"
        && admission["fence"].as_u64().is_some_and(|fence| fence > 0)
        && admission["provider_executions_consumed"].as_u64() == Some(0)
    {
        return Some(PreExecutionRuntimeRecovery::QueuedRuntimeAdmission);
    }
    let recovery = &record.metadata["controller_runtime_recovery"];
    let current =
        &record.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY];
    if matches!(
        record.state,
        AgentTaskRunState::Queued | AgentTaskRunState::Running
    ) && recovery["schema"] == "homeboy/controller-runtime-pre-execution-recovery/v1"
        && recovery["provider_executions_consumed"].as_u64() == Some(0)
        && recovery["current"] == *current
        && current["originating"]["build_identity"].as_str()
            == Some(homeboy_core::build_identity::current().display.as_str())
        && matches!(
            recovery["reason"].as_str(),
            Some(
                "queued_runtime_admission"
                    | "retryable_pre_execution_failure"
                    | "retryable_pre_execution_transport_failure"
            )
        )
    {
        return Some(PreExecutionRuntimeRecovery::ReboundZeroExecution);
    }
    None
}

/// Whether immutable inputs may be reconstructed on this controller. Actual
/// pin replacement is separately fenced and rechecks the durable record.
pub fn pre_execution_runtime_recovery_is_eligible(
    _recipe: &AgentTaskCookRecipe,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> bool {
    pre_execution_runtime_recovery(record).is_some()
}

/// One reconstruction boundary for queued dispatch and CLI continuation.
pub fn reconstruct_options_for_record_with_dispatcher(
    recipe: &AgentTaskCookRecipe,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
    dispatcher: Option<Arc<dyn AgentTaskCookAttemptDispatcher>>,
) -> Result<CookRequest> {
    reconstruct_recipe_options(
        recipe,
        dispatcher,
        pre_execution_runtime_recovery(record).is_none()
            && !committed_runtime_rebind_matches_recipe(recipe, record),
        true,
    )
}

/// Once provider execution starts, the admitted pin still owns reconstruction.
/// The historical recipe remains immutable; its exact previous identity must
/// match the committed transition rather than authorizing another rebind.
fn committed_runtime_rebind_matches_recipe(
    recipe: &AgentTaskCookRecipe,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> bool {
    let recovery = &record.metadata["controller_runtime_recovery"];
    let runtime =
        &record.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY];
    recovery["schema"] == "homeboy/controller-runtime-pre-execution-recovery/v1"
        && recovery["reason"] == "queued_runtime_admission"
        && recovery["compatibility"] == "unambiguous_zero_provider_execution"
        && recovery["provider_executions_consumed"].as_u64() == Some(0)
        && recovery["previous"]["originating"]["build_identity"].as_str()
            == Some(recipe.runtime_generation.as_str())
        && recovery["current"] == *runtime
        && runtime["originating"]["build_identity"].as_str()
            == Some(homeboy_core::build_identity::current().display.as_str())
}

/// Reconstruct the policy used to adopt an already-prepared candidate. Adoption
/// never replays provider work, so it may use a validated historical recipe
/// after a controller runtime upgrade.
pub fn reconstruct_adoption_options(recipe: &AgentTaskCookRecipe) -> Result<CookRequest> {
    reconstruct_recipe_options(recipe, None, false, false)
}

/// A historical recipe can only continue through the terminal adoption path.
/// Other states could still dispatch or replay provider work and must retain
/// the runtime pin.
pub fn historical_terminal_continuation_is_eligible(
    recipe: &AgentTaskCookRecipe,
    state: agent_task_lifecycle::AgentTaskRunState,
) -> bool {
    recipe.runtime_generation != homeboy_core::build_identity::current().display
        && matches!(
            state,
            agent_task_lifecycle::AgentTaskRunState::Succeeded
                | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
                | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable
        )
}

/// Resolve a Cook identifier to its latest durable attempt. Candidate selection
/// is promotion authority, not continuation authority: a failed gate-feedback
/// attempt must remain the source of the next retry.
pub fn resolve_cook_continuation_run_id(cook_or_attempt_id: &str) -> Result<String> {
    resolve_cook_continuation_run_id_in_store(
        &default_store()?,
        &agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?,
        cook_or_attempt_id,
    )
}

/// [`resolve_cook_continuation_run_id`] against explicitly injected durable
/// roots.
///
/// The recipe half and the Cook-index half are one resolution across two store
/// kinds, so both are parameters and the caller pairs them. An index read from
/// another home would select an attempt this recipe has never declared — which
/// is exactly the mismatch the guard below reports, only sourced from the wrong
/// installation (#7505).
pub fn resolve_cook_continuation_run_id_in_store(
    store: &CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    cook_or_attempt_id: &str,
) -> Result<String> {
    let recipe = match store.load_recipe(cook_or_attempt_id) {
        // A Cook ID can also be its original attempt's run ID. In that case the
        // Cook alias remains authoritative and must select the current candidate.
        Ok(recipe) => recipe,
        Err(cook_error) => {
            store
                .load_recipe_for_attempt(cook_or_attempt_id)?
                .ok_or(cook_error)?;
            return Ok(cook_or_attempt_id.to_string());
        }
    };
    let run_id =
        if agent_task_lifecycle::cook_index_exists_in_store(lifecycle_store, &recipe.cook_id)? {
            agent_task_lifecycle::cook_index_in_store(lifecycle_store, &recipe.cook_id)?
                .latest_run_id
        } else {
            recipe
                .attempts
                .last()
                .expect("validated recipe has an attempt")
                .run_id
                .clone()
        };
    if !recipe
        .attempts
        .iter()
        .any(|attempt| attempt.run_id == run_id)
    {
        return Err(Error::validation_invalid_argument(
            "cook_id",
            "Cook index latest attempt is not declared by the durable recipe",
            Some(recipe.cook_id.clone()),
            None,
        ));
    }
    Ok(continuation_source_run_id(lifecycle_store, &recipe, run_id))
}

/// A latest attempt that failed before any provider ran, with a failure that
/// is not retryable, has nothing to continue: it produced no candidate and
/// replaying it reproduces the same failure. The Cook still owns earlier
/// attempts. Continue from the newest one that holds a recoverable candidate,
/// so its promotion and feedback loop can dispatch a fresh retry. This is also
/// the only continuation a historical recipe admits after a controller upgrade
/// (`historical_terminal_continuation_is_eligible`), so a Cook stranded by a
/// pre-provider failure no longer becomes unrecoverable on every upgrade.
fn continuation_source_run_id(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    recipe: &AgentTaskCookRecipe,
    latest_run_id: String,
) -> String {
    use agent_task_lifecycle::AgentTaskRunState;
    let Ok(latest) = lifecycle_store.read_record(&latest_run_id) else {
        return latest_run_id;
    };
    let stranded_pre_provider_failure = latest.state == AgentTaskRunState::Failed
        && latest.metadata.get("pre_execution_failure").is_some()
        && has_unambiguous_zero_execution(&latest)
        && !super::cook_pre_execution::retryable_pre_execution_failure(&latest);
    if !stranded_pre_provider_failure {
        return latest_run_id;
    }
    recipe
        .attempts
        .iter()
        .rev()
        .filter(|attempt| attempt.run_id != latest_run_id)
        .find(|attempt| {
            lifecycle_store
                .read_record(&attempt.run_id)
                .is_ok_and(|record| {
                    matches!(
                        record.state,
                        AgentTaskRunState::CandidateRecoverable
                            | AgentTaskRunState::PartialRecoverable
                            | AgentTaskRunState::Succeeded
                    )
                })
        })
        .map(|attempt| attempt.run_id.clone())
        .unwrap_or(latest_run_id)
}

/// Adoption accepts historical policy, but a remediation retry still needs the
/// recipe's exact durable transport reconstructed before it can dispatch.
pub fn reconstruct_adoption_options_with_dispatcher(
    recipe: &AgentTaskCookRecipe,
    attempt_dispatcher: Option<Arc<dyn AgentTaskCookAttemptDispatcher>>,
) -> Result<CookRequest> {
    reconstruct_recipe_options(recipe, attempt_dispatcher, false, true)
}

fn reconstruct_recipe_options(
    recipe: &AgentTaskCookRecipe,
    attempt_dispatcher: Option<Arc<dyn AgentTaskCookAttemptDispatcher>>,
    require_current_runtime: bool,
    require_attempt_dispatcher: bool,
) -> Result<CookRequest> {
    validate_recipe(recipe)?;
    if require_current_runtime
        && recipe.runtime_generation != homeboy_core::build_identity::current().display
    {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.runtime_generation",
            format!(
                "cook recipe is pinned to Homeboy runtime `{}` but this process is `{}`",
                recipe.runtime_generation,
                homeboy_core::build_identity::current().display
            ),
            Some(recipe.cook_id.clone()),
            None,
        )
        .with_retryable(true));
    }
    let initial = recipe
        .attempts
        .first()
        .expect("validated recipe has attempt");
    let mut gates: crate::agent_task_gate::VerifyGateOptions =
        serde_json::from_value(recipe.gate_policy.clone()).map_err(|error| {
            Error::validation_invalid_argument(
                "cook_recipe.gate_policy",
                format!("malformed gate policy: {error}"),
                None,
                None,
            )
        })?;
    if gates.gate_environment.admitted_component_id.is_none() {
        gates.gate_environment.admitted_component_id =
            super::cook::cook_repository_identity_component_id(&initial.plan);
    }
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
    if require_attempt_dispatcher && dispatch_kind == "local" && attempt_dispatcher.is_some() {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.promotion_transport.attempt_dispatch",
            "local cook recipe cannot be reconstructed with an external dispatcher",
            Some(recipe.cook_id.clone()),
            None,
        ));
    }
    if require_attempt_dispatcher && dispatch_kind != "local" && attempt_dispatcher.is_none() {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.promotion_transport.attempt_dispatch",
            format!("cook recipe requires `{dispatch_kind}` attempt dispatcher reconstruction"),
            Some(recipe.cook_id.clone()),
            None,
        ));
    }
    Ok(CookRequest {
        identity: CookIdentity {
            cook_id: recipe.cook_id.clone(),
            initial_run_id: initial.run_id.clone(),
            initial_plan: initial.plan.clone(),
        },
        workspace: CookWorkspace {
            to_worktree: serde_json::from_value(field("to_worktree")?)
                .map_err(recipe_value_error("to_worktree"))?,
            source_worktree_path: serde_json::from_value(field("source_worktree_path")?)
                .map_err(recipe_value_error("source_worktree_path"))?,
            task_base_sha: serde_json::from_value::<Option<String>>(field("task_base_sha")?)
                .map_err(recipe_value_error("task_base_sha"))?
                .or_else(|| {
                    initial
                        .plan
                        .metadata
                        .pointer("/cook_workspace_base/sha")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                }),
            source_refs: recipe.source_refs.clone(),
        },
        provider_transport: CookProviderTransport {
            provider_command,
            provider_invocation,
            attempt_dispatcher,
        },
        gates,
        retry_policy: CookRetryPolicy {
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
        },
        finalization: CookFinalization {
            no_finalize: serde_json::from_value(field("no_finalize")?)
                .map_err(recipe_value_error("no_finalize"))?,
            // Recipes persisted before draft publication existed retain ready-PR behavior.
            draft_pr: recipe
                .finalization
                .get("draft_pr")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(recipe_value_error("draft_pr"))?
                .unwrap_or(false),
            provider_ci: recipe
                .finalization
                .get("provider_ci")
                .filter(|value| !value.is_null())
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(recipe_value_error("provider_ci"))?,
            base: serde_json::from_value(field("base")?).map_err(recipe_value_error("base"))?,
            head: serde_json::from_value(field("head")?).map_err(recipe_value_error("head"))?,
            title: serde_json::from_value(field("title")?).map_err(recipe_value_error("title"))?,
            commit_message: serde_json::from_value(field("commit_message")?)
                .map_err(recipe_value_error("commit_message"))?,
            protected_branches: serde_json::from_value(field("protected_branches")?)
                .map_err(recipe_value_error("protected_branches"))?,
        },
        ai_disclosure: CookAiDisclosure {
            ai_tool: serde_json::from_value(field("ai_tool")?)
                .map_err(recipe_value_error("ai_tool"))?,
            ai_model: serde_json::from_value(field("ai_model")?)
                .map_err(recipe_value_error("ai_model"))?,
            ai_used_for: serde_json::from_value(field("ai_used_for")?)
                .map_err(recipe_value_error("ai_used_for"))?,
        },
        harvest_context: recipe.harvest_context.clone(),
    })
}

pub fn consume_claimed_with_dispatcher(
    claim: ClaimedCookContinuation,
    dispatcher: impl FnOnce(&Value) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
    execute: impl FnOnce(CookRequest) -> Result<i32>,
) -> Result<i32> {
    default_store()?.consume_claimed_with_dispatcher(claim, dispatcher, execute)
}

pub fn consume_claimed_terminal_with_dispatcher(
    claim: ClaimedCookContinuation,
    dispatcher: impl FnOnce(&Value) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
    execute: impl FnOnce(CookRequest) -> Result<i32>,
) -> Result<i32> {
    consume_claimed_with_dispatcher_policy(
        &default_store()?,
        claim,
        dispatcher,
        execute,
        CookMode::ContinueTerminal,
    )
}

/// Complete terminal review-form follow-ups whose accepted outcome was
/// absorbed by a successful historical-run finalization. This prevents the
/// daemon from treating the same provider outcome as independent Cook work.
pub(crate) fn complete_absorbed_review_form_follow_ups(
    store: &CookRecipeStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    finalized_run_id: &str,
) -> Result<()> {
    let recipe = store
        .load_recipe_for_attempt(finalized_run_id)?
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_continuation.finalization",
                "finalized Cook attempt has no durable recipe",
                Some(finalized_run_id.to_string()),
                None,
            )
        })?;
    for attempt in recipe.attempts {
        if attempt.run_id == finalized_run_id {
            continue;
        }
        let Ok(record) = lifecycle_store.read_record(&attempt.run_id) else {
            continue;
        };
        if !record.state.is_terminal()
            || record
                .metadata
                .pointer("/latest_promotion/provenance/cook_follow_up/kind")
                .and_then(Value::as_str)
                != Some("review_form_only")
            || record
                .metadata
                .pointer("/latest_promotion/provenance/cook_follow_up/source_run_id")
                .and_then(Value::as_str)
                != Some(finalized_run_id)
        {
            continue;
        }
        lifecycle_store.mutate_record(&attempt.run_id, |stored| {
            let continuation = stored
                .ensure_metadata_object()
                .get_mut(LIFECYCLE_CONTINUATION_KEY)
                .and_then(Value::as_object_mut);
            let Some(continuation) = continuation else {
                return false;
            };
            if !matches!(
                continuation.get("state").and_then(Value::as_str),
                Some("pending" | "claimed")
            ) {
                return false;
            }
            continuation.insert("state".to_string(), serde_json::json!("completed"));
            continuation.insert(
                "absorbed_by_run_id".to_string(),
                serde_json::json!(finalized_run_id),
            );
            continuation.insert(
                "completed_at".to_string(),
                serde_json::json!(agent_task_lifecycle::now_timestamp()),
            );
            continuation.remove("claim_identity");
            continuation.remove("owner_pid");
            stored.updated_at = Some(agent_task_lifecycle::now_timestamp());
            true
        })?;
    }
    Ok(())
}

fn consume_claimed_with_dispatcher_policy(
    store: &CookRecipeStore,
    claim: ClaimedCookContinuation,
    dispatcher: impl FnOnce(&Value) -> Result<Option<Arc<dyn AgentTaskCookAttemptDispatcher>>>,
    execute: impl FnOnce(CookRequest) -> Result<i32>,
    mode: CookMode,
) -> Result<i32> {
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_data_root(store.data_root());
    if lifecycle_store
        .read_record(&claim.continuation().run_id)?
        .metadata
        .pointer("/cook_continuation/absorbed_by_run_id")
        .and_then(Value::as_str)
        .is_some()
    {
        return Ok(0);
    }
    let recipe = match store.load_recipe(&claim.continuation().cook_id) {
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
    let options = match if matches!(mode, CookMode::ContinueTerminal | CookMode::Adopt) {
        reconstruct_adoption_options_with_dispatcher(&recipe, attempt_dispatcher)
    } else {
        reconstruct_options_with_dispatcher(&recipe, attempt_dispatcher)
    } {
        Ok(options) => options,
        Err(error) if error.retryable == Some(true) => {
            claim.retry()?;
            return Err(error);
        }
        Err(error) => {
            claim.fail(&error.message)?;
            return Err(error);
        }
    };
    let mut options = options;
    if matches!(mode, CookMode::ContinueTerminal | CookMode::Adopt) {
        // A newer coordinator may finish an accepted terminal candidate, but it
        // must never replay provider work under a different runtime generation.
        options.retry_policy.max_attempts = recipe
            .attempts
            .last()
            .map(|attempt| attempt.attempt)
            .unwrap_or(0);
    }
    if agent_task_lifecycle::run_record_exists_in_store(
        &lifecycle_store,
        &claim.continuation().run_id,
    )? {
        if let Err(error) = reconcile_recipe_attempt_for_continuation_in_stores(
            store,
            &lifecycle_store,
            &recipe,
            &claim.continuation().run_id,
        ) {
            if error.retryable == Some(true) {
                claim.retry()?;
            } else {
                claim.fail(&error.message)?;
            }
            return Err(error);
        }
    }
    let record = lifecycle_store.read_record(&claim.continuation().run_id)?;
    if let Some(attempt) = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == claim.continuation().run_id)
    {
        options.identity.initial_run_id = attempt.run_id.clone();
        options.identity.initial_plan = attempt.plan.clone();
    } else {
        let error = Error::validation_invalid_argument(
            "cook_continuation.run_id",
            "claimed continuation is not declared by the durable cook recipe",
            Some(claim.continuation().run_id.clone()),
            None,
        );
        claim.fail(&error.message)?;
        return Err(error);
    }
    // Feedback is acknowledged only at the same durable continuation boundary
    // that owns the next remediation. Candidate mismatch is recorded as stale
    // by the feedback store and cannot leak into another attempt.
    let prepared_feedback = if let Some(candidate) =
        crate::agent_task_feedback::candidate_identity(&record)
    {
        let feedback_store = crate::agent_task_feedback::CookFeedbackStore::new(store.data_root());
        let feedback = feedback_store.prepare_for_candidate(&recipe.cook_id, &candidate)?;
        crate::agent_task_feedback::append_to_plan(
            &record,
            &mut options.identity.initial_plan,
            &feedback,
        )?;
        Some(feedback)
    } else {
        None
    };
    let provider_executions_before = record
        .metadata
        .get("provider_executions_consumed")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let execution = execute(options);
    if let Some(feedback) = prepared_feedback {
        let current_recipe = store.load_recipe(&recipe.cook_id)?;
        if provider_invocation_observed(
            &lifecycle_store,
            &current_recipe,
            &claim.continuation().run_id,
            provider_executions_before,
        )? {
            crate::agent_task_feedback::CookFeedbackStore::new(store.data_root())
                .acknowledge(&recipe.cook_id, &feedback)?;
        }
    }
    match execution {
        Ok(0) => {
            claim.complete()?;
            Ok(0)
        }
        Ok(exit_code) => {
            claim.fail(&format!(
                "Cook continuation exited unsuccessfully with status {exit_code}"
            ))?;
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

fn provider_invocation_observed(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    recipe: &AgentTaskCookRecipe,
    source_run_id: &str,
    source_executions_before: u64,
) -> Result<bool> {
    if lifecycle_store
        .read_record(source_run_id)
        .ok()
        .and_then(|record| {
            record
                .metadata
                .get("provider_executions_consumed")
                .and_then(Value::as_u64)
        })
        .is_some_and(|executions| executions > source_executions_before)
    {
        return Ok(true);
    }
    let source_attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == source_run_id)
        .map(|attempt| attempt.attempt)
        .unwrap_or(0);
    Ok(recipe.attempts.iter().any(|attempt| {
        attempt.attempt > source_attempt
            && lifecycle_store
                .read_record(&attempt.run_id)
                .ok()
                .and_then(|record| {
                    record
                        .metadata
                        .get("provider_executions_consumed")
                        .and_then(Value::as_u64)
                })
                .is_some_and(|executions| executions > 0)
    }))
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
    if continuation.key != format!("{}:{}", continuation.cook_id, continuation.run_id) {
        return Err(Error::validation_invalid_argument(
            "cook_continuation.key",
            "durable continuation key does not match its Cook attempt",
            Some(continuation.key.clone()),
            None,
        ));
    }
    Ok(())
}

/// Re-derive a loaded recipe's sensitive mapping projection from its attempt
/// plans (#15338).
///
/// `sensitive_mappings` is not independent state: it is exactly the sorted,
/// deduplicated `executor.secret_env` names of the attempt plans, which remain
/// the authority. A recipe persisted with a stale projection (an in-place plan
/// rewrite that did not recompute it) would otherwise fail validation on every
/// load and strand its Cook, including `cook-continue`. Explicitly empty names
/// are still rejected by validation.
fn normalize_sensitive_mapping_projection(recipe: &mut AgentTaskCookRecipe) -> Result<()> {
    if recipe
        .sensitive_mappings
        .iter()
        .any(|mapping| mapping.trim().is_empty())
    {
        return Ok(());
    }
    recipe.sensitive_mappings = canonical_sensitive_mappings(&recipe.attempts)?;
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

pub(crate) fn canonical_sensitive_mappings(
    attempts: &[AgentTaskCookRecipeAttempt],
) -> Result<Vec<String>> {
    let mut mappings = attempts
        .iter()
        .map(|attempt| sensitive_mappings(&attempt.plan))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    mappings.sort();
    mappings.dedup();
    Ok(mappings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task::{
        AgentTaskArtifact, AgentTaskExecutor, AgentTaskLimits, AgentTaskOutcome,
        AgentTaskOutcomeStatus, AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace,
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
                &crate::agent_task_service::cook_baseline::DerivedCookBaselineCapability,
            >,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct LabLikeDispatcher;

    impl AgentTaskCookAttemptDispatcher for LabLikeDispatcher {
        fn durable_recipe(&self) -> Result<Value> {
            Ok(serde_json::json!({
                "kind": "lab",
                "queue": "cook-lab",
            }))
        }

        fn dispatch_attempt(
            &self,
            _plan: AgentTaskPlan,
            _run_id: &str,
            _derived_cook_baseline: Option<
                &crate::agent_task_service::cook_baseline::DerivedCookBaselineCapability,
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
            output_declarations: Vec::new(),
            runtime_tools: Vec::new(),
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

    #[test]
    fn provider_ci_recipe_field_accepts_legacy_absent_and_null_values() {
        let mut legacy = recipe();
        legacy
            .finalization
            .as_object_mut()
            .unwrap()
            .remove("provider_ci");
        assert!(reconstruct_options(&legacy)
            .expect("legacy recipe reconstructs")
            .finalization
            .provider_ci
            .is_none());

        let mut explicit_null = recipe();
        explicit_null.finalization["provider_ci"] = Value::Null;
        assert!(reconstruct_options(&explicit_null)
            .expect("null provider CI reconstructs")
            .finalization
            .provider_ci
            .is_none());
    }

    #[test]
    fn provider_ci_recipe_field_reconstructs_configured_identity() {
        let mut configured = recipe();
        configured.finalization["provider_ci"] = serde_json::json!({
            "loop_id": "loop-ci",
            "gate_id": "external-ci",
            "check_id": "homeboy-test",
            "environment_digest": "sha256:environment"
        });
        let provider_ci = reconstruct_options(&configured)
            .expect("configured provider CI reconstructs")
            .finalization
            .provider_ci
            .expect("provider CI policy");
        assert_eq!(provider_ci.loop_id, "loop-ci");
        assert_eq!(provider_ci.gate_id, "external-ci");
        assert_eq!(provider_ci.check_id, "homeboy-test");
        assert_eq!(provider_ci.environment_digest, "sha256:environment");
    }

    fn promotion_value(status: &str, gate_results: Value) -> Value {
        serde_json::json!({
            "schema": "homeboy/agent-task-promotion-report/v1",
            "status": status,
            "source": { "kind": "aggregate", "task_id": "task", "run_id": "run-2" },
            "to_worktree": "corrected-target",
            "target": { "worktree": "corrected-target" },
            "patch_artifact": { "id": "patch", "kind": "patch", "path": "/tmp/patch" },
            "gate_results": gate_results,
            "operator_notification": { "status": "completed", "message": "fixture" }
        })
    }

    #[test]
    fn injected_cook_stores_isolate_recipe_mutations_in_parallel() {
        let left_context = homeboy_core::test_support::HermeticTestContext::new();
        let right_context = homeboy_core::test_support::HermeticTestContext::new();
        let left_store = CookRecipeStore::new(left_context.path_roots());
        let right_store = CookRecipeStore::new(right_context.path_roots());

        let mutate = |store: CookRecipeStore, source_ref: &str, plan_id: &str| {
            let mut options = reconstruct_options(&recipe()).expect("recipe options");
            options.workspace.source_refs = vec![source_ref.to_string()];
            store.persist_initial_recipe(&options)?;
            store.validate_initial_recipe_compatibility(&options)?;

            let mut retry_plan = options.identity.initial_plan;
            retry_plan.plan_id = plan_id.to_string();
            store.record_recipe_attempt("cook", 2, "run-2", &retry_plan)?;
            store.record_recipe_attempt_replacement("cook", "run-2", "run-2-replacement")?;
            Ok::<_, Error>(store)
        };

        let left = std::thread::spawn(move || mutate(left_store, "left-issue", "left-plan"));
        let right = std::thread::spawn(move || mutate(right_store, "right-issue", "right-plan"));
        let left_store = left.join().expect("left thread").expect("left mutations");
        let right_store = right
            .join()
            .expect("right thread")
            .expect("right mutations");

        let left_recipe = left_store.load_recipe("cook").expect("left recipe");
        let right_recipe = right_store.load_recipe("cook").expect("right recipe");
        assert_eq!(left_recipe.source_refs, ["left-issue"]);
        assert_eq!(right_recipe.source_refs, ["right-issue"]);
        assert_eq!(left_recipe.attempts[1].run_id, "run-2");
        assert_eq!(right_recipe.attempts[1].run_id, "run-2");
        assert_eq!(left_recipe.attempts[2].run_id, "run-2-replacement");
        assert_eq!(right_recipe.attempts[2].run_id, "run-2-replacement");
        assert_eq!(left_recipe.attempts[2].plan.plan_id, "left-plan");
        assert_eq!(right_recipe.attempts[2].plan.plan_id, "right-plan");
        assert_ne!(left_store.recipe_root(), right_store.recipe_root());
    }

    /// The rooted recipe + first-attempt fixture.
    ///
    /// Submission goes through `submit_plan_with_runtime_admission` with a stub
    /// admission rather than the ambient `submit_plan`, because that entry point
    /// admits through `homeboy_core::controller_runtime`, whose FIFO admission
    /// queue and content-addressed pin store are machine-global on purpose
    /// (#7505, #12608). A test that no longer repoints HOME would otherwise
    /// enqueue against the real operator runtime store. The consequence is that
    /// the durable run created here carries `{}` for its controller-runtime pin;
    /// no caller below asserts on controller-runtime provenance.
    fn persist_recipe_run(
        store: &CookRecipeStore,
        lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    ) -> (AgentTaskCookRecipe, AgentTaskPlan) {
        let recipe = recipe();
        let plan = recipe.attempts[0].plan.clone();
        store.persist_recipe(&recipe).unwrap();
        lifecycle_store
            .submit_plan_with_runtime_admission(&plan, "run", |_| Ok(serde_json::json!({})))
            .unwrap();
        crate::agent_task_lifecycle::record_cook_attempt_in_store(
            lifecycle_store,
            "cook",
            1,
            "run",
        )
        .unwrap();
        (recipe, plan)
    }

    #[test]
    fn queued_runtime_rebind_preserves_original_admission_and_survives_restart() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let store = CookRecipeStore::from_current_data_root().unwrap();
            let lifecycle =
                agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment().unwrap();
            let mut historical = recipe();
            historical.runtime_generation = "homeboy historical-controller".into();
            store.persist_recipe(&historical).unwrap();
            let plan = &historical.attempts[0].plan;
            lifecycle
                .submit_plan_with_runtime_admission(plan, "run", |_| {
                    Ok(serde_json::json!({"originating": {"build_identity": historical.runtime_generation}}))
                })
                .unwrap();
            agent_task_lifecycle::record_cook_attempt_in_store(&lifecycle, "cook", 1, "run")
                .unwrap();
            assert!(agent_task_lifecycle::defer_cook_runtime_admission_in_store(
                &lifecycle,
                "run",
                serde_json::json!({"operation": "upgrade"})
            )
            .unwrap());
            let before = lifecycle.read_record("run").unwrap();
            assert!(before.metadata.get("retry_of").is_none());
            let before_plan =
                serde_json::to_value(lifecycle.read_controller_plan("run").unwrap()).unwrap();
            let before_index = serde_json::to_value(
                agent_task_lifecycle::cook_index_in_store(&lifecycle, "cook").unwrap(),
            )
            .unwrap();
            assert!(reconstruct_options(&historical).is_err());

            // A crash after manifest admission but before the store commit has
            // no effect on the original queue row; retry uses the same request.
            let failed =
                super::super::cook_pre_execution::rebind_queued_cook_runtime_with_admission(
                    &historical,
                    &lifecycle,
                    "run",
                    |id| {
                        let _manifest =
                            super::super::cook_pre_execution::production_runtime_admission(
                                &lifecycle,
                            )(id)?;
                        Err(Error::internal_unexpected("interrupted before commit"))
                    },
                );
            assert!(failed.is_err());
            assert_eq!(
                serde_json::to_value(lifecycle.read_record("run").unwrap()).unwrap(),
                serde_json::to_value(&before).unwrap()
            );

            let restarted =
                agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment().unwrap();
            let rebound =
                super::super::rebind_queued_cook_runtime_in_store(&historical, &restarted, "run")
                    .unwrap();
            let recovery = &rebound.metadata["controller_runtime_recovery"];
            assert_eq!(recovery["reason"], "queued_runtime_admission");
            assert_eq!(
                recovery["compatibility"],
                "unambiguous_zero_provider_execution"
            );
            assert_eq!(
                recovery["previous"]["originating"]["build_identity"],
                historical.runtime_generation
            );
            assert_eq!(
                recovery["current"]["originating"]["build_identity"],
                homeboy_core::build_identity::current().display
            );
            homeboy_core::controller_runtime::validate(&recovery["current"]).unwrap();
            let mut expected = serde_json::to_value(&before).unwrap();
            let actual = serde_json::to_value(&rebound).unwrap();
            for key in [
                "controller_runtime",
                "controller_identity",
                "controller_runtime_recovery",
            ] {
                expected["metadata"][key] = actual["metadata"][key].clone();
            }
            expected["updated_at"] = actual["updated_at"].clone();
            assert_eq!(
                actual, expected,
                "only runtime provenance changes: workspace, tracker, budget and claims survive"
            );
            assert_eq!(
                serde_json::to_value(restarted.read_controller_plan("run").unwrap()).unwrap(),
                before_plan
            );
            assert_eq!(
                serde_json::to_value(
                    agent_task_lifecycle::cook_index_in_store(&restarted, "cook").unwrap()
                )
                .unwrap(),
                before_index
            );
            assert_eq!(
                serde_json::to_value(store.load_recipe("cook").unwrap()).unwrap(),
                serde_json::to_value(&historical).unwrap()
            );
            let options =
                reconstruct_options_for_record_with_dispatcher(&historical, &rebound, None)
                    .unwrap();
            assert_eq!(options.identity.initial_run_id, "run");
            assert_eq!(
                serde_json::to_value(&options.identity.initial_plan).unwrap(),
                serde_json::to_value(plan).unwrap()
            );
            let expected_gates: crate::agent_task_gate::VerifyGateOptions =
                serde_json::from_value(historical.gate_policy.clone()).unwrap();
            assert_eq!(
                serde_json::to_value(&options.gates).unwrap(),
                serde_json::to_value(expected_gates).unwrap()
            );
            let repeated =
                super::super::cook_pre_execution::rebind_queued_cook_runtime_with_admission(
                    &historical,
                    &restarted,
                    "run",
                    |_| panic!("committed rebind must not readmit"),
                )
                .unwrap();
            assert_eq!(serde_json::to_value(repeated).unwrap(), actual);
            agent_task_lifecycle::mark_running_in_store(&restarted, "run").unwrap();
            let running = restarted.read_record("run").unwrap();
            reconstruct_options_for_record_with_dispatcher(&historical, &running, None).unwrap();
            restarted
                .mutate_record("run", |record| {
                    record.metadata["provider_executions_consumed"] = serde_json::json!(1);
                    record.metadata["provider_run_ids"] = serde_json::json!(["started-provider"]);
                    true
                })
                .unwrap();
            let started = restarted.read_record("run").unwrap();
            assert!(!pre_execution_runtime_recovery_is_eligible(
                &historical,
                &started
            ));
            reconstruct_options_for_record_with_dispatcher(&historical, &started, None)
                .expect("started work reconstructs only on its already-admitted runtime");
            assert!(
                restarted
                    .rebind_queued_cook_runtime("run", recovery["current"].clone())
                    .is_ok(),
                "replayed commit is inert after claim"
            );
        });
    }

    #[test]
    fn queued_runtime_rebind_rechecks_started_accepted_and_ambiguous_records() {
        homeboy_core::test_support::with_isolated_home(|_| {
            for evidence in [
                "consumed",
                "reservation",
                "handle",
                "accepted",
                "job",
                "missing_count",
                "malformed_executions",
            ] {
                let context = homeboy_core::test_support::HermeticTestContext::new();
                let store = CookRecipeStore::new(context.path_roots());
                let lifecycle =
                    agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
                let (mut historical, _) = persist_recipe_run(&store, &lifecycle);
                historical.runtime_generation = "homeboy historical-controller".into();
                assert!(agent_task_lifecycle::defer_cook_runtime_admission_in_store(
                    &lifecycle,
                    "run",
                    Value::Null
                )
                .unwrap());
                let runtime = super::super::cook_pre_execution::production_runtime_admission(
                    &lifecycle,
                )("run")
                .unwrap();
                // Model evidence arriving after the read/admission and before
                // the fenced commit. The store must inspect the fresh row.
                lifecycle.mutate_record("run", |record| {
                    match evidence {
                        "consumed" => record.metadata["provider_executions_consumed"] = serde_json::json!(1),
                        "reservation" => record.metadata["provider_executions"] = serde_json::json!([{"state": "running"}]),
                        "handle" => record.provider_handles.push(serde_json::from_value(serde_json::json!({"task_id": "task", "backend": "test", "provider_run_id": "provider"})).unwrap()),
                        "accepted" => record.lab_handoff = Some(serde_json::from_value(serde_json::json!({"state": "accepted", "authority": "runner_daemon", "runner_id": "lab", "runner_job_id": "job", "accepted_at": "2026-09-30T00:00:00Z"})).unwrap()),
                        "job" => record.metadata["runner_job_id"] = serde_json::json!("job"),
                        "missing_count" => { record.ensure_metadata_object().remove("provider_executions_consumed"); },
                        "malformed_executions" => record.metadata["provider_executions"] = serde_json::json!({}),
                        _ => unreachable!(),
                    }
                    true
                }).unwrap();
                let before = lifecycle.read_record("run").unwrap();
                assert!(
                    !pre_execution_runtime_recovery_is_eligible(&historical, &before),
                    "{evidence}"
                );
                assert!(
                    lifecycle
                        .rebind_queued_cook_runtime("run", runtime)
                        .is_err(),
                    "{evidence}"
                );
                assert!(
                    !agent_task_lifecycle::defer_cook_runtime_admission_in_store(
                        &lifecycle,
                        "run",
                        Value::Null
                    )
                    .unwrap(),
                    "{evidence}"
                );
                assert!(
                    reconstruct_options_for_record_with_dispatcher(&historical, &before, None)
                        .is_err(),
                    "{evidence}"
                );
                assert_eq!(
                    serde_json::to_value(lifecycle.read_record("run").unwrap()).unwrap(),
                    serde_json::to_value(before).unwrap(),
                    "{evidence}: retain exact pin and record"
                );
            }
        });
    }

    /// The store-rooted form of the deleted ambient `consume_next_with`: claim
    /// the next durable continuation from this store and consume it through an
    /// injected normal-cook boundary.
    fn consume_next_from(
        store: &CookRecipeStore,
        execute: impl FnOnce(CookRequest) -> Result<i32>,
    ) -> Result<Option<i32>> {
        let Some(claim) = store.claim_continuation_with_budget(usize::MAX)?.claim else {
            return Ok(None);
        };
        store
            .consume_claimed_with_dispatcher(claim, |_| Ok(None), execute)
            .map(Some)
    }

    /// The store-rooted form of the deleted ambient `claim_continuation`.
    fn claim_next_from(store: &CookRecipeStore) -> Result<Option<ClaimedCookContinuation>> {
        Ok(store.claim_continuation_with_budget(usize::MAX)?.claim)
    }

    /// The two durable stores one hermetic context owns, paired so a test can
    /// never mismatch a recipe root with a lifecycle root.
    fn rooted_stores(
        context: &homeboy_core::test_support::HermeticTestContext,
    ) -> (
        CookRecipeStore,
        agent_task_lifecycle::AgentTaskLifecycleStore,
    ) {
        (
            CookRecipeStore::new(context.path_roots()),
            agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots()),
        )
    }

    #[test]
    fn recipe_attempt_identity_accepts_missing_metadata_and_rejects_disagreement() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let (store, lifecycle_store) = rooted_stores(&context);
        let (recipe, _) = persist_recipe_run(&store, &lifecycle_store);
        let mut record =
            crate::agent_task_lifecycle::status_in_store(&lifecycle_store, "run").unwrap();
        record.ensure_metadata_object().remove("cook_id");

        // `validate_recipe_attempt_record` is exactly this call with the
        // controller plan loaded ambiently, so the plan half is rooted here too.
        validate_recipe_attempt_record_with_controller_plan(
            &recipe,
            "run",
            &record,
            &crate::agent_task_lifecycle::load_controller_plan_in_store(&lifecycle_store, "run")
                .unwrap(),
        )
        .expect("immutable recipe membership resolves legacy mirror");

        record.ensure_metadata_object().insert(
            "cook_id".to_string(),
            Value::String("other-cook".to_string()),
        );
        let error = validate_recipe_attempt_record_with_controller_plan(
            &recipe,
            "run",
            &record,
            &crate::agent_task_lifecycle::load_controller_plan_in_store(&lifecycle_store, "run")
                .unwrap(),
        )
        .unwrap_err();
        assert!(error
            .message
            .contains("expected Cook `cook` attempt 1 run `run`"));
        assert!(error
            .message
            .contains("observed Cook `other-cook` run `run`"));
    }

    #[test]
    fn recipe_attempt_identity_rejects_controller_plan_base_drift() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let (store, lifecycle_store) = rooted_stores(&context);
        let (mut recipe, _) = persist_recipe_run(&store, &lifecycle_store);
        let record = crate::agent_task_lifecycle::status_in_store(&lifecycle_store, "run").unwrap();
        recipe.attempts[0].plan.tasks[0].workspace.base_ref = Some("other-base".to_string());

        let error = validate_recipe_attempt_record_with_controller_plan(
            &recipe,
            "run",
            &record,
            &crate::agent_task_lifecycle::load_controller_plan_in_store(&lifecycle_store, "run")
                .unwrap(),
        )
        .expect_err("controller plan drift must fail closed");

        assert!(error
            .message
            .contains("does not match the immutable recipe"));
    }

    #[test]
    fn continuation_resolution_accepts_cook_and_attempt_identifiers() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let (store, lifecycle_store) = rooted_stores(&context);
        persist_recipe_run(&store, &lifecycle_store);

        assert_eq!(
            resolve_cook_continuation_run_id_in_store(&store, &lifecycle_store, "cook").unwrap(),
            "run"
        );
        assert_eq!(
            resolve_cook_continuation_run_id_in_store(&store, &lifecycle_store, "run").unwrap(),
            "run"
        );
    }

    /// A latest attempt that failed before any provider ran with a
    /// non-retryable failure is skipped: continuation resumes the newest
    /// attempt that holds a recoverable candidate. A retryable pre-provider
    /// failure is still selected so it can be retried.
    #[test]
    fn continuation_skips_a_stranded_pre_provider_retry_for_the_candidate_attempt() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let (store, lifecycle_store) = rooted_stores(&context);
        let (_, plan) = persist_recipe_run(&store, &lifecycle_store);
        lifecycle_store
            .mutate_record("run", |record| {
                record.state = agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable;
                true
            })
            .unwrap();
        record_recipe_attempt_in_store(&store, "cook", 2, "retry", &plan).unwrap();
        lifecycle_store
            .submit_plan_with_runtime_admission(&plan, "retry", |_| Ok(serde_json::json!({})))
            .unwrap();
        crate::agent_task_lifecycle::record_cook_attempt_in_store(
            &lifecycle_store,
            "cook",
            2,
            "retry",
        )
        .unwrap();
        let fail = |retryable: bool| {
            lifecycle_store
                .mutate_record("retry", |record| {
                    record.state = agent_task_lifecycle::AgentTaskRunState::Failed;
                    let metadata = record.ensure_metadata_object();
                    metadata.insert("provider_executions_consumed".into(), serde_json::json!(0));
                    metadata.insert("provider_run_ids".into(), serde_json::json!([]));
                    metadata.insert(
                        "pre_execution_failure".into(),
                        serde_json::json!({
                            "phase": "lab_staging_controller",
                            "retryable": retryable,
                        }),
                    );
                    true
                })
                .unwrap();
        };

        fail(false);
        assert_eq!(
            resolve_cook_continuation_run_id_in_store(&store, &lifecycle_store, "cook").unwrap(),
            "run",
            "a stranded pre-provider retry resumes from the candidate attempt"
        );

        fail(true);
        assert_eq!(
            resolve_cook_continuation_run_id_in_store(&store, &lifecycle_store, "cook").unwrap(),
            "retry",
            "a retryable pre-provider failure is retried"
        );
    }

    #[test]
    fn continuation_resolution_uses_recipe_when_legacy_cook_index_is_missing() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let (store, lifecycle_store) = rooted_stores(&context);
        let recipe = recipe();
        store.persist_recipe(&recipe).unwrap();

        assert_eq!(
            resolve_cook_continuation_run_id_in_store(&store, &lifecycle_store, "cook").unwrap(),
            "run"
        );
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
                task_id: "task".to_string(),
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                ..Default::default()
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

    /// #15338: a recipe persisted with a stale sensitive mapping projection
    /// loads with the projection re-derived from its attempt plans instead of
    /// failing validation and stranding its Cook.
    #[test]
    fn stale_sensitive_mapping_projection_is_rederived_on_load() {
        let temp = tempfile::tempdir().unwrap();
        let store = CookRecipeStore::from_data_root(temp.path().to_path_buf());
        let mut initial = recipe();
        initial.cook_id = "stale-mappings".into();
        store.persist_recipe(&initial).expect("initial recipe");

        // Simulate the old in-place plan rewrite: the plan gains provider
        // secrets but the stored projection is not recomputed.
        let path = store.recipe_path("stale-mappings");
        let mut raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        raw["attempts"][0]["plan"]["tasks"][0]["executor"]["secret_env"] =
            serde_json::json!(["AI_PROVIDER_OPENCODE_OPENAI_ACCESS", "TEST_TOKEN"]);
        std::fs::write(&path, serde_json::to_string(&raw).unwrap()).unwrap();

        let loaded = store
            .load_recipe("stale-mappings")
            .expect("stale projection repaired");
        assert_eq!(
            loaded.sensitive_mappings,
            ["AI_PROVIDER_OPENCODE_OPENAI_ACCESS", "TEST_TOKEN"]
        );
        assert_eq!(
            load_recipe_for_attempt_from(&store.recipe_root(), &loaded.attempts[0].run_id)
                .expect("scan")
                .expect("found")
                .sensitive_mappings,
            loaded.sensitive_mappings
        );
    }

    #[test]
    fn replacing_pre_provider_attempt_recomputes_sensitive_mapping_projection() {
        let temp = tempfile::tempdir().unwrap();
        let store = CookRecipeStore::from_data_root(temp.path().to_path_buf());
        let mut initial = recipe();
        initial.cook_id = "replacement-mappings".into();
        initial.attempts[0].run_id = "source-run".into();
        store.persist_recipe(&initial).expect("initial recipe");

        let mut replacement_plan = initial.attempts[0].plan.clone();
        replacement_plan.tasks[0].executor.secret_env = vec!["NEW_TOKEN".into()];
        let updated = record_recipe_attempt_replacement_in_store_with_plan(
            &store,
            "replacement-mappings",
            "source-run",
            "retry-run",
            &replacement_plan,
        )
        .expect("replacement mapping projection is canonical");
        assert_eq!(updated.sensitive_mappings, ["NEW_TOKEN", "TEST_TOKEN"]);
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
        assert!(options.provider_transport.attempt_dispatcher.is_some());
        assert_eq!(options.gates.verify, Vec::<String>::new());
        assert_eq!(options.workspace.to_worktree, "target");
        assert_eq!(options.finalization.base, "main");
    }

    #[test]
    fn continuation_reconstructs_lab_dispatcher_and_grouped_request_fields() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let store = CookRecipeStore::new(context.path_roots());
        let dispatcher: Arc<dyn AgentTaskCookAttemptDispatcher> = Arc::new(LabLikeDispatcher);
        let mut request = reconstruct_adoption_options(&recipe()).expect("reconstruct fixture");
        request.identity.cook_id = "lab-cook".to_string();
        request.identity.initial_run_id = "lab-run".to_string();
        request.workspace.to_worktree = "fixture@lab".to_string();
        request.workspace.source_worktree_path = Some("/tmp/lab-source".into());
        request.workspace.task_base_sha = Some("lab-base".to_string());
        request.workspace.source_refs = vec!["issue:lab".to_string()];
        request.provider_transport.provider_command = Some("lab-agent".to_string());
        request.provider_transport.attempt_dispatcher = Some(dispatcher.clone());
        request.retry_policy.max_attempts = 2;
        request.finalization.draft_pr = true;
        request.finalization.head = Some("lab-head".to_string());
        request.ai_disclosure.ai_model = Some("lab-model".to_string());

        store
            .persist_initial_recipe(&request)
            .expect("persist Lab dispatcher recipe");
        store
            .enqueue_terminal_continuation("lab-cook", "lab-run")
            .expect("enqueue persisted continuation");
        let claim = store
            .claim_continuation_for("lab-cook", "lab-run")
            .expect("claim continuation")
            .expect("persisted continuation claim");

        let exit_code = store
            .consume_claimed_with_dispatcher(
                claim,
                |durable_recipe| {
                    assert_eq!(durable_recipe["kind"], "lab");
                    assert_eq!(durable_recipe["queue"], "cook-lab");
                    Ok(Some(dispatcher))
                },
                |request| {
                    assert_eq!(
                        request
                            .provider_transport
                            .attempt_dispatcher
                            .expect("injected Lab dispatcher")
                            .durable_recipe()?,
                        serde_json::json!({ "kind": "lab", "queue": "cook-lab" })
                    );
                    assert_eq!(request.identity.cook_id, "lab-cook");
                    assert_eq!(request.identity.initial_run_id, "lab-run");
                    assert_eq!(request.workspace.to_worktree, "fixture@lab");
                    assert_eq!(request.workspace.task_base_sha.as_deref(), Some("lab-base"));
                    assert_eq!(request.workspace.source_refs, ["issue:lab"]);
                    assert_eq!(
                        request.provider_transport.provider_command.as_deref(),
                        Some("lab-agent")
                    );
                    assert_eq!(request.retry_policy.max_attempts, 2);
                    assert!(request.finalization.draft_pr);
                    assert_eq!(request.finalization.head.as_deref(), Some("lab-head"));
                    assert_eq!(request.ai_disclosure.ai_model.as_deref(), Some("lab-model"));
                    Ok(0)
                },
            )
            .expect("consume Lab continuation");

        assert_eq!(exit_code, 0);
    }

    #[test]
    fn request_sections_round_trip_through_the_v1_recipe_without_a_dispatcher() {
        let mut options = reconstruct_adoption_options(&recipe()).expect("reconstruct fixture");
        options.identity.cook_id = "nested-cook".to_string();
        options.identity.initial_run_id = "nested-run".to_string();
        options.workspace.to_worktree = "fixture@nested".to_string();
        options.workspace.source_worktree_path = Some("/tmp/nested".into());
        options.workspace.task_base_sha = Some("base-sha".to_string());
        options.workspace.source_refs = vec!["issue:nested".to_string()];
        options.provider_transport.provider_command = Some("provider".to_string());
        options.retry_policy.max_attempts = 3;
        options.finalization.draft_pr = true;
        options.finalization.head = Some("nested-head".to_string());
        options.finalization.protected_branches = vec!["main".to_string()];
        options.ai_disclosure.ai_model = Some("nested-model".to_string());

        let persisted = initial_recipe(&options).expect("serialize request sections");
        assert_eq!(persisted.schema, COOK_RECIPE_SCHEMA);
        assert_eq!(persisted.cook_id, "nested-cook");
        assert_eq!(persisted.attempts[0].run_id, "nested-run");
        assert_eq!(persisted.source_refs, ["issue:nested"]);
        assert_eq!(
            persisted.promotion_transport["provider_command"],
            "provider"
        );
        assert_eq!(persisted.retry_budget["max_attempts"], 3);
        assert_eq!(persisted.finalization["to_worktree"], "fixture@nested");
        assert_eq!(
            persisted.finalization["source_worktree_path"],
            "/tmp/nested"
        );
        assert_eq!(persisted.finalization["task_base_sha"], "base-sha");
        assert_eq!(persisted.finalization["head"], "nested-head");
        assert_eq!(persisted.finalization["ai_model"], "nested-model");
        assert_eq!(persisted.finalization["draft_pr"], true);

        let reconstructed =
            reconstruct_adoption_options(&persisted).expect("reconstruct persisted recipe");
        assert_eq!(reconstructed.identity.cook_id, options.identity.cook_id);
        assert_eq!(
            reconstructed.workspace.to_worktree,
            options.workspace.to_worktree
        );
        assert_eq!(
            reconstructed.workspace.source_worktree_path,
            options.workspace.source_worktree_path
        );
        assert_eq!(
            reconstructed.workspace.task_base_sha,
            options.workspace.task_base_sha
        );
        assert_eq!(
            reconstructed.workspace.source_refs,
            options.workspace.source_refs
        );
        assert_eq!(
            reconstructed.provider_transport.provider_command,
            options.provider_transport.provider_command
        );
        assert!(reconstructed
            .provider_transport
            .attempt_dispatcher
            .is_none());
        assert_eq!(reconstructed.retry_policy.max_attempts, 3);
        assert_eq!(reconstructed.finalization.base, options.finalization.base);
        assert_eq!(reconstructed.finalization.head, options.finalization.head);
        assert_eq!(
            reconstructed.finalization.draft_pr,
            options.finalization.draft_pr
        );
        assert_eq!(
            reconstructed.finalization.protected_branches,
            options.finalization.protected_branches
        );
        assert_eq!(
            reconstructed.ai_disclosure.ai_model,
            options.ai_disclosure.ai_model
        );
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
    fn adoption_reconstruction_hydrates_a_legacy_plan_task_base() {
        let mut historical = recipe();
        historical.finalization["task_base_sha"] = Value::Null;
        historical.attempts[0].plan.metadata["cook_workspace_base"] =
            serde_json::json!({"sha": "immutable-plan-base"});

        let adoption =
            reconstruct_adoption_options(&historical).expect("reconstruct historical adoption");
        assert_eq!(
            adoption.workspace.task_base_sha.as_deref(),
            Some("immutable-plan-base")
        );

        historical.finalization["task_base_sha"] = serde_json::json!("recipe-base");
        let adoption =
            reconstruct_adoption_options(&historical).expect("reconstruct current adoption");
        assert_eq!(
            adoption.workspace.task_base_sha.as_deref(),
            Some("recipe-base")
        );
    }

    #[test]
    fn provider_replay_keeps_runtime_pin_while_adoption_accepts_historical_policy() {
        let mut historical = recipe();
        historical.runtime_generation = "homeboy 0.291.2+96820fe8cc53".to_string();

        let replay = reconstruct_options(&historical).expect_err("replay requires pinned runtime");
        assert_eq!(replay.details["field"], "cook_recipe.runtime_generation");

        let adoption =
            reconstruct_adoption_options(&historical).expect("adoption reads historical policy");
        assert_eq!(adoption.identity.cook_id, historical.cook_id);
        assert!(adoption.gates.verify.is_empty());
        assert!(adoption.finalization.no_finalize);
        assert!(adoption.provider_transport.attempt_dispatcher.is_none());
    }

    #[test]
    fn historical_terminal_admission_matches_provider_replay_policy() {
        let current = recipe();
        assert!(!historical_terminal_continuation_is_eligible(
            &current,
            agent_task_lifecycle::AgentTaskRunState::Succeeded
        ));
        reconstruct_options(&current).expect("current runtime permits normal continuation");

        let mut historical = current.clone();
        historical.runtime_generation = "homeboy 0.291.2+96820fe8cc53".to_string();
        assert!(historical_terminal_continuation_is_eligible(
            &historical,
            agent_task_lifecycle::AgentTaskRunState::Succeeded
        ));
        reconstruct_adoption_options(&historical)
            .expect("historical terminal continuation avoids provider replay");

        assert!(!historical_terminal_continuation_is_eligible(
            &historical,
            agent_task_lifecycle::AgentTaskRunState::Failed
        ));
        reconstruct_options(&historical)
            .expect_err("historical provider replay retains runtime pin");
    }

    #[test]
    fn terminal_continuation_accepts_historical_runtime_without_provider_replay() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let store = CookRecipeStore::new(context.path_roots());
        let mut historical = recipe();
        historical.runtime_generation = "homeboy 0.291.2+96820fe8cc53".to_string();
        historical.retry_budget["max_attempts"] = serde_json::json!(3);
        store.persist_recipe(&historical).unwrap();
        store.enqueue_terminal_continuation("cook", "run").unwrap();
        let claim = store
            .claim_continuation_for("cook", "run")
            .unwrap()
            .unwrap();

        let mut observed = None;
        // `consume_claimed_terminal_with_dispatcher` is exactly this call with
        // `default_store()` in the store position.
        let exit_code = consume_claimed_with_dispatcher_policy(
            &store,
            claim,
            |_| Ok(None),
            |options| {
                observed = Some(options);
                Ok(0)
            },
            CookMode::ContinueTerminal,
        )
        .unwrap();

        assert_eq!(exit_code, 0);
        let options = observed.expect("terminal continuation reached normal cook boundary");
        assert_eq!(options.retry_policy.max_attempts, 1);
        assert_eq!(options.identity.initial_run_id, "run");
        assert_eq!(
            options.gates,
            serde_json::from_value(recipe().gate_policy).unwrap()
        );
        assert!(
            store
                .claim_continuation_for("cook", "run")
                .unwrap()
                .is_none(),
            "a claimed terminal continuation cannot be consumed twice"
        );
    }

    #[test]
    fn dropped_continuation_claim_is_retried_even_while_owner_process_remains_alive() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let store = CookRecipeStore::new(context.path_roots());
        store.persist_recipe(&recipe()).unwrap();
        store.enqueue_terminal_continuation("cook", "run").unwrap();
        let claim = store
            .claim_continuation_for("cook", "run")
            .unwrap()
            .expect("claim pending continuation");

        // Models a panic or abandoned controller worker in the still-live
        // daemon process: process-PID liveness alone cannot reclaim this claim.
        drop(claim);
        assert_eq!(
            continuation_state_in_store(&store, "cook", "run").unwrap(),
            CookContinuationState::Pending
        );
        assert_eq!(
            agent_task_lifecycle::AgentTaskLifecycleStore::from_data_root(store.data_root())
                .read_record("run")
                .unwrap()
                .metadata["cook_continuation"]["diagnostic"],
            "Cook continuation worker exited before recording a result; retry scheduled"
        );
        let retry_record =
            agent_task_lifecycle::AgentTaskLifecycleStore::from_data_root(store.data_root())
                .read_record("run")
                .unwrap();
        assert_eq!(retry_record.metadata["cook_continuation"]["retries"], 1);
        assert_eq!(retry_record.metadata["cook_continuation"]["generation"], 1);
        assert!(store
            .claim_continuation_for("cook", "run")
            .unwrap()
            .is_some());
    }

    #[test]
    fn feedback_reaches_one_remediation_prompt_before_durable_acknowledgement() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let (store, lifecycle_store) = rooted_stores(&context);
        persist_recipe_run(&store, &lifecycle_store);
        lifecycle_store
            .mutate_record("run", |record| {
                record.metadata["provider_executions_consumed"] = serde_json::json!(1);
                record.metadata["candidate_identity"] = serde_json::json!("candidate-a");
                true
            })
            .unwrap();
        store.enqueue_terminal_continuation("cook", "run").unwrap();
        let feedback_store = crate::agent_task_feedback::CookFeedbackStore::new(
            context.path_roots().data().to_path_buf(),
        );
        feedback_store
            .submit(
                "cook",
                "candidate-a",
                "reviewer",
                "markdown",
                "fix the regression",
                "review-1",
            )
            .unwrap();
        let claim = store
            .claim_continuation_for("cook", "run")
            .unwrap()
            .unwrap();
        let expected_gates: crate::agent_task_gate::VerifyGateOptions =
            serde_json::from_value(store.load_recipe("cook").unwrap().gate_policy).unwrap();

        let mut provider_prompt = None;
        let exit = consume_claimed_with_dispatcher_policy(
            &store,
            claim,
            |_| Ok(None),
            |options| {
                provider_prompt = Some(options.identity.initial_plan.tasks[0].instructions.clone());
                assert_eq!(options.identity.cook_id, "cook");
                assert_eq!(options.workspace.to_worktree, "target");
                assert_eq!(options.gates, expected_gates);
                lifecycle_store
                    .mutate_record("run", |record| {
                        record.metadata["provider_executions_consumed"] = serde_json::json!(2);
                        true
                    })
                    .unwrap();
                Ok(0)
            },
            CookMode::ContinueTerminal,
        )
        .unwrap();
        assert_eq!(exit, 0);
        assert!(provider_prompt
            .expect("provider boundary was invoked")
            .contains("fix the regression"));
        assert_eq!(
            feedback_store.list("cook").unwrap()[0].state,
            crate::agent_task_feedback::FeedbackState::Consumed
        );
    }

    #[test]
    fn verification_only_continuation_keeps_feedback_pending() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let (store, lifecycle_store) = rooted_stores(&context);
        persist_recipe_run(&store, &lifecycle_store);
        lifecycle_store
            .mutate_record("run", |record| {
                record.metadata["provider_executions_consumed"] = serde_json::json!(1);
                record.metadata["candidate_identity"] = serde_json::json!("candidate-a");
                true
            })
            .unwrap();
        store.enqueue_terminal_continuation("cook", "run").unwrap();
        let feedback_store = crate::agent_task_feedback::CookFeedbackStore::new(
            context.path_roots().data().to_path_buf(),
        );
        feedback_store
            .submit(
                "cook",
                "candidate-a",
                "reviewer",
                "markdown",
                "must not be lost",
                "review-2",
            )
            .unwrap();
        let claim = store
            .claim_continuation_for("cook", "run")
            .unwrap()
            .unwrap();
        consume_claimed_with_dispatcher_policy(
            &store,
            claim,
            |_| Ok(None),
            |_| Ok(0),
            CookMode::Resume,
        )
        .unwrap();
        assert_eq!(
            feedback_store.list("cook").unwrap()[0].state,
            crate::agent_task_feedback::FeedbackState::Pending
        );
    }

    #[test]
    fn feedback_remediation_queues_the_existing_terminal_attempt() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let (store, lifecycle_store) = rooted_stores(&context);
        persist_recipe_run(&store, &lifecycle_store);
        lifecycle_store
            .mutate_record("run", |record| {
                record.state = agent_task_lifecycle::AgentTaskRunState::Failed;
                true
            })
            .unwrap();

        assert!(store.enqueue_feedback_remediation("cook", "run").unwrap());
        let claim = store
            .claim_continuation_for("cook", "run")
            .unwrap()
            .expect("same Cook attempt is queued");
        assert_eq!(claim.continuation().cook_id, "cook");
        assert_eq!(claim.continuation().run_id, "run");
    }

    #[test]
    fn continuation_reconstructs_persisted_retry_intent_and_resolved_budget() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let store = CookRecipeStore::new(context.path_roots());
        let mut persisted = recipe();
        persisted.attempts[0].plan.options.execution_budget =
            crate::agent_task_scheduler::AgentTaskExecutionBudget::new(3, 1, 1);
        persisted.retry_budget["max_attempts"] = serde_json::json!(2);
        persisted.retry_budget["execution_budget"] = serde_json::json!({
            "version": 1,
            "deadline_unix_ms": null,
            "max_provider_executions": 3,
            "max_same_provider_retries": 1,
            "max_provider_rotations": 1,
        });
        persisted.retry_budget["policy"] = serde_json::json!({
            "operator_intent": { "max_attempts": 2, "max_provider_executions": null, "max_same_provider_retries": null, "max_provider_rotations": null },
            "resolved": { "max_attempts": 2, "max_provider_executions": 3, "max_same_provider_retries": 1, "max_provider_rotations": 1 },
            "requested": { "max_attempts": 2, "max_provider_executions": 3, "max_same_provider_retries": 1, "max_provider_rotations": 1 },
            "effective": { "max_attempts": 2, "max_provider_executions": 3, "max_same_provider_retries": 1, "max_provider_rotations": 1 },
            "truncated": { "max_provider_rotations": 0 },
        });
        store.persist_recipe(&persisted).unwrap();

        let options = reconstruct_options(&store.load_recipe("cook").unwrap())
            .expect("continuation reconstructs persisted policy");
        assert_eq!(options.retry_policy.max_attempts, 2);
        assert_eq!(
            options
                .identity
                .initial_plan
                .options
                .execution_budget
                .max_provider_executions,
            3
        );
        assert_eq!(
            store.load_recipe("cook").unwrap().retry_budget["policy"]["resolved"]
                ["max_provider_rotations"],
            1
        );
        assert_eq!(
            store.load_recipe("cook").unwrap().retry_budget["policy"]["truncated"]
                ["max_provider_rotations"],
            0
        );
    }

    #[test]
    fn persisted_old_retry_policy_remains_compatible_after_provider_execution() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut persisted = recipe();
            let old_policy = serde_json::json!({
                "operator_intent": { "max_attempts": 1, "max_provider_executions": 1, "max_same_provider_retries": null, "max_provider_rotations": null },
                "resolved": { "max_attempts": 1, "max_provider_executions": 1, "max_same_provider_retries": 0, "max_provider_rotations": 0 },
            });
            persisted.attempts[0].plan.metadata["cook_retry_policy"] = old_policy.clone();
            persisted.retry_budget["policy"] = old_policy;
            default_store().unwrap().persist_recipe(&persisted).unwrap();
            crate::agent_task_lifecycle::submit_plan(&persisted.attempts[0].plan, Some("run"))
                .expect("materialize old provider attempt");
            crate::agent_task_lifecycle::rewrite_record_for_test("run", |record| {
                record.metadata["provider_executions_consumed"] = serde_json::json!(1);
            })
            .expect("record provider execution");

            let mut options = reconstruct_options(&persisted).expect("old recipe reconstructs");
            options.identity.initial_plan.metadata["cook_retry_policy"] = serde_json::json!({
                "operator_intent": { "max_attempts": 1, "max_provider_executions": 1, "max_same_provider_retries": null, "max_provider_rotations": null },
                "resolved": { "max_attempts": 1, "max_provider_executions": 1, "max_same_provider_retries": 0, "max_provider_rotations": 0 },
                "requested": { "max_attempts": 1, "max_provider_executions": 1, "max_same_provider_retries": 0, "max_provider_rotations": 0 },
                "effective": { "max_attempts": 1, "max_provider_executions": 1, "max_same_provider_retries": 0, "max_provider_rotations": 0 },
                "truncated": { "max_provider_rotations": 0 },
            });

            validate_initial_recipe_compatibility(&options)
                .expect("descriptive retry-policy additions preserve the frozen recipe");

            options.identity.initial_plan.metadata["cook_retry_policy"]["resolved"]
                ["max_provider_executions"] = serde_json::json!(2);
            assert!(
                validate_initial_recipe_compatibility(&options).is_err(),
                "a resolved execution budget mutation remains fenced after provider execution"
            );
        });
    }

    #[test]
    fn legacy_recipe_without_retry_policy_remains_compatible_after_provider_execution() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let legacy = recipe();
            default_store().unwrap().persist_recipe(&legacy).unwrap();
            crate::agent_task_lifecycle::submit_plan(&legacy.attempts[0].plan, Some("run"))
                .expect("materialize legacy provider attempt");
            crate::agent_task_lifecycle::rewrite_record_for_test("run", |record| {
                record.metadata["provider_executions_consumed"] = serde_json::json!(1);
            })
            .expect("record legacy provider execution");

            let mut options = reconstruct_options(&legacy).expect("legacy recipe reconstructs");
            options.identity.initial_plan.metadata["cook_retry_policy"] = serde_json::json!({
                "operator_intent": { "max_attempts": 1 },
                "resolved": { "max_attempts": 1, "max_provider_executions": 1, "max_same_provider_retries": 0, "max_provider_rotations": 0 },
            });

            validate_initial_recipe_compatibility(&options)
                .expect("new retry-policy provenance does not reject a frozen legacy recipe");
        });
    }

    #[test]
    fn durable_queue_deduplicates_and_survives_consumer_restart() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let store = CookRecipeStore::new(context.path_roots());
        store.persist_recipe(&recipe()).unwrap();
        assert!(store.enqueue_terminal_continuation("cook", "run").unwrap());
        assert!(!store.enqueue_terminal_continuation("cook", "run").unwrap());
        let first = claim_next_from(&store)
            .unwrap()
            .expect("durable queued continuation");
        assert_eq!(first.continuation().key, "cook:run");
        assert!(claim_next_from(&store).unwrap().is_none());
    }

    #[test]
    fn targeted_claim_does_not_consume_another_cooks_continuation() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let store = CookRecipeStore::new(context.path_roots());
        let mut other = recipe();
        other.cook_id = "other".to_string();
        other.attempts[0].run_id = "other-run".to_string();
        store.persist_recipe(&recipe()).unwrap();
        store.persist_recipe(&other).unwrap();
        store.enqueue_terminal_continuation("cook", "run").unwrap();
        store
            .enqueue_terminal_continuation("other", "other-run")
            .unwrap();

        let claim = store
            .claim_continuation_for("cook", "run")
            .unwrap()
            .expect("targeted continuation");
        assert_eq!(claim.continuation().key, "cook:run");
        claim.complete().unwrap();
        assert_eq!(
            claim_next_from(&store)
                .unwrap()
                .expect("other continuation remains pending")
                .continuation()
                .key,
            "other:other-run"
        );
    }

    #[test]
    fn lifecycle_continuation_cannot_be_observed_under_another_cook() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let store = CookRecipeStore::new(context.path_roots());
        store.persist_recipe(&recipe()).unwrap();
        store.enqueue_terminal_continuation("cook", "run").unwrap();

        let error = continuation_state_in_store(&store, "other-cook", "run").unwrap_err();
        assert!(error.message.contains("different Cook"));
        assert_eq!(
            continuation_state_in_store(&store, "cook", "run").unwrap(),
            CookContinuationState::Pending
        );
    }

    #[test]
    fn old_sidecar_cannot_resurrect_a_missing_lifecycle_continuation() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let (store, lifecycle_store) = rooted_stores(&context);
        store.persist_recipe(&recipe()).unwrap();
        store.enqueue_terminal_continuation("cook", "run").unwrap();
        lifecycle_store
            .mutate_record("run", |record| {
                record
                    .ensure_metadata_object()
                    .remove(LIFECYCLE_CONTINUATION_KEY);
                true
            })
            .unwrap();
        let old_queue = store.data_root().join("agent-task-cook-continuations");
        fs::create_dir_all(&old_queue).unwrap();
        fs::write(
            old_queue.join("abandoned.pending"),
            serde_json::to_vec(&AgentTaskCookContinuation {
                schema: CONTINUATION_SCHEMA.to_string(),
                key: "cook:run".to_string(),
                cook_id: "cook".to_string(),
                run_id: "run".to_string(),
                retries: 0,
            })
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            continuation_state_in_store(&store, "cook", "run").unwrap(),
            CookContinuationState::Absent
        );
        assert!(store
            .claim_continuation_for("cook", "run")
            .unwrap()
            .is_none());
        assert!(
            old_queue.join("abandoned.pending").exists(),
            "observation never imports or rewrites obsolete state"
        );
    }

    #[test]
    fn consumer_reconstructs_options_once_and_completed_work_never_replays() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let store = CookRecipeStore::new(context.path_roots());
        store.persist_recipe(&recipe()).unwrap();
        store.enqueue_terminal_continuation("cook", "run").unwrap();
        let mut observed = None;
        assert_eq!(
            consume_next_from(&store, |options| {
                observed = Some(options);
                Ok(0)
            })
            .unwrap(),
            Some(0)
        );
        let options = observed.expect("normal cook hook received options");
        assert_eq!(options.identity.cook_id, "cook");
        assert_eq!(options.identity.initial_run_id, "run");
        assert!(!store.enqueue_terminal_continuation("cook", "run").unwrap());
        assert!(
            consume_next_from(&store, |_| panic!("completed continuation replayed"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn explicit_recovery_rearms_failed_continuation_but_not_completed_work() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let (store, lifecycle_store) = rooted_stores(&context);
        let recipe = recipe();
        store.persist_recipe(&recipe).unwrap();
        lifecycle_store
            .submit_plan_with_runtime_admission(&recipe.attempts[0].plan, "run", |_| {
                Ok(serde_json::json!({}))
            })
            .expect("materialize run record");
        store.enqueue_terminal_continuation("cook", "run").unwrap();
        claim_next_from(&store)
            .unwrap()
            .unwrap()
            .fail("wrong controller runtime")
            .unwrap();

        assert_eq!(
            continuation_state_in_store(&store, "cook", "run").unwrap(),
            CookContinuationState::Failed
        );
        assert!(!store.enqueue_terminal_continuation("cook", "run").unwrap());
        agent_task_lifecycle::record_cook_controller_failure_in_store(
            &lifecycle_store,
            "run",
            &serde_json::json!({ "code": "controller.failure", "message": "stale" }),
        )
        .expect("persist controller failure");
        assert!(rearm_failed_terminal_continuation_in_store(
            &store,
            &lifecycle_store,
            "cook",
            "run"
        )
        .unwrap());
        assert_eq!(
            continuation_state_in_store(&store, "cook", "run").unwrap(),
            CookContinuationState::Pending
        );
        assert!(
            agent_task_lifecycle::exact_record_in_store(&lifecycle_store, "run")
                .expect("read rearmed record")
                .metadata
                .get("cook_controller_failure")
                .is_none(),
            "a durable rearm clears the stale controller cause before later terminal phases"
        );
        let claim = store
            .claim_continuation_for("cook", "run")
            .unwrap()
            .expect("explicit recovery rearmed the failed continuation");
        assert_eq!(
            continuation_state_in_store(&store, "cook", "run").unwrap(),
            CookContinuationState::Claimed
        );
        assert_eq!(claim.continuation().retries, 0);
        claim.complete().unwrap();

        assert_eq!(
            continuation_state_in_store(&store, "cook", "run").unwrap(),
            CookContinuationState::Completed
        );
        assert!(!rearm_failed_terminal_continuation_in_store(
            &store,
            &lifecycle_store,
            "cook",
            "run"
        )
        .unwrap());
    }

    #[test]
    fn continuation_preflight_observes_claim_and_rearm_without_renaming_queue_state() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let store = CookRecipeStore::new(context.path_roots());
        store.persist_recipe(&recipe()).unwrap();
        store.enqueue_terminal_continuation("cook", "run").unwrap();

        assert_eq!(
            preflight_continuation_claim_in_store(&store, "cook", "run", false).unwrap(),
            CookContinuationState::Pending
        );
        assert_eq!(
            continuation_state_in_store(&store, "cook", "run").unwrap(),
            CookContinuationState::Pending
        );
        claim_next_from(&store)
            .unwrap()
            .unwrap()
            .fail("fixture failure")
            .unwrap();
        assert_eq!(
            preflight_continuation_claim_in_store(&store, "cook", "run", true).unwrap(),
            CookContinuationState::Failed
        );
        assert_eq!(
            continuation_state_in_store(&store, "cook", "run").unwrap(),
            CookContinuationState::Failed
        );
    }

    #[test]
    fn continuation_preflight_rejects_a_live_local_cook_supervisor() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let store = CookRecipeStore::new(context.path_roots());
        let lifecycle_store =
            agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
        store.persist_recipe(&recipe()).unwrap();
        lifecycle_store
            .submit_plan_with_runtime_admission(&recipe().attempts[0].plan, "run", |_| {
                Ok(serde_json::json!({}))
            })
            .unwrap();
        store.enqueue_terminal_continuation("cook", "run").unwrap();
        let now = chrono::Utc::now();
        lifecycle_store
            .mutate_record("run", |record| {
                record.metadata["cook_id"] = serde_json::json!("cook");
                record.metadata["local_cook_supervisor"] = serde_json::json!({
                    "state": "supervising",
                    "cook_id": "cook",
                    "pinned_run_id": "run",
                    "lease_started_at": now.to_rfc3339(),
                    "lease_expires_at": (now + chrono::Duration::seconds(30)).to_rfc3339(),
                });
                true
            })
            .unwrap();

        let error = preflight_continuation_claim_in_store(&store, "cook", "run", false)
            .expect_err("active supervisor must retain continuation ownership");
        assert!(error.message.contains("controller supervisor is active"));
        assert_eq!(
            continuation_state_in_store(&store, "cook", "run").unwrap(),
            CookContinuationState::Pending
        );
    }

    #[test]
    fn finalization_receipt_bypasses_stale_promotion_during_continuation_reconciliation() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let (store, lifecycle_store) = rooted_stores(&context);
        let recipe = recipe();
        store.persist_recipe(&recipe).unwrap();
        let mut record = lifecycle_store
            .submit_plan_with_runtime_admission(&recipe.attempts[0].plan, "run", |_| {
                Ok(serde_json::json!({}))
            })
            .unwrap();
        record.state = agent_task_lifecycle::AgentTaskRunState::Succeeded;
        record.metadata["latest_promotion"] = serde_json::json!({ "status": "applied" });
        record.metadata["cook_finalization"] = serde_json::json!({ "status": "review_ready" });
        lifecycle_store.write_record(&record).unwrap();

        let reconciled = reconcile_recipe_attempt_for_continuation_in_stores(
            &store,
            &lifecycle_store,
            &recipe,
            "run",
        )
        .expect("execution reconciliation honors finalization receipt");
        assert_eq!(
            reconciled.metadata["cook_finalization"]["status"],
            "review_ready"
        );
        let (preflight, aggregate) =
            preflight_recipe_attempt_for_continuation_in_store(&lifecycle_store, &recipe, "run")
                .expect("read-only reconciliation honors finalization receipt");
        assert_eq!(
            preflight.metadata["cook_finalization"]["status"],
            "review_ready"
        );
        assert!(aggregate.is_none());
    }

    #[test]
    fn continuation_projection_preflight_renders_the_injected_placement() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let (store, lifecycle_store) = rooted_stores(&context);
        let (recipe, plan) = persist_recipe_run(&store, &lifecycle_store);
        lifecycle_store
            .mutate_record("run", |record| {
                let identity = homeboy_lab_runner_contract::ExecutionPlacementIdentity {
                    repository: "fixture".to_string(),
                    workspace: "fixture".to_string(),
                    task: "task".to_string(),
                    candidate: None,
                    base: None,
                };
                record.metadata["execution_placement_decision"] = serde_json::to_value(
                    homeboy_lab_runner_contract::ExecutionPlacementDecision::controller_local(
                        "fixture",
                        "v1",
                        identity,
                        homeboy_lab_runner_contract::Placement::Local,
                    ),
                )
                .unwrap();
                true
            })
            .unwrap();
        let mut aggregate = succeeded_aggregate(&plan);
        aggregate.outcomes[0].artifacts.push(AgentTaskArtifact {
            id: "unprojected-patch".to_string(),
            kind: "patch".to_string(),
            ..Default::default()
        });
        agent_task_lifecycle::record_run_aggregate_in_store(
            &lifecycle_store,
            "run",
            &plan,
            &aggregate,
        )
        .unwrap();

        let reconciling = reconcile_recipe_attempt_for_continuation_in_stores(
            &store,
            &lifecycle_store,
            &recipe,
            "run",
        )
        .expect_err("unprojected patch blocks continuation reconciliation");
        let observing =
            preflight_recipe_attempt_for_continuation_in_store(&lifecycle_store, &recipe, "run")
                .expect_err("unprojected patch blocks continuation observation");

        for error in [reconciling, observing] {
            assert!(
                error.details["tried"]
                    .as_array()
                    .is_some_and(|tried| tried.iter().any(|remediation| remediation
                        .as_str()
                        .is_some_and(|remediation| remediation
                            .contains("homeboy --placement local agent-task cook-continue run")))),
                "{:?}",
                error.details
            );
        }
    }

    /// A claim whose owner died is recoverable work. Recovery is decided from
    /// the record at claim time, so no sweep has to rename queue entries first.
    #[test]
    fn dead_claim_is_observed_pending_and_reclaimed() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let (store, lifecycle_store) = rooted_stores(&context);
        store.persist_recipe(&recipe()).unwrap();
        store.enqueue_terminal_continuation("cook", "run").unwrap();
        let claim = store
            .claim_continuation_for("cook", "run")
            .unwrap()
            .expect("targeted continuation");
        assert_eq!(
            continuation_state_in_store(&store, "cook", "run").unwrap(),
            CookContinuationState::Claimed
        );
        drop(claim);

        lifecycle_store
            .mutate_record("run", |record| {
                let mut value = lifecycle_continuation(record)
                    .cloned()
                    .expect("continuation");
                value["owner_pid"] = serde_json::json!(u32::MAX);
                record
                    .ensure_metadata_object()
                    .insert(LIFECYCLE_CONTINUATION_KEY.to_string(), value);
                true
            })
            .unwrap();

        assert_eq!(
            continuation_state_in_store(&store, "cook", "run").unwrap(),
            CookContinuationState::Pending
        );
        let reclaimed = claim_next_from(&store)
            .unwrap()
            .expect("dead claim is reclaimable");
        assert_eq!(reclaimed.continuation().key, "cook:run");
    }

    /// Continuation state lives on the run record, so a Cook attempt whose
    /// record is missing is reconstructed from the recipe that owns its plan
    /// rather than stranding recoverable work.
    #[test]
    fn recipe_reconstructs_a_missing_run_record_and_keeps_recovery_working() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let (store, lifecycle_store) = rooted_stores(&context);
        store.persist_recipe(&recipe()).unwrap();
        assert!(
            !agent_task_lifecycle::run_record_exists_in_store(&lifecycle_store, "run").unwrap()
        );

        store.enqueue_terminal_continuation("cook", "run").unwrap();

        let record = lifecycle_store
            .read_record("run")
            .expect("reconstructed record");
        assert_eq!(
            record.metadata["reconstructed_from_recipe"]["cook_id"],
            "cook"
        );
        assert_eq!(record.metadata["cook_id"], "cook");

        let claim = store
            .claim_continuation_for("cook", "run")
            .unwrap()
            .expect("targeted continuation");
        assert_eq!(
            store
                .consume_claimed_with_dispatcher(claim, |_| Ok(None), |_| Ok(7))
                .unwrap(),
            7
        );
        assert_eq!(
            continuation_state_in_store(&store, "cook", "run").unwrap(),
            CookContinuationState::Failed
        );
        let failed = lifecycle_store.read_record("run").expect("failed record");
        assert!(lifecycle_continuation(&failed)
            .and_then(|value| value.get("diagnostic"))
            .and_then(Value::as_str)
            .expect("failure diagnostic")
            .contains("status 7"));

        assert!(rearm_failed_terminal_continuation_in_store(
            &store,
            &lifecycle_store,
            "cook",
            "run"
        )
        .unwrap());
        assert_eq!(
            continuation_state_in_store(&store, "cook", "run").unwrap(),
            CookContinuationState::Pending
        );
    }

    #[test]
    fn runtime_mismatch_keeps_explicitly_rearmed_continuation_pending() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let (store, lifecycle_store) = rooted_stores(&context);
        let mut historical = recipe();
        historical.runtime_generation = "homeboy 0.291.2+96820fe8cc53".to_string();
        store.persist_recipe(&historical).unwrap();
        store.enqueue_terminal_continuation("cook", "run").unwrap();
        claim_next_from(&store)
            .unwrap()
            .unwrap()
            .fail("wrong controller runtime")
            .unwrap();

        rearm_failed_terminal_continuation_in_store(&store, &lifecycle_store, "cook", "run")
            .unwrap();
        let claim = store
            .claim_continuation_for("cook", "run")
            .unwrap()
            .unwrap();
        let error = store
            .consume_claimed_with_dispatcher(claim, |_| Ok(None), |_| Ok(0))
            .unwrap_err();

        assert_eq!(error.retryable, Some(true));
        assert!(store
            .claim_continuation_for("cook", "run")
            .unwrap()
            .is_some());
    }

    /// A record carrying an undecodable continuation is inert: it is neither
    /// claimable nor reported as live work.
    #[test]
    fn malformed_lifecycle_continuation_is_not_claimable() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let (store, lifecycle_store) = rooted_stores(&context);
        store.persist_recipe(&recipe()).unwrap();
        store.enqueue_terminal_continuation("cook", "run").unwrap();
        lifecycle_store
            .mutate_record("run", |record| {
                record.ensure_metadata_object().insert(
                    LIFECYCLE_CONTINUATION_KEY.to_string(),
                    serde_json::json!({ "state": "pending", "schema": "bogus" }),
                );
                true
            })
            .unwrap();

        assert!(claim_next_from(&store).unwrap().is_none());
        assert!(store
            .claim_continuation_for("cook", "run")
            .unwrap()
            .is_none());
        // The scan terminalizes it rather than failing, so one bad record can
        // never stall every other Cook's continuation.
        assert_eq!(
            continuation_state_in_store(&store, "cook", "run").unwrap(),
            CookContinuationState::Failed
        );
    }

    /// Stays on `with_isolated_home` (#7505). This is the only test here that
    /// both materializes a lifecycle record for `run` and then consumes the
    /// continuation, so it is the only one whose behavior actually changes when
    /// the home is not repointed: `consume_claimed_with_dispatcher_policy` gates
    /// `reconcile_recipe_attempt_for_continuation` on the *ambient*
    /// `agent_task_lifecycle::run_record_exists`. Rooted, that predicate would
    /// read an installation with no `run` record, quietly skipping the
    /// reconciliation branch this test currently exercises — the assertions
    /// would still pass, over less code. Closing this means rooting
    /// `consume_claimed_with_dispatcher_policy`'s lifecycle reach, which is its
    /// own slice.
    #[test]
    fn status_only_enqueues_and_never_invokes_the_consumer() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let store = default_store().expect("Cook store");
            let lifecycle_store =
                agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
                    .expect("lifecycle store");
            let (_, plan) = persist_recipe_run(&store, &lifecycle_store);
            crate::agent_task_lifecycle::rewrite_record_for_test("run", |record| {
                record.ensure_metadata_object().remove("cook_id");
            })
            .unwrap();
            let aggregate = succeeded_aggregate(&plan);
            crate::agent_task_lifecycle::record_run_aggregate("run", &plan, &aggregate).unwrap();
            let executions = AtomicUsize::new(0);

            crate::agent_task_lifecycle::reconcile_status("run").unwrap();

            let record = crate::agent_task_lifecycle::reconcile_status("run").unwrap();
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
                consume_next_from(&store, |_| {
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
    fn status_exposes_failed_artifact_projection_as_a_continuation_phase() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let (store, lifecycle_store) = rooted_stores(&context);
        let (_, plan) = persist_recipe_run(&store, &lifecycle_store);
        // `with_isolated_home` handed the body its home tempdir; the hermetic
        // context's root is that same directory.
        let patch = context.root().join("candidate.patch");
        fs::write(&patch, b"candidate").expect("write candidate patch");
        let mut aggregate = succeeded_aggregate(&plan);
        aggregate.outcomes[0].artifacts.push(AgentTaskArtifact {
            schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: "patch".to_string(),
            kind: "patch".to_string(),
            name: None,
            label: None,
            role: None,
            semantic_key: None,
            path: Some(patch.display().to_string()),
            url: None,
            mime: Some("text/x-patch".to_string()),
            size_bytes: Some(9),
            sha256: Some("0".repeat(64)),
            metadata: serde_json::json!({ "executor_artifact_finalized": true }),
        });
        crate::agent_task_lifecycle::record_run_aggregate_in_store(
            &lifecycle_store,
            "run",
            &plan,
            &aggregate,
        )
        .unwrap();

        let record = crate::agent_task_lifecycle::reconcile_status_in_store(
            &lifecycle_store,
            "run",
            crate::agent_task_lifecycle::AgentTaskStatusOptions::default(),
            false,
        )
        .unwrap()
        .record;

        assert_eq!(
            record.metadata["cook_continuation_scheduler"]["status"],
            "artifact_projection_failed"
        );
        assert_eq!(
            record.metadata["cook_continuation_scheduler"]["phase"],
            "artifact_projection"
        );
        assert_eq!(
            record.metadata["cook_continuation_scheduler"]["repair_command"],
            "homeboy agent-task status run"
        );
        assert!(claim_next_from(&store).unwrap().is_none());
    }

    #[test]
    fn concurrent_consumers_execute_one_continuation_once() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let store = CookRecipeStore::new(context.path_roots());
        store.persist_recipe(&recipe()).unwrap();
        store.enqueue_terminal_continuation("cook", "run").unwrap();
        let executions = AtomicUsize::new(0);

        let results = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                consume_next_from(&store, |_| {
                    executions.fetch_add(1, Ordering::SeqCst);
                    Ok(0)
                })
                .unwrap()
            });
            let second = scope.spawn(|| {
                consume_next_from(&store, |_| {
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
    }

    #[test]
    fn retry_attempts_are_appended_idempotently_before_scheduling() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let store = CookRecipeStore::new(context.path_roots());
        store.persist_recipe(&recipe()).unwrap();
        let mut retry_plan = recipe().attempts[0].plan.clone();
        retry_plan.plan_id = "retry-plan".to_string();

        store
            .record_recipe_attempt("cook", 2, "run-2", &retry_plan)
            .unwrap();
        store
            .record_recipe_attempt("cook", 2, "run-2", &retry_plan)
            .unwrap();

        let persisted = store.load_recipe("cook").unwrap();
        assert_eq!(persisted.attempts.len(), 2);
        assert_eq!(persisted.attempts[1].run_id, "run-2");
        let resumed = reconstruct_options(&persisted).unwrap();
        assert_eq!(store.persist_initial_recipe(&resumed).unwrap(), persisted);
        assert!(store
            .enqueue_terminal_continuation("cook", "run-2")
            .unwrap());

        let mut conflicting = retry_plan;
        conflicting.plan_id = "different".to_string();
        assert!(store
            .record_recipe_attempt("cook", 2, "run-2", &conflicting)
            .is_err());
    }

    #[test]
    fn initial_admission_reuses_a_recipe_that_captured_base_during_a_peer_admission() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let store = CookRecipeStore::new(context.path_roots());
        let options = reconstruct_options(&recipe()).expect("canonical options");
        store
            .persist_initial_recipe(&options)
            .expect("persist initial recipe");

        let mut captured = store.load_recipe("cook").expect("load initial recipe");
        captured.finalization["task_base_sha"] = Value::String("captured-base".to_string());
        store
            .persist_recipe(&captured)
            .expect("persist captured base");

        let materialization = store
            .persist_initial_recipe_with_outcome(&options)
            .expect("peer admission adopts captured base");
        assert!(!materialization.created);
        assert_eq!(materialization.recipe, captured);
        assert_eq!(materialization.recipe.attempts.len(), 1);
    }

    #[test]
    fn recipe_compatibility_preflight_is_read_only_and_allows_pre_provider_corrections() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let store = CookRecipeStore::new(context.path_roots());
        let options = reconstruct_options(&recipe()).expect("canonical options");

        store
            .validate_initial_recipe_compatibility(&options)
            .expect("new recipe is compatible");
        assert!(!store.recipe_exists(&options.identity.cook_id));

        store
            .persist_initial_recipe(&options)
            .expect("persist canonical recipe");
        store
            .validate_initial_recipe_compatibility(&options)
            .expect("exact replay is compatible");

        let mut changed = options;
        changed.finalization.title = "different title".to_string();
        store
            .validate_initial_recipe_compatibility(&changed)
            .expect("pre-provider correction is compatible");
        assert_eq!(
            store
                .load_recipe(&changed.identity.cook_id)
                .unwrap()
                .finalization["title"],
            "title"
        );
    }

    #[test]
    fn recipe_correction_transitions_from_pre_provider_supersede_to_frozen_boundaries() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut original = reconstruct_options(&recipe()).expect("canonical options");
            original.identity.initial_run_id = "run-1".to_string();
            persist_initial_recipe(&original).expect("persist original recipe");
            crate::agent_task_lifecycle::submit_plan(
                &original.identity.initial_plan,
                Some("run-1"),
            )
            .expect("materialize pre-provider attempt");
            crate::agent_task_lifecycle::record_pre_execution_failure(
                "run-1",
                &original.identity.initial_plan,
                "provider_missing",
                &Error::internal_unexpected("provider executable is unavailable"),
            )
            .expect("record pre-provider failure");

            let mut corrected = original.clone();
            corrected.identity.initial_run_id = "run-2".to_string();
            corrected.workspace.to_worktree = "corrected-target".to_string();
            corrected.finalization.title = "Corrected Cook".to_string();
            let superseded =
                persist_initial_recipe(&corrected).expect("supersede pre-provider recipe");
            assert_eq!(superseded.attempts.len(), 2);
            assert_eq!(superseded.attempts[0].run_id, "run-1");
            assert_eq!(superseded.attempts[1].run_id, "run-2");
            let history = default_store()
                .unwrap()
                .recipe_path("cook")
                .parent()
                .unwrap()
                .join("recipe-history");
            let archived: AgentTaskCookRecipe = serde_json::from_slice(
                &fs::read(history.join("0001.recipe.json")).expect("immutable recipe history"),
            )
            .expect("parse archived recipe");
            assert_eq!(archived.attempts[0].run_id, "run-1");
            let diff: Value = serde_json::from_slice(
                &fs::read(history.join("0001.supersession.json")).expect("supersession diff"),
            )
            .expect("parse supersession diff");
            assert_eq!(diff["replacement_attempt_run_id"], "run-2");
            assert!(diff["changed_fields"]
                .as_array()
                .expect("changed fields")
                .iter()
                .any(|field| field == "finalization.to_worktree"));

            crate::agent_task_lifecycle::submit_plan(
                &corrected.identity.initial_plan,
                Some("run-2"),
            )
            .expect("materialize provider attempt");
            crate::agent_task_lifecycle::rewrite_record_for_test("run-2", |record| {
                record.metadata["provider_executions_consumed"] = serde_json::json!(1);
            })
            .expect("record provider execution");
            let existing = load_recipe("cook").expect("load corrected recipe");
            let mut source_corrected = existing.clone();
            source_corrected.source_refs = vec!["corrected-source".to_string()];
            let error = ensure_correction_is_safe(
                &existing,
                &source_corrected,
                &recipe_mismatch_fields(&existing, &source_corrected),
            )
            .expect_err("authenticated candidate freezes source inputs");
            assert!(error.message.contains("candidate execution"));
            assert!(error.message.contains("source_refs"));

            crate::agent_task_lifecycle::rewrite_record_for_test("run-2", |record| {
                record.metadata["latest_promotion"] =
                    promotion_value("applied", serde_json::json!([]));
            })
            .expect("record applied promotion");
            let mut destination_corrected = existing.clone();
            destination_corrected.finalization["to_worktree"] = serde_json::json!("other-target");
            let error = ensure_correction_is_safe(
                &existing,
                &destination_corrected,
                &recipe_mismatch_fields(&existing, &destination_corrected),
            )
            .expect_err("applied promotion freezes destination inputs");
            assert!(error.message.contains("promotion execution"));
            assert!(error.message.contains("finalization.to_worktree"));

            crate::agent_task_lifecycle::rewrite_record_for_test("run-2", |record| {
                record.metadata["latest_promotion"]["gate_results"] = serde_json::json!([]);
            })
            .expect("record empty gate projection");
            let mut gate_corrected = existing.clone();
            gate_corrected.gate_policy["verify"] = serde_json::json!(["corrected gate"]);
            ensure_correction_is_safe(
                &existing,
                &gate_corrected,
                &recipe_mismatch_fields(&existing, &gate_corrected),
            )
            .expect("empty gate arrays do not freeze gate policy");
            crate::agent_task_lifecycle::rewrite_record_for_test("run-2", |record| {
                record.metadata["latest_promotion"]["deterministic_gates"] = serde_json::json!([
                    { "id": "gate", "status": "succeeded", "command": [], "exit_code": 0 }
                ]);
            })
            .expect("record gate execution");
            let error = ensure_correction_is_safe(
                &existing,
                &gate_corrected,
                &recipe_mismatch_fields(&existing, &gate_corrected),
            )
            .expect_err("executed gates freeze gate policy");
            assert!(error.message.contains("gate execution"));
            assert!(error.message.contains("gate_policy"));

            crate::agent_task_lifecycle::rewrite_record_for_test("run-2", |record| {
                record.metadata["cook_finalization"] =
                    serde_json::json!({ "status": "review_ready" });
            })
            .expect("record finalization");
            let mut finalization_corrected = existing;
            finalization_corrected.finalization["title"] = serde_json::json!("Different title");
            let error = ensure_correction_is_safe(
                &load_recipe("cook").expect("load finalized recipe"),
                &finalization_corrected,
                &recipe_mismatch_fields(
                    &load_recipe("cook").expect("load finalized recipe"),
                    &finalization_corrected,
                ),
            )
            .expect_err("finalization freezes finalization policy");
            assert!(error.message.contains("finalization execution"));
            assert!(error.message.contains("finalization"));
        });
    }

    #[test]
    fn rooted_recipe_validation_uses_its_own_lifecycle_store() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let context = homeboy_core::test_support::HermeticTestContext::new();
            let (store, lifecycle_store) = rooted_stores(&context);
            let original = recipe();
            store.persist_recipe(&original).expect("persist recipe");
            crate::agent_task_lifecycle::submit_plan_in_store(
                &lifecycle_store,
                &original.attempts[0].plan,
                Some("run"),
            )
            .expect("materialize provider attempt");
            crate::agent_task_lifecycle::rewrite_record_for_test_in_store(
                &lifecycle_store,
                "run",
                |record| {
                    record.metadata["provider_executions_consumed"] = serde_json::json!(1);
                },
            )
            .expect("record provider execution");

            let mut corrected = reconstruct_options(&original).expect("reconstruct options");
            corrected
                .workspace
                .source_refs
                .push("corrected-source".to_string());

            assert!(
                store
                    .validate_initial_recipe_compatibility(&corrected)
                    .is_err(),
                "the recipe-rooted check must see the provider execution outside the ambient home"
            );
        });
    }

    #[test]
    fn supersession_intent_recovers_before_and_after_active_recipe_replacement() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let store = CookRecipeStore::new(context.path_roots());
        let previous = recipe();
        let mut replacement = previous.clone();
        replacement.finalization["title"] = serde_json::json!("corrected");
        replacement.attempts.push(AgentTaskCookRecipeAttempt {
            attempt: 2,
            run_id: "run-2".to_string(),
            plan: previous.attempts[0].plan.clone(),
        });
        let supersession = AgentTaskCookRecipeSupersession {
            schema: SUPERSESSION_SCHEMA.to_string(),
            previous: previous.clone(),
            replacement: replacement.clone(),
            changed_fields: vec!["finalization".to_string()],
        };

        store
            .persist_recipe(&previous)
            .expect("persist previous recipe");
        write_supersession(&store, &supersession)
            .expect("persist intent before active replacement");
        recover_pending_supersession(&store, "cook").expect("recover interrupted replacement");
        assert_eq!(store.load_recipe("cook").unwrap(), replacement);
        assert!(!store.supersession_path("cook").exists());

        store
            .persist_recipe(&previous)
            .expect("reset active recipe");
        write_supersession(&store, &supersession).expect("persist retry intent");
        store
            .persist_recipe(&replacement)
            .expect("simulate interruption after active replacement");
        recover_pending_supersession(&store, "cook").expect("idempotently finish archived history");
        assert_eq!(store.load_recipe("cook").unwrap(), replacement);
        assert!(!store.supersession_path("cook").exists());
        assert_eq!(
            serde_json::from_slice::<AgentTaskCookRecipe>(
                &fs::read(
                    store
                        .recipe_path("cook")
                        .parent()
                        .unwrap()
                        .join("recipe-history/0001.recipe.json"),
                )
                .unwrap(),
            )
            .unwrap(),
            previous
        );
    }

    /// Rooted in explicit stores rather than a mutated process environment
    /// (#7505). This assertion is an absence — a non-applied promotion leaves
    /// destination inputs correctable — so it is only meaningful if the freeze
    /// check actually observes the promotion recorded here. The write and the
    /// read therefore name the *same* lifecycle store: a rooted spelling that
    /// split them would satisfy the `expect` by never seeing the promotion at
    /// all, which is the false pass this test has to avoid.
    #[test]
    fn non_applied_promotion_reports_do_not_freeze_destination_inputs() {
        let recipe_context = homeboy_core::test_support::HermeticTestContext::new();
        let lifecycle_context = homeboy_core::test_support::HermeticTestContext::new();
        let recipe_store = CookRecipeStore::new(recipe_context.path_roots());
        let lifecycle_store = crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(
            lifecycle_context.path_roots(),
        );

        let recipe = recipe();
        recipe_store
            .persist_recipe(&recipe)
            .expect("persist recipe");
        crate::agent_task_lifecycle::submit_plan_in_store(
            &lifecycle_store,
            &recipe.attempts[0].plan,
            Some("run"),
        )
        .expect("materialize attempt");
        let mut corrected = recipe.clone();
        corrected.finalization["to_worktree"] = serde_json::json!("other-target");
        for status in ["dry_run", "no_changes", "no_changes_gate_failed"] {
            crate::agent_task_lifecycle::rewrite_record_for_test_in_store(
                &lifecycle_store,
                "run",
                |record| {
                    record.metadata["latest_promotion"] =
                        promotion_value(status, serde_json::json!([]));
                },
            )
            .expect("record non-applied promotion");
            // Same store the promotion was just written to. If this read used a
            // different home the loop would pass without ever seeing it.
            ensure_correction_is_safe_in_store(
                &lifecycle_store,
                &recipe,
                &corrected,
                &recipe_mismatch_fields(&recipe, &corrected),
            )
            .expect("non-applied promotion leaves destination correctable");
        }
    }

    #[test]
    fn malformed_recipe_is_reported_by_status_without_executing_work() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let (store, lifecycle_store) = rooted_stores(&context);
        let (_, plan) = persist_recipe_run(&store, &lifecycle_store);
        let aggregate = succeeded_aggregate(&plan);
        crate::agent_task_lifecycle::record_run_aggregate_in_store(
            &lifecycle_store,
            "run",
            &plan,
            &aggregate,
        )
        .unwrap();
        fs::write(store.recipe_path("cook"), b"not json").unwrap();

        let record = crate::agent_task_lifecycle::reconcile_status_in_store(
            &lifecycle_store,
            "run",
            crate::agent_task_lifecycle::AgentTaskStatusOptions::default(),
            false,
        )
        .unwrap()
        .record;

        assert_eq!(
            record.metadata["cook_continuation_scheduler"]["status"],
            "failed"
        );
        assert!(record.metadata["cook_continuation_scheduler"]["message"]
            .as_str()
            .unwrap()
            .contains("malformed durable cook recipe"));
        assert!(claim_next_from(&store).unwrap().is_none());
    }
}

/// One controller drives a Cook at a time (#15566).
#[cfg(test)]
mod cook_driver_tests {
    use super::CookRecipeStore;
    use fs4::fs_std::FileExt;
    use std::fs::OpenOptions;

    /// A lock held through a separate open file stands in for another
    /// process: advisory locks conflict per open file description.
    fn hold_as_other_process(store: &CookRecipeStore, cook_id: &str) -> std::fs::File {
        let path = store.driver_lock_path(cook_id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "4242").unwrap();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(file.try_lock_exclusive().unwrap());
        file
    }

    #[test]
    fn a_cook_driven_elsewhere_cannot_be_driven_here() {
        let temp = tempfile::tempdir().unwrap();
        let store = CookRecipeStore::from_data_root(temp.path().to_path_buf());
        let other = hold_as_other_process(&store, "cook");

        assert!(store.try_acquire_cook_driver("cook").unwrap().is_none());
        assert_eq!(
            store.foreign_cook_driver("cook").unwrap().as_deref(),
            Some("4242")
        );

        drop(other);
        assert!(store.foreign_cook_driver("cook").unwrap().is_none());
        assert!(store.try_acquire_cook_driver("cook").unwrap().is_some());
    }

    #[test]
    fn driving_is_reentrant_within_a_process_and_released_on_last_drop() {
        let temp = tempfile::tempdir().unwrap();
        let store = CookRecipeStore::from_data_root(temp.path().to_path_buf());

        let outer = store.try_acquire_cook_driver("cook").unwrap().unwrap();
        let inner = store.try_acquire_cook_driver("cook").unwrap().unwrap();
        // This process is the driver, so it is not foreign to itself.
        assert!(store.foreign_cook_driver("cook").unwrap().is_none());

        let path = store.driver_lock_path("cook");
        let probe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(
            !probe.try_lock_exclusive().unwrap(),
            "still held after one acquisition"
        );

        drop(inner);
        assert!(
            !probe.try_lock_exclusive().unwrap(),
            "held until the last guard drops"
        );
        drop(outer);
        assert!(
            probe.try_lock_exclusive().unwrap(),
            "released after the last guard"
        );
    }

    /// Daemon jobs run as threads, so another thread is another controller.
    #[test]
    fn another_thread_in_this_process_is_another_controller() {
        let temp = tempfile::tempdir().unwrap();
        let store = CookRecipeStore::from_data_root(temp.path().to_path_buf());
        let _driver = store.try_acquire_cook_driver("cook").unwrap().unwrap();

        let other = store.clone();
        let (acquired, foreign) = std::thread::spawn(move || {
            (
                other.try_acquire_cook_driver("cook").unwrap().is_some(),
                other.foreign_cook_driver("cook").unwrap(),
            )
        })
        .join()
        .unwrap();
        assert!(!acquired);
        assert_eq!(foreign, Some(std::process::id().to_string()));
    }

    #[test]
    fn distinct_cooks_are_driven_independently() {
        let temp = tempfile::tempdir().unwrap();
        let store = CookRecipeStore::from_data_root(temp.path().to_path_buf());
        let _other = hold_as_other_process(&store, "cook-a");
        assert!(store.try_acquire_cook_driver("cook-b").unwrap().is_some());
    }

    #[test]
    fn an_undriven_cook_has_no_foreign_driver() {
        let temp = tempfile::tempdir().unwrap();
        let store = CookRecipeStore::from_data_root(temp.path().to_path_buf());
        assert!(store
            .foreign_cook_driver("never-started")
            .unwrap()
            .is_none());
    }
}
