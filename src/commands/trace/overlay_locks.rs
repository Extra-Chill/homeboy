use homeboy::core::extension::trace as extension_trace;
use homeboy::core::extension::trace::TraceCommandOutput;

use super::TraceArgs;
use crate::commands::CmdResult;

pub(super) fn run_overlay_locks(args: TraceArgs) -> CmdResult<TraceCommandOutput> {
    match args.scenario.as_deref() {
        Some("list") => {
            let locks = extension_trace::list_trace_overlay_locks()?;
            let output = overlay_locks_output(locks);
            Ok((TraceCommandOutput::OverlayLocks(output), 0))
        }
        Some("cleanup") => {
            if !args.stale {
                return Err(homeboy::core::Error::validation_invalid_argument(
                    "--stale",
                    "trace overlay lock cleanup requires --stale",
                    None,
                    None,
                ));
            }
            let result = extension_trace::cleanup_stale_trace_overlay_locks(args.force)?;
            let output = overlay_locks_output(result.removed);
            Ok((TraceCommandOutput::OverlayLocks(output), 0))
        }
        Some(other) => Err(homeboy::core::Error::validation_invalid_argument(
            "overlay-locks",
            format!("unsupported trace overlay-locks command `{other}`"),
            None,
            Some(vec!["list".to_string(), "cleanup --stale".to_string()]),
        )),
        None => Err(homeboy::core::Error::validation_missing_argument(vec![
            "overlay-locks command".to_string(),
        ])),
    }
}

fn overlay_locks_output(
    locks: Vec<extension_trace::TraceOverlayLockRecord>,
) -> extension_trace::TraceOverlayLocksOutput {
    let active_count = locks
        .iter()
        .filter(|lock| lock.status == extension_trace::TraceOverlayLockStatus::Active)
        .count();
    let stale_count = locks
        .iter()
        .filter(|lock| lock.status == extension_trace::TraceOverlayLockStatus::Stale)
        .count();
    let unknown_count = locks
        .iter()
        .filter(|lock| lock.status == extension_trace::TraceOverlayLockStatus::Unknown)
        .count();
    extension_trace::TraceOverlayLocksOutput {
        command: "trace.overlay-locks",
        count: locks.len(),
        active_count,
        stale_count,
        unknown_count,
        locks,
    }
}
