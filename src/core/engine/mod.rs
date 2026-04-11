//! Generic pipeline execution infrastructure.
//!
//! This extension provides the core pipeline framework:
//! - `pipeline` - Traits, topological sorting, batch execution
//! - `executor` - Command routing (local vs SSH), CLI tool templating
//!
//! Domain-specific implementations (release, deploy, etc.) use these primitives
//! to build their orchestration logic.

pub mod baseline;
pub mod cli_tool;
pub mod codebase_scan;
pub mod command;
pub mod contract;
pub mod contract_extract;
pub mod contract_testgen;
pub mod edit_op;
pub mod edit_op_apply;
pub mod execution_context;
pub mod executor;
pub mod format_write;
pub mod hooks;
pub mod identifier;
pub(crate) mod local_files;
pub mod output_parse;
pub mod pipeline;
pub mod refactor_primitive;
pub mod run_dir;
pub mod shell;
pub mod symbol_graph;
pub mod temp;
pub mod template;
pub mod text;
pub mod undo;
pub mod validate_write;
pub mod validation;
