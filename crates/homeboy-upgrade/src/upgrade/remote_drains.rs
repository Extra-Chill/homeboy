//! Controller-store authority for remote drain ownership and terminal cleanup.
use super::{operation::UPGRADE_OPERATION_KIND, runner_upgrade_provider::with_runner_upgrade};
use homeboy_core::{observation::ObservationStore, Error, Result};
use serde_json::json;

/// Apply the controller ownership rule around a runner transport. The runner
/// enforces the expected previous token atomically; only this controller consults
/// its authoritative upgrade store.
pub fn send_upgrade_drain<T>(
    runner: &str,
    operation: &str,
    request: &serde_json::Value,
    send: impl Fn(&serde_json::Value) -> Result<T>,
) -> Result<T> {
    match send(request) {
        Ok(response) => Ok(response),
        Err(error) => {
            let Some(owner) = error.details["drain_owner"]
                .as_str()
                .filter(|_| request["resume"] != true)
            else {
                return Err(error);
            };
            authorize_drain_takeover(operation, owner, runner)?;
            let mut request = request.clone();
            request["previous_operation_id"] = json!(owner);
            send(&request)
        }
    }
}

/// Authorize only a known terminal upgrade owner, and retain evidence before
/// requesting the runner's compare-and-swap. Unknown and live owners fail closed.
fn authorize_drain_takeover(operation: &str, previous: &str, runner: &str) -> Result<()> {
    let store = ObservationStore::open_initialized()?;
    let owner = store
        .get_run(previous)?
        .ok_or_else(|| Error::internal_unexpected("drain owner is unknown"))?;
    if owner.kind != UPGRADE_OPERATION_KIND
        || !matches!(owner.status.as_str(), "pass" | "fail" | "error")
    {
        return Err(Error::internal_unexpected(
            "another operation owns runner drain",
        ));
    }
    let run = store
        .get_run(operation)?
        .ok_or_else(|| Error::internal_unexpected("upgrade operation missing"))?;
    if run.kind != UPGRADE_OPERATION_KIND || run.status != "running" {
        return Err(Error::internal_unexpected(
            "drain contender is not a live upgrade",
        ));
    }
    let mut metadata = run.metadata_json;
    if !metadata["drain_takeovers"].is_array() {
        metadata["drain_takeovers"] = json!([]);
    }
    metadata["drain_takeovers"]
        .as_array_mut()
        .expect("array")
        .push(json!({
            "previous_owner": previous, "terminal_status": owner.status, "runner": runner,
            "action": "compare_and_swap_requested"
        }));
    store
        .update_running_run_metadata(operation, metadata)?
        .ok_or_else(|| {
            Error::internal_unexpected(
                "drain contender became terminal before takeover evidence was recorded",
            )
        })?;
    Ok(())
}

/// Best effort across the entire fleet, retaining failed releases for retry and
/// status. Reservations are persisted before network I/O (including ambiguous
/// responses), so every terminal path uses this same ownership inventory.
pub fn release_upgrade_drains(operation: &str) -> Result<()> {
    let store = ObservationStore::open_initialized()?;
    let Some(run) = store.get_run(operation)? else {
        return Ok(());
    };
    let mut metadata = run.metadata_json;
    let runners: Vec<String> = match metadata.get("remote_drains") {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
        None => with_runner_upgrade(|provider| provider.held_upgrade_drains(operation))?,
    };
    if runners.is_empty() {
        return Ok(());
    }
    let mut held = Vec::new();
    let mut failure = None;
    for runner in runners {
        if let Err(error) = with_runner_upgrade(|provider| {
            provider.set_upgrade_drain(std::slice::from_ref(&runner), operation, &[], true)
        }) {
            held.push(runner);
            failure = Some(error);
        }
    }
    metadata["remote_drains"] = json!(held);
    store.update_run_metadata(operation, metadata)?;
    failure.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upgrade::{
        operation::UpgradeOperation,
        runner_upgrade_provider::{with_test_provider, RunnerUpgradeProvider},
        ExtensionUpgradeEntry, InstallMethod, RunnerUpgradeEntry,
    };
    use homeboy_core::{runtime_promotion::remote_upgrade_drain, test_support::with_isolated_home};
    use std::path::Path;

    #[derive(Default)]
    struct Runner {
        fail_resume: bool,
    }
    impl RunnerUpgradeProvider for Runner {
        fn set_upgrade_drain(
            &self,
            runners: &[String],
            operation: &str,
            runs: &[String],
            resume: bool,
        ) -> Result<()> {
            if resume && self.fail_resume {
                return Err(Error::internal_unexpected("runner unreachable"));
            }
            for runner in runners {
                let request = json!({"operation_id": operation, "runs": runs, "resume": resume});
                send_upgrade_drain(runner, operation, &request, remote_upgrade_drain)?;
            }
            Ok(())
        }
        fn preflight_configured_runners_for_upgrade(
            &self,
            _: Option<InstallMethod>,
            _: Option<&Path>,
            _: bool,
            _: &[String],
            _: &str,
        ) -> Result<Vec<RunnerUpgradeEntry>> {
            Ok(vec![])
        }
        fn upgrade_configured_runners_with_explicit_source_path(
            &self,
            _: bool,
            _: Option<InstallMethod>,
            _: Option<&Path>,
            _: bool,
            _: Option<&str>,
            _: &[String],
            _: &[ExtensionUpgradeEntry],
            _: Option<&homeboy_core::runtime_promotion::RuntimePromotionLease>,
        ) -> Result<(Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>)> {
            Ok((vec![], vec![]))
        }
        fn source_checkout_build_identity(&self, _: &Path) -> Option<String> {
            None
        }
    }

    #[test]
    fn upgrade_terminal_owner_drain_takeover_has_store_evidence() {
        with_isolated_home(|_| {
            with_test_provider(Box::new(Runner::default()), || {
                for status in [
                    homeboy_core::observation::RunStatus::Error,
                    homeboy_core::observation::RunStatus::Fail,
                    homeboy_core::observation::RunStatus::Pass,
                ] {
                    let store = ObservationStore::open_initialized().unwrap();
                    let previous = store
                        .start_run(
                            homeboy_core::observation::NewRunRecord::builder(
                                UPGRADE_OPERATION_KIND,
                            )
                            .build(),
                        )
                        .unwrap();
                    let owner = previous.id;
                    store.finish_run(&owner, status, None).unwrap();
                    // Simulate a remote reservation left by an older controller.
                    remote_upgrade_drain(&json!({"operation_id": owner, "runs": []})).unwrap();
                    let mut next = UpgradeOperation::start_durable("next").unwrap();
                    next.reserve_remote_drains(&["lab".into()], &[]).unwrap();
                    let run = store.get_run(next.id().unwrap()).unwrap().unwrap();
                    assert_eq!(
                        run.metadata_json["drain_takeovers"][0]["previous_owner"],
                        owner
                    );
                    assert_eq!(
                        run.metadata_json["drain_takeovers"][0]["terminal_status"],
                        status.as_str()
                    );
                    assert_eq!(run.metadata_json["drain_takeovers"][0]["runner"], "lab");
                    next.set_phase_durable("testing persistence").unwrap();
                    assert_eq!(
                        store
                            .get_run(next.id().unwrap())
                            .unwrap()
                            .unwrap()
                            .metadata_json["drain_takeovers"],
                        run.metadata_json["drain_takeovers"]
                    );
                    drop(next);
                    assert!(
                        homeboy_core::runtime_promotion::ensure_upgrade_drain_admission(None, None)
                            .is_ok()
                    );
                }
            })
        });
    }

    #[test]
    fn upgrade_live_owner_drain_is_refused() {
        with_isolated_home(|_| {
            with_test_provider(Box::new(Runner::default()), || {
                let mut owner = UpgradeOperation::start_durable("owner").unwrap();
                owner.reserve_remote_drains(&["lab".into()], &[]).unwrap();
                let mut next = UpgradeOperation::start_durable("next").unwrap();
                assert!(next.reserve_remote_drains(&["lab".into()], &[]).is_err());
                drop(next);
                assert!(
                    homeboy_core::runtime_promotion::ensure_upgrade_drain_admission(None, None)
                        .is_err()
                );
                drop(owner);
                assert!(
                    homeboy_core::runtime_promotion::ensure_upgrade_drain_admission(None, None)
                        .is_ok()
                );
            })
        });
    }

    #[test]
    fn upgrade_owner_exit_releases_remote_drains_and_failed_release_is_actionable() {
        with_isolated_home(|_| {
            let id = with_test_provider(Box::new(Runner { fail_resume: true }), || {
                let mut operation = UpgradeOperation::start_durable("owner exits").unwrap();
                let id = operation.id().unwrap().to_string();
                operation
                    .reserve_remote_drains(&["lab".into()], &[])
                    .unwrap();
                let store = ObservationStore::open_initialized().unwrap();
                let mut metadata = store.get_run(&id).unwrap().unwrap().metadata_json;
                metadata["homeboy_run_owner"]["pid"] = json!(u32::MAX);
                store.update_running_run_metadata(&id, metadata).unwrap();
                // Status records the dead owner as terminal and attempts release.
                let status =
                    super::super::operation::load_upgrade_operation_status(Some(&id)).unwrap();
                assert_eq!(status.phase, "interrupted");
                assert_eq!(status.remote_drains, ["lab"]);
                assert_eq!(
                    status.release_drains_command,
                    Some(format!("homeboy upgrade release-drains {id}"))
                );
                drop(operation);
                id
            });
            with_test_provider(Box::new(Runner::default()), || {
                release_upgrade_drains(&id).unwrap();
                assert!(
                    super::super::operation::load_upgrade_operation_status(Some(&id))
                        .unwrap()
                        .remote_drains
                        .is_empty()
                );
                assert!(
                    homeboy_core::runtime_promotion::ensure_upgrade_drain_admission(None, None)
                        .is_ok()
                );
            });
        });
    }

    #[test]
    fn upgrade_interruption_releases_durable_remote_reservations() {
        with_isolated_home(|_| {
            with_test_provider(Box::new(Runner::default()), || {
                let mut operation = UpgradeOperation::start_durable("interrupted").unwrap();
                let id = operation.id().unwrap().to_string();
                operation
                    .reserve_remote_drains(&["lab".into()], &[])
                    .unwrap();
                assert!(
                    homeboy_core::runtime_promotion::ensure_upgrade_drain_admission(None, None)
                        .is_err()
                );
                drop(operation);
                let status =
                    super::super::operation::load_upgrade_operation_status(Some(&id)).unwrap();
                assert_eq!(status.phase, "interrupted");
                assert!(status.remote_drains.is_empty());
                assert!(
                    homeboy_core::runtime_promotion::ensure_upgrade_drain_admission(None, None)
                        .is_ok()
                );
            })
        });
    }
}
