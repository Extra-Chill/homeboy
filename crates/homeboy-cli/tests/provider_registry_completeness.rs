//! Startup provider-registration completeness.
//!
//! Homeboy inverts most subsystem behavior behind `provider_registry!` hooks: a
//! leaf layer declares a trait plus a no-op, an owning layer registers the real
//! implementation at CLI startup, and the leaf dispatches through a
//! process-global slot. When the slot is empty the accessor falls back to the
//! no-op, which means **deleting a `register_*()` line at startup produces no
//! error, no log, and no failing test** — the subsystem just stops doing
//! anything. Adding a brand-new registry and forgetting to wire it is the same
//! failure with the same silence.
//!
//! Every `provider_registry!` / `provider_registry_arc!` expansion now submits a
//! descriptor to a link-time inventory, so the set of registries is derived from
//! the declaration sites rather than hand-listed here. This test drives the
//! binary's real startup wiring (`register_all_providers`, the same sequence
//! `CliRuntime::run_from_args` performs) and then asserts every declared
//! registry is populated.
//!
//! It lives in its own integration-test binary on purpose: registration is
//! process-global and irreversible, so wiring every production provider inside
//! the shared lib-test process would leak into unrelated tests.

use std::collections::BTreeSet;

use homeboy_engine_primitives::provider_registry::{
    declared_provider_registries, unregistered_provider_registry_ids,
};

/// A registry that a full startup pass is allowed to leave empty, with the
/// reason it is allowed.
struct OptionalRegistry {
    id: &'static str,
    reason: &'static str,
}

/// The only registries permitted to be empty after `register_all_providers`.
///
/// The completeness assertion is a subset check, so removing an entry here is
/// how a registry graduates to "must be wired", and wiring one of these up later
/// shrinks the unregistered set without breaking the test. Adding an entry is
/// the deliberate, reviewable act of accepting an inert subsystem.
const OPTIONAL_REGISTRIES: &[OptionalRegistry] = &[
    OptionalRegistry {
        id: "homeboy_agents::agent_task_lifecycle::acceptance_verifier::register_acceptance_verifier",
        reason: "Conditional by design. `register_acceptance_verifier_from_config` returns \
                 Ok(()) without registering when `agent_task.acceptance_verifier` is unset, \
                 which is the default. Tests inject their own verifier through the registry.",
    },
    OptionalRegistry {
        id: "homeboy_code_audit::compiler_warning_provider::register_compiler_warning_provider",
        reason: "NOT WIRED (found while closing #11133). \
                 `homeboy_extension::audit_compiler_warning_provider::register` exists and its \
                 doc comment claims it is \"called once at binary startup by the CLI\", but no \
                 caller exists anywhere in the workspace, so audit's compiler-warning provider \
                 is inert in production and silently returns the no-op. Recorded here rather \
                 than fixed because wiring it changes audit behavior, which is out of scope for \
                 the detection fix. Removing this entry is the fix.",
    },
];

/// A registry that must exist in the inventory for this test to mean anything.
/// The inventory is collected at link time; a collection that silently yielded
/// nothing would make the completeness assertion vacuously true.
const SENTINEL_REGISTRY: &str = "homeboy_core::stack_provider::register_stack_provider";

/// Floor on the number of declared registries. Deliberately far below the real
/// count so crate extraction does not churn it — this only has to catch an
/// inventory that collected nothing or almost nothing.
const MINIMUM_DECLARED_REGISTRIES: usize = 20;

#[test]
fn cli_startup_registers_every_declared_provider_registry() {
    homeboy_core::test_support::with_isolated_home(|_home| {
        let declared = declared_provider_registries();
        let declared_ids = declared
            .iter()
            .map(|registry| registry.id())
            .collect::<Vec<_>>();

        assert!(
            declared.len() >= MINIMUM_DECLARED_REGISTRIES,
            "the link-time provider-registry inventory collected only {} descriptor(s); \
             expected at least {}. Either the inventory is no longer being linked in or the \
             macros stopped submitting descriptors — every assertion below is vacuous until \
             this holds. Collected: {declared_ids:?}",
            declared.len(),
            MINIMUM_DECLARED_REGISTRIES,
        );
        assert!(
            declared_ids.iter().any(|id| id == SENTINEL_REGISTRY),
            "expected the inventory to contain {SENTINEL_REGISTRY}; collected {declared_ids:?}",
        );

        let mut unique = declared_ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            declared_ids.len(),
            "provider-registry ids must be unique; a module hosts at most one registry",
        );

        // Nothing has registered yet, so whatever is populated after the call
        // below is populated *because of* the call.
        assert!(
            unregistered_provider_registry_ids()
                .iter()
                .any(|id| id == SENTINEL_REGISTRY),
            "expected {SENTINEL_REGISTRY} to be empty before startup wiring runs",
        );

        homeboy_cli::cli_runtime::register_all_providers(
            &homeboy_core::defaults::AgentTaskConfig::default(),
        )
        .expect("startup provider registration succeeds under the default agent-task config");

        let optional = OPTIONAL_REGISTRIES
            .iter()
            .map(|registry| registry.id)
            .collect::<BTreeSet<_>>();
        let inert = unregistered_provider_registry_ids()
            .into_iter()
            .filter(|id| !optional.contains(id.as_str()))
            .collect::<Vec<_>>();

        assert!(
            inert.is_empty(),
            "{} provider registr{} declared but never registered by \
             `cli_runtime::register_all_providers`:\n  {}\n\n\
             An empty registry is not an error at runtime — the accessor falls back to the \
             no-op and the subsystem silently does nothing. Either add the missing \
             `register_*()` call to `register_all_providers` (in the order the subsystem \
             needs), or, if the registry is intentionally conditional, add it to \
             OPTIONAL_REGISTRIES in this file with the reason.",
            inert.len(),
            if inert.len() == 1 { "y is" } else { "ies are" },
            inert.join("\n  "),
        );

        // Every allowlist entry must still name a real registry, so the list
        // cannot rot into a set of ids nothing declares anymore.
        for registry in OPTIONAL_REGISTRIES {
            assert!(
                declared_ids.iter().any(|id| id == registry.id),
                "OPTIONAL_REGISTRIES names {}, which no registry declares anymore. \
                 Recorded reason: {} Remove the entry.",
                registry.id,
                registry.reason,
            );
        }
    });
}
