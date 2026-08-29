//! Tagged control-plane reference with a stable parse/render string form.

use std::fmt;
use std::str::FromStr;

use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::identity::{
    AttemptId, ExecutionId, IdentityError, MissionId, ProviderSessionId, RunId, TaskId,
};

const KIND_MISSION: &str = "mission";
const KIND_RUN: &str = "run";
const KIND_TASK: &str = "task";
const KIND_ATTEMPT: &str = "attempt";
const KIND_EXECUTION: &str = "execution";
const KIND_PROVIDER_SESSION: &str = "provider-session";

/// One of the control-plane identities, tagged so the kind cannot be lost.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ControlPlaneRef {
    Mission(MissionId),
    Run(RunId),
    Task(TaskId),
    Attempt(AttemptId),
    Execution(ExecutionId),
    ProviderSession(ProviderSessionId),
}

impl ControlPlaneRef {
    pub fn as_str_kind(&self) -> &'static str {
        match self {
            Self::Mission(_) => KIND_MISSION,
            Self::Run(_) => KIND_RUN,
            Self::Task(_) => KIND_TASK,
            Self::Attempt(_) => KIND_ATTEMPT,
            Self::Execution(_) => KIND_EXECUTION,
            Self::ProviderSession(_) => KIND_PROVIDER_SESSION,
        }
    }

    pub fn identity_str(&self) -> &str {
        match self {
            Self::Mission(id) => id.as_str(),
            Self::Run(id) => id.as_str(),
            Self::Task(id) => id.as_str(),
            Self::Attempt(id) => id.as_str(),
            Self::Execution(id) => id.as_str(),
            Self::ProviderSession(id) => id.as_str(),
        }
    }
}

impl fmt::Display for ControlPlaneRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.as_str_kind(), self.identity_str())
    }
}

impl FromStr for ControlPlaneRef {
    type Err = ControlPlaneRefError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() || value.trim().is_empty() {
            return Err(ControlPlaneRefError::Empty);
        }
        let Some((kind, identity)) = value.split_once('/') else {
            return Err(ControlPlaneRefError::Malformed {
                value: value.to_string(),
            });
        };
        if kind.is_empty() || identity.is_empty() {
            return Err(ControlPlaneRefError::Malformed {
                value: value.to_string(),
            });
        }
        match kind {
            KIND_MISSION => Ok(Self::Mission(MissionId::new(identity)?)),
            KIND_RUN => Ok(Self::Run(RunId::new(identity)?)),
            KIND_TASK => Ok(Self::Task(TaskId::new(identity)?)),
            KIND_ATTEMPT => Ok(Self::Attempt(AttemptId::new(identity)?)),
            KIND_EXECUTION => Ok(Self::Execution(ExecutionId::new(identity)?)),
            KIND_PROVIDER_SESSION => Ok(Self::ProviderSession(ProviderSessionId::new(identity)?)),
            _ => Err(ControlPlaneRefError::Unrecognized {
                value: value.to_string(),
            }),
        }
    }
}

impl Serialize for ControlPlaneRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ControlPlaneRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(DeError::custom)
    }
}

/// Why a [`ControlPlaneRef`] string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlPlaneRefError {
    Empty,
    Malformed { value: String },
    Unrecognized { value: String },
    InvalidIdentity(IdentityError),
}

impl fmt::Display for ControlPlaneRefError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("control-plane ref must be a nonempty string"),
            Self::Malformed { value } => {
                write!(
                    formatter,
                    "control-plane ref `{value}` is malformed; expected `<kind>/<identity>`"
                )
            }
            Self::Unrecognized { value } => {
                write!(
                    formatter,
                    "control-plane ref `{value}` has an unrecognized kind"
                )
            }
            Self::InvalidIdentity(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for ControlPlaneRefError {}

impl From<IdentityError> for ControlPlaneRefError {
    fn from(error: IdentityError) -> Self {
        Self::InvalidIdentity(error)
    }
}

#[cfg(test)]
mod tests {
    use super::ControlPlaneRef;
    use crate::{
        AttemptId, ControlPlaneRefError, ExecutionId, MissionId, ProviderSessionId, RunId, TaskId,
    };

    const AGENT_TASK_RUN: &str =
        "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e-attempt-1-ea6a6751";
    const AGENT_TASK_COOK: &str = "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e";
    const DETACHED_RUN: &str =
        "cook-detached-37abbb52-d638-495c-b270-46fdc965fc9c-attempt-1-fb890874-transport-retry";

    fn round_trip(reference: ControlPlaneRef) {
        let rendered = reference.to_string();
        let parsed: ControlPlaneRef = rendered.parse().expect("parse");
        assert_eq!(parsed, reference);
        let json = serde_json::to_value(&reference).expect("serialize");
        assert_eq!(json, serde_json::Value::String(rendered));
        let decoded: ControlPlaneRef = serde_json::from_value(json).expect("deserialize");
        assert_eq!(decoded, reference);
    }

    #[test]
    fn every_identity_round_trips_through_control_plane_ref() {
        round_trip(ControlPlaneRef::Mission(
            MissionId::new(AGENT_TASK_COOK).expect("mission"),
        ));
        round_trip(ControlPlaneRef::Run(
            RunId::new(AGENT_TASK_RUN).expect("run"),
        ));
        round_trip(ControlPlaneRef::Run(
            RunId::new(DETACHED_RUN).expect("detached run"),
        ));
        round_trip(ControlPlaneRef::Task(
            TaskId::new("cook-static-site-importer").expect("task"),
        ));
        round_trip(ControlPlaneRef::Attempt(
            AttemptId::new(AGENT_TASK_RUN).expect("attempt"),
        ));
        round_trip(ControlPlaneRef::Execution(
            ExecutionId::new("accepted-daemon-job").expect("execution"),
        ));
        round_trip(ControlPlaneRef::ProviderSession(
            ProviderSessionId::new("session-123").expect("session"),
        ));
    }

    #[test]
    fn unrecognized_and_malformed_refs_fail() {
        assert_eq!(
            "".parse::<ControlPlaneRef>().unwrap_err(),
            ControlPlaneRefError::Empty
        );
        assert!(matches!(
            "run".parse::<ControlPlaneRef>().unwrap_err(),
            ControlPlaneRefError::Malformed { .. }
        ));
        assert!(matches!(
            "run/".parse::<ControlPlaneRef>().unwrap_err(),
            ControlPlaneRefError::Malformed { .. }
        ));
        assert!(matches!(
            "widget/agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e"
                .parse::<ControlPlaneRef>()
                .unwrap_err(),
            ControlPlaneRefError::Unrecognized { .. }
        ));
        assert!(matches!(
            "provider_session/session-123"
                .parse::<ControlPlaneRef>()
                .unwrap_err(),
            ControlPlaneRefError::Unrecognized { .. }
        ));
    }
}
