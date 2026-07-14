//! Engine primitives: filesystem I/O, command execution, refactor helpers,
//! lint/test runners, and other cross-cutting infrastructure used by domain
//! modules (release, deploy, audit, refactor, …).

// Leaf execution primitives moved to the internal `homeboy-engine-primitives`
// crate. Re-exported here so existing `crate::core::engine::{shell,command,...}`
// call sites keep working unchanged.
pub use homeboy_engine_primitives::{
    command, identifier, output_parse, shell, template, text,
};

pub mod baseline;
pub mod cli_tool;
pub mod codebase_scan;
pub mod detail_output;
pub mod edit_op;
pub mod edit_op_apply;
pub mod execution_context;
pub mod executor;
pub mod format_write;
pub mod hooks;
pub mod invocation;
pub(crate) mod local_files;
pub mod refactor_primitive;
pub mod resource;
pub mod run_dir;
pub mod symbol_graph;
pub mod temp;
pub mod undo;
pub mod validate_write;
pub mod validation;
