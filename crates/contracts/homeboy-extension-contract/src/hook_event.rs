//! Lifecycle events extensions and components may hook.
//!
//! This is a closed set because only Homeboy can emit lifecycle events. A
//! free-form event name that Homeboy never emits is inert configuration, so
//! adding an event requires an explicit contract change and a real emitter.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookEvent {
    /// Runs after version targets are updated, before git commit.
    #[serde(rename = "pre:version:bump")]
    PreVersionBump,
    /// Runs after pre-bump hooks, before git commit.
    #[serde(rename = "post:version:bump")]
    PostVersionBump,
    /// Runs after the release pipeline completes.
    #[serde(rename = "post:release")]
    PostRelease,
    /// Runs after deploy completes.
    #[serde(rename = "post:deploy")]
    PostDeploy,
}

impl HookEvent {
    pub const fn label(self) -> &'static str {
        match self {
            HookEvent::PreVersionBump => "pre:version:bump",
            HookEvent::PostVersionBump => "post:version:bump",
            HookEvent::PostRelease => "post:release",
            HookEvent::PostDeploy => "post:deploy",
        }
    }
}

impl std::fmt::Display for HookEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn hook_map_keeps_existing_wire_labels() {
        let hooks = HashMap::from([
            (HookEvent::PreVersionBump, vec!["pre".to_string()]),
            (HookEvent::PostVersionBump, vec!["post".to_string()]),
            (HookEvent::PostRelease, vec!["release".to_string()]),
            (HookEvent::PostDeploy, vec!["deploy".to_string()]),
        ]);
        let serialized = serde_json::to_value(&hooks).expect("serialize hooks");

        assert_eq!(
            serialized,
            serde_json::json!({
                "pre:version:bump": ["pre"],
                "post:version:bump": ["post"],
                "post:release": ["release"],
                "post:deploy": ["deploy"]
            })
        );
        assert_eq!(
            serde_json::from_value::<HashMap<HookEvent, Vec<String>>>(serialized)
                .expect("deserialize hooks"),
            hooks
        );
    }

    #[test]
    fn unknown_hook_event_is_not_silently_inert() {
        let error = serde_json::from_value::<HashMap<HookEvent, Vec<String>>>(
            serde_json::json!({ "post:deply": ["wp cache flush"] }),
        )
        .expect_err("Homeboy cannot emit an unknown hook event");

        assert!(
            error.to_string().contains("post:deply"),
            "error should name the inert event: {error}"
        );
    }
}
