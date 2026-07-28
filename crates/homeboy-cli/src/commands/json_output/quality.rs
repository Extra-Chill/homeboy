use crate::cli_surface::Commands;

use super::{map, JsonRun};
use crate::commands::{bench, fuzz, review, trace};

pub(super) fn dispatch(command: Commands) -> JsonRun {
    match command {
        Commands::Bench(args) => map(bench::run(args)),
        Commands::Fuzz(args) => map(fuzz::run(args)),
        Commands::Trace(args) => map(trace::run(args)),
        Commands::Review(args) => map(review::run(args)),
        _ => unreachable!("command routed to wrong JSON output family"),
    }
}
