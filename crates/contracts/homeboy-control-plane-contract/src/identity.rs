//! Distinct identity newtypes over the opaque strings Homeboy already persists.
//!
//! Each type serializes as its plain string form so existing documents remain
//! readable. The types are not interchangeable: a [`RunId`] cannot be passed
//! where a [`MissionId`] is expected.
//!
//! ```compile_fail
//! use homeboy_control_plane_contract::{MissionId, RunId};
//! fn takes_mission(_: MissionId) {}
//! fn example(run: RunId) {
//!     takes_mission(run);
//! }
//! ```

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! opaque_identity {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wrap a nonempty opaque identity string.
            pub fn new(value: impl Into<String>) -> Result<Self, IdentityError> {
                let value = value.into();
                validate_opaque(&value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[allow(dead_code)]
            pub(crate) fn from_validated(value: impl Into<String>) -> Self {
                Self(value.into())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

opaque_identity!(
    /// Typed name for the existing grouping identity. A Cook id and a fanout
    /// portfolio id both resolve to a mission; this crate does not invent a
    /// third grouping concept.
    MissionId
);
opaque_identity!(
    /// A single Cook attempt's run identity, including any trailing qualifiers
    /// such as `-transport-retry`.
    RunId
);
opaque_identity!(
    /// A plan-level task identity as persisted on agent-task documents.
    TaskId
);
opaque_identity!(
    /// The attempt identity encoded by a run id. The opaque string is the run
    /// id itself; the attempt *number* lives on [`crate::ResolvedIdentities`].
    AttemptId
);
opaque_identity!(
    /// A runner-job / execution identity.
    ExecutionId
);
opaque_identity!(
    /// A provider session identity.
    ProviderSessionId
);
opaque_identity!(
    /// Stable identity of one event in a run stream.
    EventId
);
opaque_identity!(
    /// Opaque resume cursor returned by an event page.
    EventCursor
);

/// Why an identity newtype could not be constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityError {
    Empty,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("control-plane identity must be a nonempty string"),
        }
    }
}

impl std::error::Error for IdentityError {}

pub(crate) fn validate_opaque(value: &str) -> Result<(), IdentityError> {
    if value.is_empty() || value.trim().is_empty() {
        Err(IdentityError::Empty)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{AttemptId, ExecutionId, MissionId, ProviderSessionId, RunId, TaskId};
    use crate::IdentityError;

    #[test]
    fn identity_types_are_not_interchangeable() {
        let names = [
            std::any::type_name::<MissionId>(),
            std::any::type_name::<RunId>(),
            std::any::type_name::<TaskId>(),
            std::any::type_name::<AttemptId>(),
            std::any::type_name::<ExecutionId>(),
            std::any::type_name::<ProviderSessionId>(),
        ];
        for (index, name) in names.iter().enumerate() {
            for other in names.iter().skip(index + 1) {
                assert_ne!(name, other);
            }
        }
    }

    #[test]
    fn identities_serialize_as_plain_strings() {
        let mission =
            MissionId::new("agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e").expect("mission");
        let json = serde_json::to_value(&mission).expect("serialize");
        assert_eq!(
            json,
            serde_json::Value::String(
                "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e".to_string()
            )
        );
        let decoded: MissionId = serde_json::from_value(json).expect("deserialize");
        assert_eq!(decoded, mission);
    }

    #[test]
    fn empty_identity_is_rejected() {
        assert_eq!(MissionId::new("").unwrap_err(), IdentityError::Empty);
        assert_eq!(RunId::new("   ").unwrap_err(), IdentityError::Empty);
    }
}
