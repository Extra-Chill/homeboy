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
        ($(($module:ident, $variant:ident, $handler:path),)*) => {
            match command {
                $(Commands::$variant(args) => map($handler(args)),)*
                _ => unreachable!("command routed to wrong JSON output family"),
            }
        };
    }

    crate::ops_command_descriptors!(registered_ops_dispatch)
}

#[cfg(test)]
mod tests {
    use crate::command_contract::{registered_command_json_family, CommandJsonFamily};

    /// `ops_command_descriptors!` no longer carries a copy of each row's
    /// `CommandSpec`; the specs live once in `ops_command_spec!`, which
    /// `command_contract::spec` splices into `COMMANDS`. Nothing in the type
    /// system ties the two tables together, so assert the invariant the deleted
    /// duplicate column only ever documented: every module in the ops descriptor
    /// table is registered, and registered in the `Ops` JSON family — which is
    /// what routes it back into this dispatcher.
    #[test]
    fn every_ops_descriptor_is_registered_in_the_ops_json_family() {
        macro_rules! assert_registered_as_ops {
            ($(($module:ident, $variant:ident, $handler:path),)*) => {
                $({
                    // Module names are the command names, except `self_cmd`,
                    // which exists only because `self` is a Rust keyword.
                    let module = stringify!($module);
                    let name = if module == "self_cmd" { "self" } else { module };

                    assert_eq!(
                        registered_command_json_family(name),
                        Some(CommandJsonFamily::Ops),
                        "ops descriptor `{name}` is missing from COMMAND_SPECS or is \
                         registered outside the Ops JSON family, so it would never \
                         reach the ops dispatcher",
                    );
                })*
            };
        }

        crate::ops_command_descriptors!(assert_registered_as_ops);
    }
}
