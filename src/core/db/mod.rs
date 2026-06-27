//! Database operations for homeboy projects.
//!
//! Two subsystems:
//! - **Operations**: Query, search, list/describe tables, delete rows, drop tables
//!   via extension-defined CLI commands.
//! - **SSH forward**: ad-hoc SSH port-forward for connecting local ports to
//!   remote databases (distinct from the `core/tunnel` service-tunnel entity).

mod operations;
mod ssh_forward;

// Re-export everything at module level to preserve existing import paths.
pub use operations::{
    delete_row, describe_table, drop_table, list_tables, query, search, DbResult,
};
pub use ssh_forward::{create_tunnel, DbTunnelInfo, DbTunnelResult};
