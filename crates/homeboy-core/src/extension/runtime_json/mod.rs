//! Native JSON emission for the core-shipped extension runtime shell helpers.
//!
//! `sidecar-writer.sh`, `emit-test-failure.sh`, and `bench-helper.sh` call the
//! running Homeboy binary (exported as `HOMEBOY_RUNTIME_EXECUTABLE`) through
//! the private `__homeboy_runtime_json` child protocol instead of requiring a
//! script interpreter on every host that runs an extension.

pub mod bench;
mod json;
pub mod sidecar;
pub mod test_failure;

pub use json::Json;

/// argv[1] that selects the runtime JSON child protocol.
pub const CHILD_COMMAND: &str = "__homeboy_runtime_json";

#[cfg(test)]
mod tests;
