//! Replacement execution fencing belongs at command start, not gate admission.

use std::cell::RefCell;

use homeboy_core::Result;

type Start = Box<dyn FnOnce() -> Result<()>>;

thread_local! {
    static BEFORE_COMMAND: RefCell<Option<Start>> = RefCell::new(None);
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
