//! Optional service-tunnel feature for homeboy.
//!
//! Provides tunnel declarations (`ServiceTunnel`), lifecycle (start/stop/status),
//! readiness checks, and native preview consumption. Sits on top of the
//! `homeboy-core` engine; core does not depend on this crate.
//!
//! Because core owns the config-entity collision invariant but must not depend
//! on this optional feature, the hosting binary calls [`register`] once at
//! startup so `ServiceTunnel` participates in cross-entity ID/alias collision
//! detection.

mod entity;
mod lifecycle;
mod preview;
pub mod preview_consumer;
mod readiness;
mod runtime;
mod types;
mod validation;

pub use entity::{expose, native_preview_token_record, native_preview_token_sha256};
pub use lifecycle::{local_url, start, status, stop};
pub use preview::validate_native_preview_claim;
pub use types::*;

// Re-exported for sibling `tunnel_tests` integration coverage under `core`,
// which pulls these in via `use super::tunnel::*`. The glob consumer is not
// seen by the unused-imports lint, so the allow keeps a clean non-test build.
#[allow(unused_imports)]
pub(crate) use preview::{preview_artifact_for, preview_policy_allows};

homeboy_core::entity_crud!(ServiceTunnel; list_ids, merge);

/// Register this feature crate's config entity with core. Call once at binary
/// startup. Idempotent. Without this, core's cross-entity ID/alias collision
/// checks would silently omit `ServiceTunnel`.
pub fn register() {
    homeboy_core::config::register_config_entity::<ServiceTunnel>();
}

#[cfg(test)]
mod register_tests {
    /// Guards the collision invariant: after `register()`, `ServiceTunnel` must
    /// participate in core's cross-entity ID/alias collision detection. If the
    /// registration wiring is ever dropped, this fails instead of silently
    /// weakening collision checks in production.
    #[test]
    fn register_adds_service_tunnel_to_collision_detection() {
        super::register();
        let types = homeboy_core::config::registered_config_entity_types();
        assert!(
            types.contains(&"service_tunnel"),
            "ServiceTunnel not registered for collision detection; registered types: {types:?}"
        );
    }

    /// Registration is idempotent — calling it twice must not duplicate the
    /// entity in the registry.
    #[test]
    fn register_is_idempotent() {
        super::register();
        super::register();
        let count = homeboy_core::config::registered_config_entity_types()
            .into_iter()
            .filter(|entity_type| *entity_type == "service_tunnel")
            .count();
        assert_eq!(count, 1, "ServiceTunnel registered more than once");
    }
}
