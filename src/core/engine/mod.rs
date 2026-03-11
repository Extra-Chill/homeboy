//! Generic pipeline execution infrastructure.
//!
//! This extension provides the core pipeline framework:
//! - `pipeline` - Traits, topological sorting, batch execution
//! - `executor` - Command routing (local vs SSH), CLI tool templating
//!
//! Domain-specific implementations (release, deploy, etc.) use these primitives
//! to build their orchestration logic.

pub mod baseline;
pub mod command;
pub mod codebase_scan;
pub mod executor;
pub mod output_parse;
pub mod pipeline;
pub mod shell;
pub mod symbol_graph;
pub mod template;
pub mod temp;
pub mod validation;
