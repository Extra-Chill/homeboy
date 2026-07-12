use crate::cli_surface::Commands;

use super::{map, JsonRun};
use crate::commands::GlobalArgs;

pub(super) fn dispatch(command: Commands, global: &GlobalArgs) -> JsonRun {
    match command {
        Commands::Deps(args) => map(args.run()),
        command => dispatch_registered(command, global),
    }
}

fn dispatch_registered(command: Commands, global: &GlobalArgs) -> JsonRun {
    macro_rules! registered_ops_dispatch {
        ($(($module:ident, $variant:ident, $args:path, $spec:expr, $handler:path),)*) => {
            match command {
                $(Commands::$variant(args) => map($handler(args, global)),)*
                _ => unreachable!("command routed to wrong JSON output family"),
            }
        };
    }

    crate::ops_command_descriptors!(registered_ops_dispatch)
}
