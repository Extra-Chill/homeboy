//! Generic pipeline execution infrastructure.
//!
//! This module provides the core pipeline framework:
//! - `pipeline` - Traits, topological sorting, batch execution
//! - `executor` - Command routing (local vs SSH), CLI tool templating
//!
//! Domain-specific implementations (release, deploy, etc.) use these primitives
//! to build their orchestration logic.

pub mod executor;
pub mod pipeline;
