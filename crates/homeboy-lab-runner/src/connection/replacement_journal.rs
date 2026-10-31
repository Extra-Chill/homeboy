//! The controller's durable journal of one runner's daemon replacement.
//!
//! A remote lifecycle mutation (start, state-loss recovery, unleased
//! candidate reconciliation) is journaled under a stable operation id before
//! it crosses the remote boundary, so a lost response is replayed rather than
//! repeated. A recovery-created daemon's coordinates are journaled as the
//! pending replacement until a controller authenticates it. Every journal is
//! keyed by the runner and guarded by the runner state lock.

use std::path::{Path, PathBuf};

use chrono::Utc;
use homeboy_core::error::{Error, Result};
use homeboy_core::paths;

use crate::runner_state_lock::{
    runner_state_dir_in_root, with_rooted_runner_registry_lock, with_runner_registry_lock,
    write_durable_json,
};
use crate::RunnerSession;

fn pending_replacement_path_in_root(config_root: &Path, runner_id: &str) -> PathBuf {
    runner_state_dir_in_root(config_root, runner_id).join("pending-replacement.json")
}

fn pending_replacement_path(runner_id: &str) -> Result<PathBuf> {
    Ok(pending_replacement_path_in_root(
        &paths::homeboy()?,
        runner_id,
    ))
}

fn replacement_operation_path_in_root(config_root: &Path, runner_id: &str) -> PathBuf {
    runner_state_dir_in_root(config_root, runner_id).join("replacement-operation.json")
}

fn replacement_operation_path(runner_id: &str) -> Result<PathBuf> {
    Ok(replacement_operation_path_in_root(
        &paths::homeboy()?,
        runner_id,
    ))
}

fn rejected_replacement_path_in_root(config_root: &Path, runner_id: &str) -> PathBuf {
    runner_state_dir_in_root(config_root, runner_id)
        .join("rejected-replacements")
        .join(format!("{}.json", uuid::Uuid::new_v4()))
}

fn superseded_replacement_path_in_root(config_root: &Path, runner_id: &str) -> PathBuf {
    runner_state_dir_in_root(config_root, runner_id)
        .join("superseded-replacements")
        .join(format!("{}.json", uuid::Uuid::new_v4()))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ReplacementOperation {
    runner_id: String,
    operation_id: String,
    replay_command: Option<String>,
    kind: Option<String>,
}

#[derive(serde::Serialize)]
struct RejectedReplacementEvidence<'a> {
    schema: &'static str,
    runner_id: &'a str,
    operation_id: &'a str,
    kind: &'a str,
    replay_command: Option<&'a str>,
    rejected_at: String,
    exit_code: i32,
    timed_out: bool,
    stdout: &'a str,
    stderr: &'a str,
}

#[derive(serde::Serialize)]
struct SupersededReplacementEvidence<'a> {
    schema: &'static str,
    runner_id: &'a str,
    operation_id: &'a str,
    previous_kind: &'a str,
    previous_replay_command: &'a str,
    replacement_kind: &'a str,
    replacement_replay_command: &'a str,
    superseded_at: String,
}

/// A recovery-created daemon is durable before a controller can authenticate
/// its tunnel. Retain its exact coordinates so an interruption cannot fall
/// back to the superseded generation or start a competing daemon.
pub(super) fn pending_replacement(runner_id: &str) -> Result<Option<RunnerSession>> {
    let path = pending_replacement_path(runner_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
    })?;
    let session: RunnerSession = serde_json::from_str(&raw)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
    if session.runner_id != runner_id {
        return Err(Error::config_invalid_value(
            "pending_replacement.runner_id",
            Some(session.runner_id),
            format!("pending replacement must match runner `{runner_id}`"),
        ));
    }
    Ok(Some(session))
}

/// Persist the controller id before invoking a remote lifecycle mutation. The
/// daemon uses this exact id as its durable startup token/receipt key.
pub(super) fn replacement_operation(runner_id: &str) -> Result<String> {
    with_runner_registry_lock(runner_id, || {
        let path = replacement_operation_path(runner_id)?;
        if path.exists() {
            let operation: ReplacementOperation =
                serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
                    Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
                })?)
                .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
            if operation.runner_id != runner_id {
                return Err(Error::config_invalid_value(
                    "replacement_operation.runner_id",
                    Some(operation.runner_id),
                    "replacement operation must match its runner",
                ));
            }
            return Ok(operation.operation_id);
        }
        let operation_id = uuid::Uuid::new_v4().to_string();
        write_durable_json(
            &path,
            &ReplacementOperation {
                runner_id: runner_id.to_string(),
                operation_id: operation_id.clone(),
                replay_command: None,
                kind: None,
            },
        )?;
        Ok(operation_id)
    })
}

pub(super) fn replacement_operation_replay(runner_id: &str) -> Result<Option<(String, String)>> {
    let path = replacement_operation_path(runner_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let operation: ReplacementOperation =
        serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
        })?)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
    if operation.runner_id != runner_id {
        return Err(Error::config_invalid_value(
            "replacement_operation.runner_id",
            Some(operation.runner_id),
            "replacement operation must match its runner",
        ));
    }
    Ok(operation.kind.zip(operation.replay_command))
}

pub(super) fn record_replacement_operation_replay(
    runner_id: &str,
    kind: &str,
    command: &str,
) -> Result<()> {
    with_runner_registry_lock(runner_id, || {
        let path = replacement_operation_path(runner_id)?;
        let mut operation: ReplacementOperation =
            serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
                Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
            })?)
            .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
        operation.replay_command = Some(command.to_string());
        operation.kind = Some(kind.to_string());
        write_durable_json(&path, &operation)
    })
}

/// Bind candidate reconciliation to the current replacement identity. An
/// explicit operator recovery may supersede only `ensure-running`: that command
/// can create one of the candidates reconciliation is responsible for, and
/// replaying it first would reproduce the conflict the operator selected this
/// recovery to resolve.
pub(super) fn record_unleased_candidate_reconciliation_replay(
    runner_id: &str,
    command: &str,
) -> Result<()> {
    with_rooted_runner_registry_lock(runner_id, |config_root| {
        let path = replacement_operation_path_in_root(config_root, runner_id);
        let mut operation: ReplacementOperation =
            serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
                Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
            })?)
            .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
        match (
            operation.kind.as_deref(),
            operation.replay_command.as_deref(),
        ) {
            (Some("unleased-candidates"), Some(existing)) if existing == command => return Ok(()),
            (Some("ensure-running"), Some(previous_command)) => {
                write_durable_json(
                    &superseded_replacement_path_in_root(config_root, runner_id),
                    &SupersededReplacementEvidence {
                        schema: "homeboy/runner-replacement-operation-supersession/v1",
                        runner_id,
                        operation_id: &operation.operation_id,
                        previous_kind: "ensure-running",
                        previous_replay_command: previous_command,
                        replacement_kind: "unleased-candidates",
                        replacement_replay_command: command,
                        superseded_at: Utc::now().to_rfc3339(),
                    },
                )?;
            }
            (None, None) => {}
            _ => {
                return Err(Error::internal_unexpected(
                    "replacement operation cannot transition to unleased candidate reconciliation",
                ));
            }
        }
        operation.kind = Some("unleased-candidates".to_string());
        operation.replay_command = Some(command.to_string());
        write_durable_json(&path, &operation)
    })
}

/// An acknowledged `applied: false` reconciliation is terminal: the remote
/// inspected the candidate set and declined to mutate it. Rotate the operation
/// identity rather than replaying that stale observation on a later connect.
pub(super) fn terminalize_unleased_candidate_reconciliation(runner_id: &str) -> Result<()> {
    with_runner_registry_lock(runner_id, || {
        let path = replacement_operation_path(runner_id)?;
        let operation: ReplacementOperation =
            serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
                Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
            })?)
            .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
        if operation.runner_id != runner_id
            || operation.kind.as_deref() != Some("unleased-candidates")
            || operation.replay_command.is_none()
        {
            return Err(Error::internal_unexpected(
                "refusing to terminalize a replacement operation that is not an immutable unleased candidate reconciliation",
            ));
        }
        write_durable_json(
            &path,
            &ReplacementOperation {
                runner_id: runner_id.to_string(),
                operation_id: uuid::Uuid::new_v4().to_string(),
                replay_command: None,
                kind: None,
            },
        )
    })
}

/// An exact live-lease adoption rechecks the observed daemon after its tunnel
/// health probe. Once that succeeds, an older automatic start replay cannot
/// safely add information and is terminalized with its replacement evidence.
pub(super) fn terminalize_ensure_running_replay_for_live_lease_adoption(
    runner_id: &str,
) -> Result<()> {
    with_rooted_runner_registry_lock(runner_id, |config_root| {
        let path = replacement_operation_path_in_root(config_root, runner_id);
        let mut operation: ReplacementOperation =
            serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
                Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
            })?)
            .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
        let Some(previous_command) = operation.replay_command.as_deref() else {
            return Ok(());
        };
        if operation.kind.as_deref() != Some("ensure-running") {
            return Ok(());
        }
        write_durable_json(
            &superseded_replacement_path_in_root(config_root, runner_id),
            &SupersededReplacementEvidence {
                schema: "homeboy/runner-replacement-operation-supersession/v1",
                runner_id,
                operation_id: &operation.operation_id,
                previous_kind: "ensure-running",
                previous_replay_command: previous_command,
                replacement_kind: "live-lease-adoption",
                replacement_replay_command: "verified exact live-lease adoption",
                superseded_at: Utc::now().to_rfc3339(),
            },
        )?;
        operation.operation_id = uuid::Uuid::new_v4().to_string();
        operation.replay_command = None;
        operation.kind = None;
        write_durable_json(&path, &operation)
    })
}

/// Update an operation journal while the caller already holds this runner's
/// state lock.
pub(super) fn record_replacement_operation_replay_locked(
    runner_id: &str,
    kind: &str,
    command: &str,
) -> Result<()> {
    let path = replacement_operation_path(runner_id)?;
    let mut operation: ReplacementOperation =
        serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
        })?)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
    operation.replay_command = Some(command.to_string());
    operation.kind = Some(kind.to_string());
    write_durable_json(&path, &operation)
}

pub(super) fn record_pending_replacement(runner_id: &str, session: &RunnerSession) -> Result<()> {
    with_runner_registry_lock(runner_id, || {
        write_durable_json(&pending_replacement_path(runner_id)?, session)
    })
}

/// Retire an interrupted replacement only after the caller has re-probed the
/// remote lease and proved that these recorded coordinates are no longer a
/// publishable daemon. A later attempt receives a new operation identity.
pub(super) fn retire_pending_replacement(runner_id: &str) -> Result<String> {
    retire_replacement(runner_id)
}

fn retire_replacement(runner_id: &str) -> Result<String> {
    with_rooted_runner_registry_lock(runner_id, |config_root| {
        let operation_id = uuid::Uuid::new_v4().to_string();
        // Publish the successor receipt before releasing the old coordinates.
        // A crash can therefore leave both records, but never neither record.
        write_durable_json(
            &replacement_operation_path_in_root(config_root, runner_id),
            &ReplacementOperation {
                runner_id: runner_id.to_string(),
                operation_id: operation_id.clone(),
                replay_command: None,
                kind: None,
            },
        )?;
        let pending_path = pending_replacement_path_in_root(config_root, runner_id);
        if pending_path.exists() {
            std::fs::remove_file(&pending_path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("remove {}", pending_path.display())),
                )
            })?;
        }
        Ok(operation_id)
    })
}

/// Retire a remote policy or validation refusal after preserving the exact
/// terminal response. Unlike an interrupted mutation, this operation cannot
/// become publishable by replaying the same authority.
pub(super) fn retire_rejected_state_loss_replacement(
    runner_id: &str,
    output: &homeboy_core::server::CommandOutput,
) -> Result<()> {
    retire_rejected_replacement(
        runner_id,
        "state-loss",
        output.exit_code,
        output.timed_out,
        &output.stdout,
        &output.stderr,
    )
}

/// Retire a terminally refused `ensure-running` replacement after preserving
/// the exact rejection. `ensure-running` journals its replay command before
/// crossing the remote mutation boundary (#8811 pattern) so a lost response
/// can be replayed safely — but a `daemon_unleased_process_conflict` refusal
/// is not a lost response, it is the remote authoritatively saying the exact
/// operation id this journal entry names will never be admitted. Replaying
/// that same command forever reproduces #15087: every subsequent `runner
/// connect`/`refresh-homeboy --reconnect` repeats the identical doomed
/// mutation instead of minting a fresh attempt.
///
/// Plain fields rather than a borrowed `CommandOutput`: the caller that hits
/// this refusal on the *first* ensure-running attempt (not a replay) only
/// has the summarized `Error` the remote failure was projected into, not the
/// raw SSH transport output.
pub(super) fn retire_rejected_ensure_running_replacement(
    runner_id: &str,
    exit_code: i32,
    timed_out: bool,
    stdout: &str,
    stderr: &str,
) -> Result<()> {
    retire_rejected_replacement(
        runner_id,
        "ensure-running",
        exit_code,
        timed_out,
        stdout,
        stderr,
    )
}

fn retire_rejected_replacement(
    runner_id: &str,
    kind: &str,
    exit_code: i32,
    timed_out: bool,
    stdout: &str,
    stderr: &str,
) -> Result<()> {
    with_rooted_runner_registry_lock(runner_id, |config_root| {
        let operation_path = replacement_operation_path_in_root(config_root, runner_id);
        let operation: ReplacementOperation =
            serde_json::from_slice(&std::fs::read(&operation_path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("read {}", operation_path.display())),
                )
            })?)
            .map_err(|error| {
                Error::config_invalid_json(operation_path.display().to_string(), error)
            })?;
        if operation.runner_id != runner_id || operation.kind.as_deref() != Some(kind) {
            return Err(Error::internal_unexpected(format!(
                "refusing to retire a replacement operation that is not the rejected {kind} recovery"
            )));
        }
        let evidence_path = rejected_replacement_path_in_root(config_root, runner_id);
        write_durable_json(
            &evidence_path,
            &RejectedReplacementEvidence {
                schema: "homeboy/runner-rejected-replacement/v1",
                runner_id,
                operation_id: &operation.operation_id,
                kind,
                replay_command: operation.replay_command.as_deref(),
                rejected_at: Utc::now().to_rfc3339(),
                exit_code,
                timed_out,
                stdout,
                stderr,
            },
        )?;
        write_durable_json(
            &operation_path,
            &ReplacementOperation {
                runner_id: runner_id.to_string(),
                operation_id: uuid::Uuid::new_v4().to_string(),
                replay_command: None,
                kind: None,
            },
        )?;
        let pending_path = pending_replacement_path_in_root(config_root, runner_id);
        if pending_path.exists() {
            std::fs::remove_file(&pending_path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("remove {}", pending_path.display())),
                )
            })?;
        }
        Ok(())
    })
}

/// Promote B only after `connect_remote_daemon` has authenticated its lease,
/// PID, version, and build identity. Keeping the pending record until this
/// locked transaction commits makes every interruption retry B first.
pub(super) fn promote_pending_replacement(runner_id: &str, session: &RunnerSession) -> Result<()> {
    let generation = session.remote_daemon_lease_id.as_deref().ok_or_else(|| {
        Error::internal_unexpected("authenticated daemon session has no lease ID")
    })?;
    with_runner_registry_lock(runner_id, || {
        if let Some(pending) = pending_replacement(runner_id)? {
            if pending.remote_daemon_lease_id.as_deref() != Some(generation)
                || pending.remote_daemon_pid != session.remote_daemon_pid
                || pending.remote_daemon_address != session.remote_daemon_address
            {
                return Err(Error::validation_invalid_argument(
                    "pending_replacement",
                    "authenticated daemon does not match the pending replacement; refusing authority promotion",
                    Some(runner_id.to_string()),
                    None,
                ));
            }
        }
        crate::generation_store::record_authenticated_admission_locked(
            runner_id, session, generation,
        )?;
        let path = pending_replacement_path(runner_id)?;
        if path.exists() {
            std::fs::remove_file(&path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("remove {}", path.display())),
                )
            })?;
        }
        let operation_path = replacement_operation_path(runner_id)?;
        if operation_path.exists() {
            std::fs::remove_file(&operation_path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("remove {}", operation_path.display())),
                )
            })?;
        }
        Ok(())
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn session(lease: &str) -> RunnerSession {
        let mut session = crate::test_support::direct_ssh_session(lease);
        session.runner_id = "runner-a".to_string();
        session
    }

    #[test]
    fn retiring_an_unpublishable_replacement_releases_its_endpoint_and_operation_identity() {
        crate::test_support::with_isolated_home(|_| {
            let first_operation = replacement_operation("runner-a").expect("operation");
            record_replacement_operation_replay("runner-a", "ensure-running", "replay")
                .expect("replay journal");
            record_pending_replacement("runner-a", &session("lease-dead"))
                .expect("pending coordinates");

            let next_operation =
                retire_pending_replacement("runner-a").expect("retire dead pending replacement");

            assert!(pending_replacement("runner-a")
                .expect("read pending")
                .is_none());
            assert!(replacement_operation_replay("runner-a")
                .expect("read replay")
                .is_none());
            assert_ne!(next_operation, first_operation);
            assert_eq!(
                replacement_operation("runner-a").expect("new operation"),
                next_operation
            );
        });
    }

    /// #15087: a `daemon_unleased_process_conflict` refusal is authoritative,
    /// not a lost response. Retiring it must clear the journal so the next
    /// connect mints a fresh operation id instead of replaying the exact
    /// command that will refuse identically forever, and must preserve the
    /// exact rejection as durable evidence.
    #[test]
    fn retiring_a_rejected_ensure_running_replacement_preserves_evidence_and_frees_the_operation() {
        crate::test_support::with_isolated_home(|_| {
            let first_operation = replacement_operation("runner-a").expect("operation");
            let command =
                "homeboy daemon ensure-running --replacement-operation-id operation-a --addr 127.0.0.1:0";
            record_replacement_operation_replay("runner-a", "ensure-running", command)
                .expect("ensure-running replay journal");
            record_pending_replacement("runner-a", &session("lease-conflict"))
                .expect("pending coordinates");

            retire_rejected_ensure_running_replacement(
                "runner-a",
                2,
                false,
                "",
                "daemon lease is absent or stale while foreground daemon candidates remain live",
            )
            .expect("retire the terminally refused ensure-running replacement");

            assert!(
                pending_replacement("runner-a")
                    .expect("read pending")
                    .is_none(),
                "the unpublishable pending coordinates must not survive the refusal"
            );
            assert!(
                replacement_operation_replay("runner-a")
                    .expect("read replay")
                    .is_none(),
                "the doomed replay command must not be replayed again"
            );
            let next_operation = replacement_operation("runner-a").expect("new operation");
            assert_ne!(
                next_operation, first_operation,
                "a fresh operation id must be minted so a retry cannot collide with the refused one"
            );

            let evidence_dir = paths::runner_sessions_dir()
                .expect("runner sessions")
                .join("runner-a")
                .join("rejected-replacements");
            let evidence = std::fs::read_dir(&evidence_dir)
                .expect("rejection evidence directory")
                .collect::<std::result::Result<Vec<_>, _>>()
                .expect("evidence entries");
            assert_eq!(evidence.len(), 1, "exactly one rejection is recorded");
            let evidence: serde_json::Value = serde_json::from_slice(
                &std::fs::read(evidence[0].path()).expect("read rejection evidence"),
            )
            .expect("rejection evidence JSON");
            assert_eq!(evidence["kind"], "ensure-running");
            assert_eq!(evidence["operation_id"], first_operation);
            assert_eq!(evidence["replay_command"], command);
            assert_eq!(evidence["exit_code"], 2);
            assert_eq!(evidence["timed_out"], false);
        });
    }

    /// Retiring the wrong kind must fail closed rather than silently clearing
    /// a replacement journal entry that belongs to a different, still-viable
    /// recovery (e.g. a state-loss replay in flight).
    #[test]
    fn retiring_ensure_running_refuses_a_journal_of_a_different_kind() {
        crate::test_support::with_isolated_home(|_| {
            replacement_operation("runner-a").expect("operation");
            record_replacement_operation_replay(
                "runner-a",
                "state-loss",
                "homeboy daemon recover-missing-lease-state --lease-id lease-a",
            )
            .expect("state-loss replay journal");

            let error =
                retire_rejected_ensure_running_replacement("runner-a", 1, false, "", "unrelated")
                    .expect_err("must not retire a differently-kinded journal entry");
            assert!(error
                .message
                .contains("not the rejected ensure-running recovery"));
            assert_eq!(
                replacement_operation_replay("runner-a")
                    .expect("read replay")
                    .map(|(kind, _)| kind),
                Some("state-loss".to_string()),
                "the unrelated journal entry must be left untouched"
            );
        });
    }

    #[test]
    fn explicit_candidate_reconciliation_supersedes_ensure_running_with_evidence() {
        crate::test_support::with_isolated_home(|_| {
            let operation_id = replacement_operation("runner-a").expect("operation");
            record_replacement_operation_replay(
                "runner-a",
                "ensure-running",
                "homeboy daemon ensure-running --replacement-operation-id operation-a",
            )
            .expect("ensure-running replay");

            let reconciliation = "homeboy daemon reconcile-unleased-candidates --apply --replacement-operation-id operation-a";
            record_unleased_candidate_reconciliation_replay("runner-a", reconciliation)
                .expect("explicit reconciliation supersedes ensure-running");
            record_unleased_candidate_reconciliation_replay("runner-a", reconciliation)
                .expect("same reconciliation replay is idempotent");

            assert_eq!(
                replacement_operation("runner-a").expect("preserved operation"),
                operation_id
            );
            assert_eq!(
                replacement_operation_replay("runner-a").expect("replacement replay"),
                Some((
                    "unleased-candidates".to_string(),
                    reconciliation.to_string()
                ))
            );
            let evidence_dir = paths::runner_sessions_dir()
                .expect("runner sessions")
                .join("runner-a")
                .join("superseded-replacements");
            let evidence = std::fs::read_dir(evidence_dir)
                .expect("supersession evidence")
                .collect::<std::result::Result<Vec<_>, _>>()
                .expect("evidence entries");
            assert_eq!(evidence.len(), 1, "idempotent replay writes one transition");
            let evidence: serde_json::Value = serde_json::from_slice(
                &std::fs::read(evidence[0].path()).expect("read supersession evidence"),
            )
            .expect("supersession evidence JSON");
            assert_eq!(evidence["operation_id"], operation_id);
            assert_eq!(evidence["previous_kind"], "ensure-running");
            assert_eq!(evidence["replacement_kind"], "unleased-candidates");
        });
    }

    #[test]
    fn verified_live_lease_adoption_terminalizes_ensure_running_with_evidence() {
        crate::test_support::with_isolated_home(|_| {
            let operation_id = replacement_operation("runner-a").expect("operation");
            record_replacement_operation_replay(
                "runner-a",
                "ensure-running",
                "homeboy daemon ensure-running --replacement-operation-id operation-a",
            )
            .expect("ensure-running replay");

            terminalize_ensure_running_replay_for_live_lease_adoption("runner-a")
                .expect("verified adoption terminalizes stale replay");

            assert!(replacement_operation_replay("runner-a")
                .expect("replacement replay")
                .is_none());
            assert_ne!(
                replacement_operation("runner-a").expect("replacement operation"),
                operation_id
            );
            let evidence_dir = paths::runner_sessions_dir()
                .expect("runner sessions")
                .join("runner-a")
                .join("superseded-replacements");
            let evidence = std::fs::read_dir(evidence_dir)
                .expect("supersession evidence")
                .collect::<std::result::Result<Vec<_>, _>>()
                .expect("evidence entries");
            assert_eq!(evidence.len(), 1);
            let evidence: serde_json::Value = serde_json::from_slice(
                &std::fs::read(evidence[0].path()).expect("read supersession evidence"),
            )
            .expect("supersession evidence JSON");
            assert_eq!(evidence["operation_id"], operation_id);
            assert_eq!(evidence["previous_kind"], "ensure-running");
            assert_eq!(evidence["replacement_kind"], "live-lease-adoption");
        });
    }

    #[test]
    fn a_journal_without_a_replay_binds_its_first_replay_command() {
        crate::test_support::with_isolated_home(|_| {
            let path = replacement_operation_path("runner-a").expect("journal path");
            write_durable_json(
                &path,
                &ReplacementOperation {
                    runner_id: "runner-a".to_string(),
                    operation_id: "legacy-operation".to_string(),
                    replay_command: None,
                    kind: None,
                },
            )
            .expect("write unbound journal");

            assert_eq!(
                replacement_operation("runner-a").expect("read operation"),
                "legacy-operation"
            );
            assert!(replacement_operation_replay("runner-a")
                .expect("read legacy replay")
                .is_none());
            record_replacement_operation_replay(
                "runner-a",
                "ensure-running",
                "/selected/homeboy daemon ensure-running --replacement-operation-id legacy-operation --addr 127.0.0.1:0",
            )
            .expect("rebind legacy journal");
            assert_eq!(
                replacement_operation_replay("runner-a").expect("read rebound replay"),
                Some((
                    "ensure-running".to_string(),
                    "/selected/homeboy daemon ensure-running --replacement-operation-id legacy-operation --addr 127.0.0.1:0".to_string(),
                ))
            );
        });
    }

    #[test]
    fn promoting_the_authenticated_pending_replacement_clears_both_journals() {
        crate::test_support::with_isolated_home(|_| {
            replacement_operation("runner-a").expect("operation");
            let pending = session("lease-b");
            record_pending_replacement("runner-a", &pending).expect("pending coordinates");

            promote_pending_replacement("runner-a", &pending).expect("promote");

            assert!(pending_replacement("runner-a").expect("read").is_none());
            assert!(!replacement_operation_path("runner-a")
                .expect("journal path")
                .exists());
        });
    }

    #[test]
    fn promoting_a_daemon_other_than_the_pending_replacement_is_refused() {
        crate::test_support::with_isolated_home(|_| {
            record_pending_replacement("runner-a", &session("lease-b")).expect("pending");

            let error = promote_pending_replacement("runner-a", &session("lease-c"))
                .expect_err("a different daemon is not the pending replacement");

            assert_eq!(error.details["field"], "pending_replacement");
            assert!(pending_replacement("runner-a").expect("read").is_some());
        });
    }
}
