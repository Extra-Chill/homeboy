//! Engine primitives: filesystem I/O, command execution, refactor helpers,
//! lint/test runners, and other cross-cutting infrastructure used by domain
//! modules (release, deploy, audit, refactor, …).

// Leaf execution primitives moved to the internal `homeboy-engine-primitives`
// crate. Re-exported here so existing `crate::engine::{shell,command,...}`
// call sites keep working unchanged.
pub use homeboy_engine_primitives::{
    baseline, codebase_scan, command, detail_output, edit_op, edit_op_apply, identifier, language,
    output_parse, shell, template, text, validation,
};
// local_files was `pub(crate)` in-tree; preserve that visibility across the
// crate boundary rather than widening it via the `pub use` above.
pub use homeboy_engine_primitives::local_files;

pub mod cli_tool;
pub mod execution_context;
pub mod executor;
pub mod format_write;
pub mod hooks;
pub mod invocation;
pub mod resource;
pub mod run_dir;
pub mod symbol_graph;
pub mod temp;
pub mod undo;
