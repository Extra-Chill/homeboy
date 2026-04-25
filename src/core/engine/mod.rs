//! Engine primitives: filesystem I/O, command execution, refactor helpers,
//! lint/test runners, and other cross-cutting infrastructure used by domain
//! modules (release, deploy, audit, refactor, …).

pub mod baseline;
pub mod cli_tool;
pub mod codebase_scan;
pub mod command;
pub mod edit_op;
pub mod edit_op_apply;
pub mod execution_context;
pub mod executor;
pub mod format_write;
pub mod hooks;
pub mod identifier;
pub(crate) mod local_files;
pub mod output_parse;
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
