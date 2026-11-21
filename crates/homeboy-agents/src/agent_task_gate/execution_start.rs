//! Replacement execution fencing belongs at command start, not gate admission.

use std::cell::RefCell;
use std::rc::Rc;

use homeboy_core::Result;

type Start = Box<dyn FnOnce() -> Result<()>>;
type Submitted = Rc<dyn Fn(&str, &str) -> Result<()>>;

thread_local! {
    static BEFORE_COMMAND: RefCell<Option<Start>> = RefCell::new(None);
    static ON_JOB_SUBMITTED: RefCell<Option<Submitted>> = RefCell::new(None);
}

pub(crate) fn with_command_start<T>(start: Start, operation: impl FnOnce() -> T) -> T {
    struct Restore(Option<Start>);
    impl Drop for Restore {
        fn drop(&mut self) {
            BEFORE_COMMAND.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(BEFORE_COMMAND.with(|slot| slot.replace(Some(start))));
    operation()
}

/// Called only after refusable local preflight, immediately before spawning a
/// gate (or handing it off to a remote execution owner). At most once per round.
pub fn before_command_start() -> Result<()> {
    let start = BEFORE_COMMAND.with(|slot| slot.borrow_mut().take());
    if let Some(start) = start {
        start()?;
    }
    Ok(())
}

/// Run `operation` with `submitted` recording each runner job a gate hands
/// to a remote daemon, so recovery can find the job after a controller that
/// died before it saw the job start (#18709).
pub(crate) fn with_job_submission<T>(submitted: Submitted, operation: impl FnOnce() -> T) -> T {
    struct Restore(Option<Submitted>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ON_JOB_SUBMITTED.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(ON_JOB_SUBMITTED.with(|slot| slot.replace(Some(submitted))));
    operation()
}

/// Called once a remote daemon has accepted a gate job, before the controller
/// waits for it to leave the daemon's queue. The daemon records the job's
/// start itself; this durable job id is how recovery reads that record.
pub fn job_submitted(runner_id: &str, job_id: &str) -> Result<()> {
    let submitted = ON_JOB_SUBMITTED.with(|slot| slot.borrow().clone());
    match submitted {
        Some(submitted) => submitted(runner_id, job_id),
        None => Ok(()),
    }
}
