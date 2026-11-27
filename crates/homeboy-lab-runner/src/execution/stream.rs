//! Invocation-scoped observation of attached daemon output. The watch cursor
//! owns deduplication; observers never alter the durable event log.
use homeboy_core::api_jobs::{JobEvent, JobEventKind};
use std::cell::RefCell;

type Observer = Box<dyn Fn(bool, &str)>;
thread_local! {
    static OBSERVER: RefCell<Option<Observer>> = RefCell::new(None);
}

/// Observe stdout (`false`) and stderr (`true`) chunks during a synchronous
/// runner invocation. Restores the previous observer even if the call unwinds.
pub fn with_exec_stream<T>(observer: impl Fn(bool, &str) + 'static, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<Observer>);
    impl Drop for Restore {
        fn drop(&mut self) {
            OBSERVER.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(OBSERVER.with(|slot| slot.replace(Some(Box::new(observer)))));
    run()
}

pub(super) fn emit(event: &JobEvent) {
    if event.kind == JobEventKind::Progress {
        if let Some(data) = &event.data {
            if data["phase"] == "command_output" {
                if let Some(text) = data["text"].as_str() {
                    OBSERVER.with(|slot| {
                        if let Some(observer) = slot.borrow().as_ref() {
                            observer(data["stderr"].as_bool().unwrap_or(false), text);
                        }
                    });
                }
            }
        }
        return;
    }
    // Stdout/Stderr events can be terminal full-stream snapshots. The final
    // exec result is the fallback for older daemons without chunk progress.
}
