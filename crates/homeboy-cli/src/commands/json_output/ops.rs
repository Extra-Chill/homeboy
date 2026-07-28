use crate::cli_surface::Commands;

use super::{map, JsonRun};

pub(super) fn dispatch(command: Commands) -> JsonRun {
    match command {
        Commands::Deps(args) => map(args.run()),
        command => dispatch_registered(command),
    }
}

fn dispatch_registered(command: Commands) -> JsonRun {
    macro_rules! registered_ops_dispatch {
        ($(($module:ident, $variant:ident, $args:path, $spec:expr, $handler:path),)*) => {
            match command {
                $(Commands::$variant(args) => map($handler(args)),)*
                _ => unreachable!("command routed to wrong JSON output family"),
            }
        };
    }

    crate::ops_command_descriptors!(registered_ops_dispatch)
}
