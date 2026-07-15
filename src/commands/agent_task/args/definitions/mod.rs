//! Clap argument and subcommand definitions for the `agent-task` command tree.

mod command;
mod cook;
mod fanout;
mod lifecycle;

pub use command::*;
pub use cook::*;
pub use fanout::*;
pub use lifecycle::*;
