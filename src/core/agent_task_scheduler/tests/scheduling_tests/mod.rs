//! Scheduler tests organized by scheduling concern.

mod shared;
use shared::*;

mod adaptive_concurrency;
mod artifact_binding;
mod cancellation;
mod concurrency;
mod plan_projection;
mod provider_rotation;
mod resource_budget;
mod retry_failure;
mod timeout;
