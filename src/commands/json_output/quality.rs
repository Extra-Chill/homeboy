use crate::cli_surface::Commands;

use super::{map, JsonRun};
use crate::commands::{
    audit, audit_baseline, bench, fuzz, lint, observe, review, test, trace, GlobalArgs,
};

pub(super) fn dispatch(command: Commands, global: &GlobalArgs) -> JsonRun {
    match command {
        Commands::Test(args) => map(test::run(args, global)),
        Commands::Bench(args) => map(bench::run(args, global)),
        Commands::Fuzz(args) => map(fuzz::run(args, global)),
        Commands::Trace(args) => map(trace::run(args, global)),
        Commands::Observe(args) => map(observe::run(args, global)),
        Commands::Lint(args) => map(lint::run(args, global)),
        Commands::Review(args) => map(review::run(args, global)),
        Commands::Audit(args) => map(audit::run(args, global)),
        Commands::AuditBaseline(args) => map(audit_baseline::run(args, global)),
        _ => unreachable!("command routed to wrong JSON output family"),
    }
}
