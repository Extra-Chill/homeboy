pub(super) mod aggregate_construction;
pub(super) mod artifact_portability;
pub(super) mod command_status_contracts;
pub(super) mod config_key_usage;
pub(super) mod core_boundary_leak;
pub(super) mod dead_guard;
pub(super) mod deprecation_age;
pub(super) mod enum_dispatch_contracts;
pub(super) mod facade_passthrough;
pub(super) mod field_patterns;
pub(super) mod global_env_guard;
pub(super) mod layer_ownership;
pub(super) mod mutating_resource_access;
pub(super) mod parallel_runner_setup;
pub(super) mod public_registry_exposure;
pub(super) mod redirect_validation;
pub(super) mod repeated_literal_shape;
pub(super) mod requested_detectors;
pub(super) mod runner_offload_preflight;
pub(super) mod shared_scaffolding;
pub(super) mod test_coverage;
pub(super) mod test_topology;
pub(super) mod test_vacuity;
pub(super) mod test_wiring;
pub(super) mod unbounded_output_capture;
pub(super) mod upstream_workaround;
pub(super) mod wrapper_inference;

pub(super) use super::{
    comment_blocks, conventions, findings, fingerprint, idiomatic, requirements, source_locations,
    test_mapping, walker,
};
