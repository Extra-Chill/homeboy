// Shared imports and helpers are visible to every child test module.
use super::*;
use std::sync::{Arc, Mutex};

#[path = "tests/environment_and_cache.rs"]
mod environment_and_cache;
#[path = "tests/execution_and_supervision.rs"]
mod execution_and_supervision;
#[path = "tests/failure_and_invocation.rs"]
mod failure_and_invocation;
#[path = "tests/preflight_and_isolation.rs"]
mod preflight_and_isolation;

use failure_and_invocation::*;
