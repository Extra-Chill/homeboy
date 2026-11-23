//! Durable CI verification receipt, separate from pre-publication gate evidence.
use super::cook_recipe::CookRecipeStore;
use homeboy_core::cook_status::{CookDisposition, CookStatus};
use homeboy_core::git::cook_ci::{poll_cook_ci, CookCiSource, GithubCookCi};
use homeboy_core::{Error, Result};
use serde_json::{json, Value};
use std::time::Duration;

/// Progress phase a Cook reports while it waits for required PR checks.
pub const AWAITING_CI_PHASE: &str = "awaiting_ci";

/// Bounded cadence for the Cook driver's automatic CI re-poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CookCiPollCadence {
    pub interval: Duration,
    pub max_polls: u32,
}

impl CookCiPollCadence {
    /// One poll a minute for up to an hour, so a full PR suite resolves
    /// without an operator `recover` and an abandoned PR cannot pin a driver.
    pub(crate) const DEFAULT: Self = Self {
        interval: Duration::from_secs(60),
        max_polls: 60,
    };
}

pub fn cook_ci_status(id: &str) -> Result<Option<Value>> {
    cook_ci_status_in(&CookRecipeStore::from_current_data_root()?, id)
}

pub(crate) fn cook_ci_status_in(store: &CookRecipeStore, id: &str) -> Result<Option<Value>> {
    // An attempt id resolves to its Cook; a Cook id names its receipt directly.
    let cook_id = store
        .load_recipe(id)
        .ok()
        .or_else(|| store.load_recipe_for_attempt(id).ok().flatten())
        .map_or_else(|| id.to_owned(), |recipe| recipe.cook_id);
    let path = store.ci_path(&cook_id);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).map_err(|e| Error::internal_unexpected(e.to_string()))?;
    Ok(Some(
        serde_json::from_slice(&bytes).map_err(|e| Error::internal_unexpected(e.to_string()))?,
    ))
}

pub fn repoll_cook_ci(id: &str) -> Result<Option<Value>> {
    repoll_cook_ci_in(
        &CookRecipeStore::from_current_data_root()?,
        id,
        &GithubCookCi,
    )
}

/// The single CI poll: read the receipt, classify the PR, persist the result.
pub(crate) fn repoll_cook_ci_in(
    store: &CookRecipeStore,
    id: &str,
    source: &dyn CookCiSource,
) -> Result<Option<Value>> {
    let Some(receipt) = cook_ci_status_in(store, id)? else {
        return Ok(None);
    };
    let cook_id = receipt["cook_id"].as_str().unwrap_or(id);
    let _driver = store.try_acquire_cook_driver(cook_id)?.ok_or_else(|| {
        Error::internal_unexpected("Cook is owned by another controller; poll again later")
    })?;
    let url = receipt["pr_url"]
        .as_str()
        .ok_or_else(|| Error::internal_unexpected("CI receipt missing PR URL"))?;
    let evidence = poll_cook_ci(source, url)?;
    let status = if evidence.status == "verified" {
        "completed"
    } else {
        &evidence.status
    };
    let value =
        json!({"cook_id":receipt["cook_id"], "pr_url":url, "status":status, "evidence":evidence});
    save(store, cook_id, &value)?;
    Ok(Some(value))
}

/// Re-poll an `awaiting_ci` receipt on a bounded cadence until it leaves
/// `awaiting_ci` or the budget is spent. A failed poll (transport error, head
/// moved mid-poll) is retried on the next tick rather than ending the wait.
/// Returns the last persisted receipt.
pub(crate) fn await_cook_ci(
    store: &CookRecipeStore,
    cook_id: &str,
    source: &dyn CookCiSource,
    cadence: CookCiPollCadence,
    sleep: &mut dyn FnMut(Duration),
) -> Result<Option<Value>> {
    let mut last = cook_ci_status_in(store, cook_id)?;
    for poll in 0..cadence.max_polls {
        if last
            .as_ref()
            .is_some_and(|receipt| receipt["status"] != "awaiting_ci")
        {
            break;
        }
        if poll > 0 {
            sleep(cadence.interval);
        }
        match repoll_cook_ci_in(store, cook_id, source) {
            Ok(Some(receipt)) => last = Some(receipt),
            Ok(None) => return Ok(None),
            Err(error) => {
                homeboy_core::log_status!("agent-task", "Cook CI poll failed: {}", error);
            }
        }
    }
    Ok(last)
}

/// The Cook outcome a settled CI receipt stands for, or `None` while CI is
/// still pending. Returns the status, the exit code and a stop reason.
pub(crate) fn ci_outcome(
    receipt: &Value,
) -> Option<(CookStatus, CookDisposition, i32, Option<String>)> {
    let feedback = || {
        let lines = receipt["evidence"]["feedback"]
            .as_array()
            .map(|lines| {
                lines
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();
        Some(lines).filter(|lines| !lines.is_empty())
    };
    let terminal = CookDisposition::Terminal;
    Some(match receipt["status"].as_str()? {
        "awaiting_ci" => return None,
        "completed" => (CookStatus::ReviewReady, terminal, 0, None),
        "ci_gate_failed" => (
            CookStatus::GateFailed,
            terminal,
            1,
            Some(format!(
                "Required CI failed: {}",
                feedback().unwrap_or_else(|| "no failing check reported".into())
            )),
        ),
        "ci_infrastructure" => (
            CookStatus::Failed,
            terminal,
            1,
            Some("Required CI was cancelled or timed out".into()),
        ),
        "pr_closed" => (
            CookStatus::Blocked,
            terminal,
            1,
            Some(feedback().unwrap_or_else(|| "PR was closed without merging".into())),
        ),
        other => (
            CookStatus::Failed,
            terminal,
            1,
            Some(format!("Unrecognized CI outcome `{other}`")),
        ),
    })
}

/// Drive an `awaiting_ci` Cook report to its CI outcome on a bounded cadence.
/// On a transition the report becomes terminal, so the driver's normal
/// terminal path settles and announces it. Still pending: left unchanged.
pub(crate) fn settle_awaiting_ci(
    store: &CookRecipeStore,
    report: &mut super::AgentTaskCookReport,
    exit_code: &mut i32,
    source: &dyn CookCiSource,
    cadence: CookCiPollCadence,
    sleep: &mut dyn FnMut(Duration),
) -> Result<()> {
    if report.status != CookStatus::AwaitingCi {
        return Ok(());
    }
    let Some(receipt) = await_cook_ci(store, &report.cook_id, source, cadence, sleep)? else {
        return Ok(());
    };
    if let Some((status, disposition, code, stop_reason)) = ci_outcome(&receipt) {
        report.status = status;
        report.disposition = disposition;
        report.stop_reason = stop_reason.or(report.stop_reason.take());
        *exit_code = code;
    }
    Ok(())
}

pub(crate) fn save(store: &CookRecipeStore, cook_id: &str, value: &Value) -> Result<()> {
    let path = store.ci_path(cook_id);
    homeboy_engine_primitives::local_files::write_json_file(&path, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::git::cook_ci::CookPrState;
    use std::cell::RefCell;

    /// Serves one scripted PR observation per poll; the last one repeats.
    struct Scripted(RefCell<Vec<(CookPrState, Vec<Value>)>>);
    impl Scripted {
        fn new(steps: Vec<(CookPrState, Vec<Value>)>) -> Self {
            Self(RefCell::new(steps))
        }
        fn current(&self) -> (CookPrState, Vec<Value>) {
            self.0.borrow()[0].clone()
        }
    }
    impl CookCiSource for Scripted {
        fn pull(&self, _: &str) -> Result<CookPrState> {
            Ok(self.current().0)
        }
        fn checks(&self, _: &str) -> Result<(String, Vec<Value>)> {
            let (pr, checks) = self.current();
            let mut steps = self.0.borrow_mut();
            if steps.len() > 1 {
                steps.remove(0);
            }
            Ok((pr.head_sha, checks))
        }
    }
    fn open_pr() -> CookPrState {
        CookPrState {
            state: "open".into(),
            mergeable: Some(true),
            mergeable_state: "clean".into(),
            base_ref: "main".into(),
            head_sha: "head".into(),
            ..CookPrState::default()
        }
    }
    fn check(state: &str) -> Vec<Value> {
        vec![json!({"name":"ci", "state":state, "link":"https://ci/log"})]
    }
    fn awaiting_store() -> (tempfile::TempDir, CookRecipeStore) {
        let temp = tempfile::tempdir().unwrap();
        let store = CookRecipeStore::from_data_root(temp.path().to_path_buf());
        let path = store.ci_path("cook");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        save(
            &store,
            "cook",
            &json!({"cook_id":"cook", "pr_url":"https://github.com/o/r/pull/1", "status":"awaiting_ci"}),
        )
        .unwrap();
        (temp, store)
    }
    fn cadence(max_polls: u32) -> CookCiPollCadence {
        CookCiPollCadence {
            interval: Duration::from_secs(60),
            max_polls,
        }
    }

    #[test]
    fn awaiting_ci_repolls_until_checks_settle() {
        let (_temp, store) = awaiting_store();
        let source = Scripted::new(vec![
            (open_pr(), vec![]),
            (open_pr(), check("IN_PROGRESS")),
            (open_pr(), check("SUCCESS")),
        ]);
        let mut sleeps = 0;
        let receipt = await_cook_ci(&store, "cook", &source, cadence(10), &mut |_| sleeps += 1)
            .unwrap()
            .unwrap();
        assert_eq!(receipt["status"], "completed");
        assert_eq!(sleeps, 2, "first poll is immediate, then one per interval");
        assert_eq!(
            ci_outcome(&receipt).map(|o| (o.0, o.2)),
            Some((CookStatus::ReviewReady, 0))
        );
    }

    #[test]
    fn awaiting_ci_polling_is_bounded() {
        let (_temp, store) = awaiting_store();
        let source = Scripted::new(vec![(open_pr(), vec![])]);
        let mut sleeps = 0;
        let receipt = await_cook_ci(&store, "cook", &source, cadence(3), &mut |_| sleeps += 1)
            .unwrap()
            .unwrap();
        assert_eq!(receipt["status"], "awaiting_ci");
        assert_eq!(sleeps, 2);
        assert!(ci_outcome(&receipt).is_none());
    }

    #[test]
    fn awaiting_ci_conflict_merge_and_close_settle_the_receipt() {
        let conflicting = CookPrState {
            mergeable: Some(false),
            mergeable_state: "dirty".into(),
            ..open_pr()
        };
        let merged = CookPrState {
            state: "closed".into(),
            merged: true,
            ..open_pr()
        };
        let closed = CookPrState {
            state: "closed".into(),
            ..open_pr()
        };
        for (pr, receipt_status, cook_status) in [
            (conflicting, "ci_gate_failed", CookStatus::GateFailed),
            (merged, "completed", CookStatus::ReviewReady),
            (closed, "pr_closed", CookStatus::Blocked),
        ] {
            let (_temp, store) = awaiting_store();
            let receipt = await_cook_ci(
                &store,
                "cook",
                &Scripted::new(vec![(pr, vec![])]),
                cadence(5),
                &mut |_| panic!("a settled PR needs no second poll"),
            )
            .unwrap()
            .unwrap();
            assert_eq!(receipt["status"], receipt_status);
            assert_eq!(ci_outcome(&receipt).unwrap().0, cook_status);
            let persisted = cook_ci_status_in(&store, "cook").unwrap().unwrap();
            assert_eq!(persisted["status"], receipt_status);
        }
    }
    #[test]
    fn cook_ci_receipt_is_durable_and_replaceable() {
        let root = std::env::temp_dir().join(format!("cook-ci-{}", uuid::Uuid::new_v4()));
        let store = CookRecipeStore::from_data_root(root.clone());
        let path = store.ci_path("cook");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for status in ["awaiting_ci", "ci_gate_failed", "completed"] {
            let receipt = json!({"cook_id":"cook", "pr_url":"https://github.com/o/r/pull/1", "status":status});
            save(&store, "cook", &receipt).unwrap();
            let restored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(restored, receipt);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
