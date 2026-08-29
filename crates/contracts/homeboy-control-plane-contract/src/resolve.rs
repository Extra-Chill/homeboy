//! Deterministic resolution of persisted identity strings.
//!
//! Resolution is pure and total: every input yields a resolved identity set or
//! a typed explanation of why it could not be resolved. It never guesses a
//! kind from an untagged string.

use std::fmt;

use crate::identity::{validate_opaque, AttemptId, ExecutionId, MissionId, RunId, TaskId};

/// Which persisted identity string is being resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityKind {
    CookId,
    RunId,
    TaskId,
    RunnerJobId,
    FanoutPortfolioId,
}

impl IdentityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CookId => "cook id",
            Self::RunId => "run id",
            Self::TaskId => "task id",
            Self::RunnerJobId => "runner job id",
            Self::FanoutPortfolioId => "fanout portfolio id",
        }
    }
}

impl fmt::Display for IdentityKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Canonical identities denoted by a persisted identity string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedIdentities {
    pub mission: Option<MissionId>,
    pub run: Option<RunId>,
    pub attempt: Option<AttemptId>,
    pub attempt_number: Option<u32>,
    pub task: Option<TaskId>,
    pub execution: Option<ExecutionId>,
}

/// Why resolution refused to produce an identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    Empty { kind: IdentityKind },
    MalformedRun { value: String },
    GroupingIdEncodesRun { kind: IdentityKind, value: String },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { kind } => write!(formatter, "{kind} must be a nonempty string"),
            Self::MalformedRun { value } => write!(
                formatter,
                "run id `{value}` does not match the `-attempt-<n>-<suffix>` convention"
            ),
            Self::GroupingIdEncodesRun { kind, value } => write!(
                formatter,
                "{kind} `{value}` encodes a run, not a grouping identity"
            ),
        }
    }
}

impl std::error::Error for ResolveError {}

/// Resolve a persisted identity string of a declared kind.
///
/// The kind is part of the input so this function never classifies an
/// untagged string as a cook, run, task, runner job, or fanout portfolio.
pub fn resolve(kind: IdentityKind, value: &str) -> Result<ResolvedIdentities, ResolveError> {
    match kind {
        IdentityKind::CookId | IdentityKind::FanoutPortfolioId => resolve_grouping(kind, value),
        IdentityKind::RunId => resolve_run(value),
        IdentityKind::TaskId => resolve_task(value),
        IdentityKind::RunnerJobId => resolve_execution(value),
    }
}

fn resolve_grouping(kind: IdentityKind, value: &str) -> Result<ResolvedIdentities, ResolveError> {
    require_opaque(kind, value)?;
    if run_parts(value).is_some() {
        return Err(ResolveError::GroupingIdEncodesRun {
            kind,
            value: value.to_string(),
        });
    }
    Ok(ResolvedIdentities {
        mission: Some(MissionId::from_validated(value)),
        run: None,
        attempt: None,
        attempt_number: None,
        task: None,
        execution: None,
    })
}

fn resolve_run(value: &str) -> Result<ResolvedIdentities, ResolveError> {
    require_opaque(IdentityKind::RunId, value)?;
    let Some(parts) = run_parts(value) else {
        return Err(ResolveError::MalformedRun {
            value: value.to_string(),
        });
    };
    Ok(ResolvedIdentities {
        mission: Some(MissionId::from_validated(parts.mission)),
        run: Some(RunId::from_validated(value)),
        attempt: Some(AttemptId::from_validated(value)),
        attempt_number: Some(parts.attempt_number),
        task: None,
        execution: None,
    })
}

fn resolve_task(value: &str) -> Result<ResolvedIdentities, ResolveError> {
    require_opaque(IdentityKind::TaskId, value)?;
    Ok(ResolvedIdentities {
        mission: None,
        run: None,
        attempt: None,
        attempt_number: None,
        task: Some(TaskId::from_validated(value)),
        execution: None,
    })
}

fn resolve_execution(value: &str) -> Result<ResolvedIdentities, ResolveError> {
    require_opaque(IdentityKind::RunnerJobId, value)?;
    Ok(ResolvedIdentities {
        mission: None,
        run: None,
        attempt: None,
        attempt_number: None,
        task: None,
        execution: Some(ExecutionId::from_validated(value)),
    })
}

fn require_opaque(kind: IdentityKind, value: &str) -> Result<(), ResolveError> {
    validate_opaque(value).map_err(|_| ResolveError::Empty { kind })
}

struct RunParts<'a> {
    mission: &'a str,
    attempt_number: u32,
}

/// Split a run id minted as `{cook_id}-attempt-{n}-{suffix}[qualifiers]`.
///
/// Takes the last well-formed `-attempt-<n>-` marker so the mission is the
/// cook id the run was minted from, and trailing qualifiers such as
/// `-transport-retry` stay on the run string rather than being dropped.
fn run_parts(value: &str) -> Option<RunParts<'_>> {
    const MARKER: &str = "-attempt-";
    let mut last = None;
    let mut from = 0;
    while from < value.len() {
        let rest = &value[from..];
        let Some(relative) = rest.find(MARKER) else {
            break;
        };
        let marker_at = from + relative;
        let after = &value[marker_at + MARKER.len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        let number = after.get(..digits).and_then(|raw| {
            if raw.is_empty() || raw.starts_with('0') {
                return None;
            }
            raw.parse::<u32>().ok().filter(|number| *number >= 1)
        });
        let has_suffix = after.len() > digits + 1 && after[digits..].starts_with('-');
        if let Some(attempt_number) = number {
            if has_suffix && marker_at > 0 {
                last = Some(RunParts {
                    mission: &value[..marker_at],
                    attempt_number,
                });
            }
        }
        from = marker_at + 1;
    }
    last
}

#[cfg(test)]
mod tests {
    use super::{resolve, run_parts, IdentityKind, ResolveError};

    const AGENT_TASK_COOK: &str = "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e";
    const AGENT_TASK_RUN: &str =
        "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e-attempt-1-ea6a6751";
    const DETACHED_COOK: &str = "cook-detached-37abbb52-d638-495c-b270-46fdc965fc9c";
    const DETACHED_RUN: &str =
        "cook-detached-37abbb52-d638-495c-b270-46fdc965fc9c-attempt-1-fb890874-transport-retry";

    #[test]
    fn agent_task_run_id_resolves_mission_run_and_attempt_number() {
        let resolved = resolve(IdentityKind::RunId, AGENT_TASK_RUN).expect("run");
        assert_eq!(
            resolved.mission.as_ref().map(|id| id.as_str()),
            Some(AGENT_TASK_COOK)
        );
        assert_eq!(
            resolved.run.as_ref().map(|id| id.as_str()),
            Some(AGENT_TASK_RUN)
        );
        assert_eq!(
            resolved.attempt.as_ref().map(|id| id.as_str()),
            Some(AGENT_TASK_RUN)
        );
        assert_eq!(resolved.attempt_number, Some(1));
        assert!(resolved.task.is_none());
        assert!(resolved.execution.is_none());
    }

    #[test]
    fn detached_transport_retry_run_id_preserves_trailing_qualifiers() {
        let resolved = resolve(IdentityKind::RunId, DETACHED_RUN).expect("detached run");
        assert_eq!(
            resolved.mission.as_ref().map(|id| id.as_str()),
            Some(DETACHED_COOK)
        );
        assert_eq!(
            resolved.run.as_ref().map(|id| id.as_str()),
            Some(DETACHED_RUN)
        );
        assert!(DETACHED_RUN.ends_with("-transport-retry"));
        assert_eq!(resolved.attempt_number, Some(1));
        assert_eq!(
            run_parts(DETACHED_RUN).map(|parts| parts.mission),
            Some(DETACHED_COOK)
        );
    }

    #[test]
    fn bare_cook_id_resolves_to_a_mission_with_no_attempt() {
        for cook_id in [AGENT_TASK_COOK, DETACHED_COOK] {
            let resolved = resolve(IdentityKind::CookId, cook_id).expect("cook");
            assert_eq!(
                resolved.mission.as_ref().map(|id| id.as_str()),
                Some(cook_id)
            );
            assert!(resolved.run.is_none());
            assert!(resolved.attempt.is_none());
            assert!(resolved.attempt_number.is_none());
        }
    }

    #[test]
    fn fanout_portfolio_id_resolves_to_a_mission() {
        let resolved =
            resolve(IdentityKind::FanoutPortfolioId, "production-interface").expect("fanout");
        assert_eq!(
            resolved.mission.as_ref().map(|id| id.as_str()),
            Some("production-interface")
        );
        assert!(resolved.attempt_number.is_none());
    }

    #[test]
    fn task_and_runner_job_ids_resolve_to_their_own_identities() {
        let task = resolve(IdentityKind::TaskId, "cook-static-site-importer").expect("task");
        assert_eq!(
            task.task.as_ref().map(|id| id.as_str()),
            Some("cook-static-site-importer")
        );
        assert!(task.mission.is_none());
        let job = resolve(IdentityKind::RunnerJobId, "accepted-daemon-job").expect("job");
        assert_eq!(
            job.execution.as_ref().map(|id| id.as_str()),
            Some("accepted-daemon-job")
        );
        assert!(job.mission.is_none());
    }

    #[test]
    fn malformed_and_unrecognized_inputs_return_typed_explanations() {
        assert_eq!(
            resolve(IdentityKind::RunId, "").unwrap_err(),
            ResolveError::Empty {
                kind: IdentityKind::RunId
            }
        );
        assert_eq!(
            resolve(IdentityKind::RunId, AGENT_TASK_COOK).unwrap_err(),
            ResolveError::MalformedRun {
                value: AGENT_TASK_COOK.to_string()
            }
        );
        assert_eq!(
            resolve(IdentityKind::RunId, "attempt-1-ea6a6751").unwrap_err(),
            ResolveError::MalformedRun {
                value: "attempt-1-ea6a6751".to_string()
            }
        );
        assert_eq!(
            resolve(
                IdentityKind::RunId,
                "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e-attempt-1"
            )
            .unwrap_err(),
            ResolveError::MalformedRun {
                value: "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e-attempt-1".to_string()
            }
        );
        assert_eq!(
            resolve(
                IdentityKind::RunId,
                "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e-attempt-01-ea6a6751"
            )
            .unwrap_err(),
            ResolveError::MalformedRun {
                value: "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e-attempt-01-ea6a6751"
                    .to_string()
            }
        );
        assert_eq!(
            resolve(IdentityKind::CookId, AGENT_TASK_RUN).unwrap_err(),
            ResolveError::GroupingIdEncodesRun {
                kind: IdentityKind::CookId,
                value: AGENT_TASK_RUN.to_string()
            }
        );
        assert_eq!(
            resolve(IdentityKind::FanoutPortfolioId, DETACHED_RUN).unwrap_err(),
            ResolveError::GroupingIdEncodesRun {
                kind: IdentityKind::FanoutPortfolioId,
                value: DETACHED_RUN.to_string()
            }
        );
    }
}
