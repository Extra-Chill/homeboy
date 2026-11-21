//! Per-attempt storage the daemon reclaims on its maintenance cadence (#18420).
//!
//! Every runner attempt leaves a path behind in one of three categories. Each
//! already has a cleanup implementation and a retention policy in
//! `homeboy cleanup`; this pass runs that same planner over exactly those
//! categories with `--apply` and default retention, inside the daemon's
//! bounded maintenance deadline. Nothing here widens a delete predicate: every
//! category keeps its own guards (live owner, active run, unpushed commits,
//! retention windows).

use std::cell::Cell;
use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime};

use homeboy::core::cleanup;
use homeboy::core::daemon::storage_maintenance::{
    register_storage_maintenance_provider, StorageMaintenanceProvider,
};
use serde_json::Value;

use super::{
    cleanup_category_metadata, is_bounded_continuation, run_cleanup_category,
    save_remote_workspace_cursors, CleanupArgs, CleanupCategoryArg, CleanupInventoryCategory,
    CleanupPassBudget, CleanupSweep,
};

/// The categories the daemon reclaims, in the order one pass visits them.
///
/// Remote workspaces run last: they are the only category that talks to
/// another host, so a slow runner can only spend the budget nothing after it
/// needs.
///
/// Task worktrees are operator work: like automatic retention, the daemon
/// never reclaims them unattended.
pub(super) const DAEMON_MAINTENANCE_CATEGORIES: [CleanupCategoryArg; 3] = [
    CleanupCategoryArg::ControllerScratch,
    CleanupCategoryArg::RuntimeTmp,
    CleanupCategoryArg::RemoteLabWorkspaces,
];

/// Whether `category` (an inventory category name) is reclaimed by the daemon
/// maintenance pass, so `homeboy cleanup` must not tell operators to reclaim
/// it by hand.
pub(super) fn reclaimed_by_daemon(category: &str) -> bool {
    DAEMON_MAINTENANCE_CATEGORIES
        .iter()
        .any(|arg| cleanup_category_metadata(*arg).category == category)
}

thread_local! {
    static CATEGORIES_IN_PROCESS: Cell<bool> = const { Cell::new(false) };
}

/// Whether this thread is running a daemon maintenance pass.
///
/// The pass budget is far below what a spawned category process needs to
/// start and be reaped, so the pass runs its categories in process; each one
/// stops at its own deadline instead of being killed with its lock held.
pub(super) fn categories_run_in_process() -> bool {
    CATEGORIES_IN_PROCESS.with(Cell::get)
}

struct InProcessCategories;

impl InProcessCategories {
    fn enter() -> Self {
        CATEGORIES_IN_PROCESS.with(|flag| flag.set(true));
        Self
    }
}

impl Drop for InProcessCategories {
    fn drop(&mut self) {
        CATEGORIES_IN_PROCESS.with(|flag| flag.set(false));
    }
}

/// Register per-attempt reclamation with the daemon's maintenance pass.
pub(crate) fn register_storage_maintenance() {
    register_storage_maintenance_provider(Box::new(PerAttemptStorageMaintenance));
}

struct PerAttemptStorageMaintenance;

impl StorageMaintenanceProvider for PerAttemptStorageMaintenance {
    fn reclaim(&self, deadline: Instant) -> homeboy::core::Result<()> {
        let failures = reclaim_per_attempt_storage(deadline)?;
        if failures.is_empty() {
            return Ok(());
        }
        Err(homeboy::core::Error::internal_unexpected(format!(
            "per-attempt storage maintenance: {}",
            failures.join("; ")
        )))
    }
}

/// Run one bounded pass over [`DAEMON_MAINTENANCE_CATEGORIES`].
///
/// Each category gets an equal share of what remains of the pass, so one
/// category with a large backlog cannot starve the others; time a category
/// leaves unused flows to the ones after it. Each resumes from the cursor the
/// previous pass persisted for it. A category that fails is reported and the
/// pass moves on to the next one; the returned lines name each failure for the
/// observed daemon pass.
fn reclaim_per_attempt_storage(deadline: Instant) -> homeboy::core::Result<Vec<String>> {
    let pass_deadline = SystemTime::now()
        .checked_add(deadline.saturating_duration_since(Instant::now()))
        .unwrap_or_else(SystemTime::now);
    let mut failures = Vec::new();
    // The pass lease admits one pass at a time and holds the cursors; it is
    // released when this function returns, which the deadline bounds.
    let mut retention =
        match homeboy::core::observation::ObservationStore::open_initialized_for_lifecycle()
            .and_then(|store| {
                cleanup::begin_automatic_retention_pass(
                    store,
                    cleanup::AutomaticRetentionKind::DaemonStorage,
                    SystemTime::now(),
                )
            }) {
            Ok(Some(pass)) => Some(pass),
            Ok(None) => return Ok(failures),
            // Reclaiming is most urgent exactly when the store cannot be written;
            // the pass runs without resume cursors and reports why.
            Err(error) => {
                failures.push(format!("resume cursors unavailable: {}", error.message));
                None
            }
        };
    let mut cursors: BTreeMap<String, String> = retention
        .as_ref()
        .and_then(|pass| pass.cursor())
        .and_then(|cursor| serde_json::from_str(cursor).ok())
        .unwrap_or_default();

    let _in_process = InProcessCategories::enter();
    let mut categories: Vec<CleanupInventoryCategory> = Vec::new();
    for (index, category) in DAEMON_MAINTENANCE_CATEGORIES.into_iter().enumerate() {
        let name = cleanup_category_metadata(category).category;
        let remote = category == CleanupCategoryArg::RemoteLabWorkspaces;
        let args = CleanupArgs {
            apply: true,
            include: vec![category],
            exclude: Vec::new(),
            include_untagged: false,
            older_than_days: None,
            runtime_tmp_managed_older_than_days: None,
            limit: None,
            full: false,
            cursor: if remote {
                retention.as_ref().map(|pass| {
                    serde_json::to_string(pass.remote_workspace_cursors()).expect("cursor map")
                })
            } else {
                cursors.get(name).cloned()
            },
            command: None,
        };
        let sweep = match CleanupSweep::new(&args) {
            Ok(sweep) => sweep,
            Err(error) => {
                failures.push(format!("{name}: {}", error.message));
                continue;
            }
        };
        let categories_left = (DAEMON_MAINTENANCE_CATEGORIES.len() - index) as u32;
        let share = pass_deadline
            .duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO)
            / categories_left;
        let mut budget = CleanupPassBudget::until(SystemTime::now().checked_add(share));
        let first = categories.len();
        run_cleanup_category(&sweep, category, &mut budget, &mut categories);
        let rows = &categories[first..];
        for row in rows {
            if let Some(failure) = row
                .failure
                .as_ref()
                .filter(|_| !is_bounded_continuation(row))
            {
                failures.push(format!("{}: {}", row.category, failure.message));
            }
        }
        if remote {
            if let Some(pass) = retention.as_mut() {
                save_remote_workspace_cursors(pass, rows);
            }
        } else if let Some(row) = rows.iter().find(|row| {
            row.failure.is_none() && row.outcome != super::CLEANUP_PHASE_OUTCOME_DEFERRED
        }) {
            // A completed walk clears the cursor so the next pass starts over;
            // a failed or deferred category keeps the position it had.
            match row.output.get("next_cursor").and_then(Value::as_str) {
                Some(next) => cursors.insert(name.to_string(), next.to_string()),
                None => cursors.remove(name),
            };
        }
    }

    if let Some(pass) = retention {
        let status = if failures.is_empty() {
            "completed"
        } else {
            "partial_failure"
        };
        let cursor =
            (!cursors.is_empty()).then(|| serde_json::to_string(&cursors).expect("cursor map"));
        if let Err(error) = pass.finish(status, cursor) {
            failures.push(format!("persist resume cursors: {}", error.message));
        }
    }
    Ok(failures)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;
    use std::time::{Duration, Instant, SystemTime};

    use super::*;

    fn git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(cwd)
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn the_plan_covers_every_per_attempt_path() {
        for category in [
            CleanupCategoryArg::ControllerScratch,
            CleanupCategoryArg::RuntimeTmp,
            // Needs a connected runner to exercise end to end; its presence in
            // the plan is what puts `_lab_workspaces/*` on the cadence.
            CleanupCategoryArg::RemoteLabWorkspaces,
        ] {
            assert!(
                DAEMON_MAINTENANCE_CATEGORIES.contains(&category),
                "{category:?} is not reclaimed on the daemon cadence"
            );
            assert!(reclaimed_by_daemon(
                cleanup_category_metadata(category).category
            ));
        }
        assert_eq!(DAEMON_MAINTENANCE_CATEGORIES.len(), 3);
        assert!(!DAEMON_MAINTENANCE_CATEGORIES.contains(&CleanupCategoryArg::TaskWorktrees));
        assert!(!reclaimed_by_daemon("terminal_runs"));
    }

    #[test]
    fn the_registered_provider_reclaims_finished_attempts_and_keeps_active_ones() {
        homeboy::test_support::with_isolated_home(|root| {
            let store = homeboy::core::observation::ObservationStore::open_initialized().unwrap();
            let running = store
                .start_run(homeboy::core::observation::NewRunRecord::builder("test").build())
                .unwrap();

            // controller-scratch: a released lease whose owner exited, and one
            // still held by a running attempt.
            let released = homeboy::agents::controller_scratch::allocate_attempt_at(
                &homeboy::core::paths::homeboy_data().unwrap(),
                "finished",
                "plan",
                "task",
                1,
            )
            .unwrap();
            std::fs::write(released.path.join("output"), "done").unwrap();
            homeboy::agents::controller_scratch::release_attempt_by_exited_owner_for_test(
                &released,
            )
            .unwrap();
            let active = homeboy::agents::controller_scratch::allocate_attempt_at(
                &homeboy::core::paths::homeboy_data().unwrap(),
                &running.id,
                "plan",
                "task",
                1,
            )
            .unwrap();
            std::fs::write(active.path.join("live"), "active").unwrap();

            // task-worktrees are operator work: a clean, unowned checkout an
            // operator just created survives the pass, as does an owned one.
            let source = root.path().join("repo");
            std::fs::create_dir_all(&source).unwrap();
            git(&source, &["init", "-q", "-b", "main"]);
            std::fs::write(source.join("README"), "fixture\n").unwrap();
            git(&source, &["add", "."]);
            git(&source, &["commit", "-q", "-m", "initial"]);
            let finished_worktree = root.path().join("finished-worktree");
            let active_worktree = root.path().join("active-worktree");
            for (id, path) in [
                ("fixture@finished", &finished_worktree),
                ("fixture@active", &active_worktree),
            ] {
                git(
                    &source,
                    &[
                        "worktree",
                        "add",
                        "-q",
                        "-b",
                        &format!("task/{id}"),
                        path.to_str().unwrap(),
                        "main",
                    ],
                );
            }
            std::fs::write(active_worktree.join("work"), "in flight\n").unwrap();
            git(&active_worktree, &["add", "."]);
            git(&active_worktree, &["commit", "-q", "-m", "in flight"]);
            homeboy::core::worktree::record_linked_active_for_test(
                "fixture@finished",
                &source,
                &finished_worktree,
                None,
            );
            homeboy::core::worktree::record_linked_active_for_test(
                "fixture@active",
                &source,
                &active_worktree,
                Some(&running.id),
            );

            // runtime-tmp: a stale ownerless root, and one a live owner holds.
            // The live owner needs a socket-safe root to land in this one.
            let runtime = tempfile::tempdir_in("/tmp").unwrap();
            let previous = std::env::var_os("HOMEBOY_RUNTIME_TMPDIR");
            std::env::set_var("HOMEBOY_RUNTIME_TMPDIR", runtime.path());
            let stale_tmp = runtime.path().join("homeboy-stray-finished");
            std::fs::create_dir(&stale_tmp).unwrap();
            std::fs::write(stale_tmp.join("payload"), "stale").unwrap();
            std::fs::File::open(&stale_tmp)
                .unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(30 * 86_400))
                .unwrap();
            let live_tmp =
                homeboy::core::engine::temp::RuntimeTempOwner::allocate("live", "test").unwrap();
            std::fs::write(live_tmp.path().join("payload"), "live").unwrap();

            register_storage_maintenance();
            let result = homeboy::core::daemon::storage_maintenance::with_provider(|provider| {
                provider.reclaim(Instant::now() + Duration::from_secs(60))
            });

            match previous {
                Some(value) => std::env::set_var("HOMEBOY_RUNTIME_TMPDIR", value),
                None => std::env::remove_var("HOMEBOY_RUNTIME_TMPDIR"),
            }
            result.expect("maintenance pass");

            assert!(!released.path.exists(), "released scratch was kept");
            assert!(
                active.path.join("live").exists(),
                "active scratch was reclaimed"
            );
            assert!(
                finished_worktree.exists(),
                "an operator's task worktree was reclaimed unattended"
            );
            assert!(
                active_worktree.join("work").exists(),
                "active task worktree was reclaimed"
            );
            assert!(!stale_tmp.exists(), "stale runtime temp root was kept");
            assert!(
                live_tmp.path().join("payload").exists(),
                "live runtime temp root was reclaimed"
            );
        });
    }
}
