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
    /// Runs once per deploy invocation, after the last component, for a
    /// project whose deploy included at least one component. Unlike
    /// `post:deploy`, this event is not resolved per component and is not
    /// merged from extension manifests — it is declared only on the project
    /// (or another deploy-target-scoped config), because it names a property
    /// of the deploy *target*, not a component or a platform. See
    /// `crates/homeboy-core/src/engine/hooks.rs` module docs.
    #[serde(rename = "post:deploy:project")]
    PostDeployProject,
}

impl HookEvent {
    pub const fn label(self) -> &'static str {
        match self {
            HookEvent::PreVersionBump => "pre:version:bump",
            HookEvent::PostVersionBump => "post:version:bump",
            HookEvent::PostRelease => "post:release",
            HookEvent::PostDeploy => "post:deploy",
            HookEvent::PostDeployProject => "post:deploy:project",
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

    /// Hook map keys are the manifest wire contract: every event round-trips
    /// under its fixed label, `Display` matches that label, and an event
    /// Homeboy never emits is rejected instead of loading as inert config.
    #[test]
    fn hook_map_keys_are_a_closed_set_of_fixed_wire_labels() {
        let events = [
            (HookEvent::PreVersionBump, "pre:version:bump"),
            (HookEvent::PostVersionBump, "post:version:bump"),
            (HookEvent::PostRelease, "post:release"),
            (HookEvent::PostDeploy, "post:deploy"),
            (HookEvent::PostDeployProject, "post:deploy:project"),
        ];
        let hooks: HashMap<HookEvent, Vec<String>> = events
            .iter()
            .map(|(event, label)| (*event, vec![label.to_string()]))
            .collect();
        let serialized = serde_json::to_value(&hooks).expect("serialize hooks");

        for (event, label) in events {
            assert_eq!(event.to_string(), label);
            assert_eq!(serialized[label], serde_json::json!([label]));
        }
        assert_eq!(
            serde_json::from_value::<HashMap<HookEvent, Vec<String>>>(serialized)
                .expect("deserialize hooks"),
            hooks
        );

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
