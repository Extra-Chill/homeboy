use homeboy::core::observation::runs_service::{self, TerminalRunRetentionOptions};

use super::{CmdResult, RunsOutput, RunsRetentionArgs, RunsRetentionOutput};

pub fn retain_terminal_runs(args: RunsRetentionArgs) -> CmdResult<RunsOutput> {
    let outcome = runs_service::retain_terminal_runs(TerminalRunRetentionOptions {
        apply: args.apply,
        older_than_days: args.older_than_days,
        limit: args.limit,
    })?;
    Ok((
        RunsOutput::Retention(RunsRetentionOutput {
            command: "runs.retention",
            dry_run: outcome.dry_run,
            older_than_days: outcome.older_than_days,
            candidate_run_ids: outcome.candidate_run_ids,
            removed_run_count: outcome.removed_run_count,
        }),
        0,
    ))
}
