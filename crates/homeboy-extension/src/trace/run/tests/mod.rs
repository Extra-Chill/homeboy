//! Trace run workflow tests, grouped by concern.
//!
//! Two self-contained submodules preserve the original isolated test fixtures
//! (each previously its own `mod`): extension/runner/overlay coverage and the
//! workflow/provenance/baseline coverage.

mod extension;
mod workflow;
