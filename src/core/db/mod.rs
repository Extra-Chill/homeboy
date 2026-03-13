//! Database operations for homeboy projects.
//!
//! Two subsystems:
//! - **Operations**: Query, search, list/describe tables, delete rows, drop tables
//!   via extension-defined CLI commands.
//! - **Tunnel**: SSH tunnel for forwarding local ports to remote databases.

mod operations;
mod tunnel;

// Re-export everything at module level to preserve existing import paths.
pub use operations::{
    delete_row, describe_table, drop_table, list_tables, query, search, DbResult,
};
pub use tunnel::{create_tunnel, DbTunnelInfo, DbTunnelResult};
