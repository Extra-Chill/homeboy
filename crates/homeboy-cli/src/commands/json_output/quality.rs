use crate::cli_surface::Commands;

use super::{map, JsonRun};
use crate::commands::{bench, fuzz, review, trace, GlobalArgs};

pub(super) fn dispatch(command: Commands, global: &GlobalArgs) -> JsonRun {
    match command {
        Commands::Bench(args) => map(bench::run(args, global)),
        Commands::Fuzz(args) => map(fuzz::run(args, global)),
        Commands::Trace(args) => map(trace::run(args, global)),
        Commands::Review(args) => map(review::run(args, global)),
        _ => unreachable!("command routed to wrong JSON output family"),
    }
}
