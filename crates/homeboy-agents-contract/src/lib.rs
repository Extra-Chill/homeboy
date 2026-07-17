//! Shared agent-task provider source-root type.
//!
//! Read by both `homeboy-core` (the agent-runtime manifest, which reads a
//! source root's git ref for immutability checks and remote-drift diagnostics)
//! and `homeboy-agents`. It sits below core so neither crate needs the other
//! for this type.

mod provider_source_types;

pub use provider_source_types::AgentTaskProviderRunnerSource;
