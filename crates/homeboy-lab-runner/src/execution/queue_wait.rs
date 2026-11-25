//! Foreground request-local queue policy. Never exported into child environments.
use std::cell::RefCell;
use std::time::{Duration, Instant};

use homeboy_core::{Error, ErrorCode, Result};
use serde_json::{json, Value};

thread_local! {
    static WAIT: RefCell<Option<QueueWait>> = const { RefCell::new(None) };
}

struct QueueWait {
    started: Instant,
    limit: Option<Duration>,
    last: Option<String>,
    progress: std::sync::mpsc::Sender<Option<String>>,
}

pub(super) struct QueueWaitScope {
    previous: Option<QueueWait>,
    heartbeat: Option<std::thread::JoinHandle<()>>,
}
impl QueueWaitScope {
    pub(super) fn enter(limit: Option<Duration>) -> Self {
        let started = Instant::now();
        let (progress, receiver) = std::sync::mpsc::channel::<Option<String>>();
        let heartbeat = std::thread::spawn(move || {
            let mut status = Some("submitting: awaiting runner admission".to_string());
            loop {
                match receiver.recv_timeout(Duration::from_secs(10)) {
                    Ok(next) => status = next,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if let Some(status) = &status {
                            eprintln!("{status}, waited {}s", started.elapsed().as_secs());
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        Self {
            previous: WAIT.with(|slot| {
                slot.replace(Some(QueueWait {
                    started,
                    limit,
                    last: None,
                    progress,
                }))
            }),
            heartbeat: Some(heartbeat),
        }
    }
}
impl Drop for QueueWaitScope {
    fn drop(&mut self) {
        WAIT.with(|slot| {
            slot.replace(self.previous.take());
        });
        if let Some(heartbeat) = self.heartbeat.take() {
            let _ = heartbeat.join();
        }
    }
}

pub(super) fn no_wait() -> bool {
    WAIT.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|wait| wait.limit == Some(Duration::ZERO))
    })
}

pub(super) fn observe(queued: bool, admission: &Value, job_id: &str) -> Result<()> {
    WAIT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(wait) = slot.as_mut() else { return Ok(()) };
        if !queued {
            if wait.last.take().is_some() { eprintln!("admitted: job {job_id}"); }
            let _ = wait.progress.send(None);
            return Ok(());
        }
        let reason = admission["pending_admission_reason"].as_str().unwrap_or("scheduler capacity");
        let pressure = &admission["load"];
        let load = pressure["observed_load_per_cpu"].as_u64().map(|load| {
            let limit = admission["budget"]["load_per_cpu"].as_u64().unwrap_or(100);
            format!(" (load/cpu {:.1}, limit {:.1}; PSI cpu/io/memory {}/{}/{} hundredths of percent)",
                load as f64 / 10_000.0, limit as f64 / 100.0,
                pressure["cpu"], pressure["io"], pressure["memory"])
        }).unwrap_or_default();
        let description = format!("{reason}{load}, position {} of {}, blocker {}", admission["queue_position"], admission["queue_length"], admission["blocker"]);
        let elapsed = wait.started.elapsed();
        if wait.last.as_ref() != Some(&description) {
            eprintln!("queued: {description}, waited {}s", elapsed.as_secs());
            let _ = wait.progress.send(Some(format!("queued: {description}")));
            wait.last = Some(description);
        }
        if wait.limit.is_some_and(|limit| elapsed >= limit) {
            return Err(Error::new(ErrorCode::RunnerCapacityBusy, format!("runner queue wait expired: {reason}"), json!({
                "admission_reason": reason, "blocker": admission, "runner_job_id": job_id,
                "waited_ms": elapsed.as_millis(),
            })));
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queue_deadline_is_independent_of_command_timeout() {
        let _scope = QueueWaitScope::enter(Some(Duration::from_secs(45)));
        let admission = json!({"pending_admission_reason": "pressure"});
        assert!(observe(true, &admission, "job").is_ok());
        WAIT.with(|slot| {
            slot.borrow_mut().as_mut().unwrap().started = Instant::now() - Duration::from_secs(46)
        });
        let error = observe(true, &admission, "job").unwrap_err();
        assert_eq!(error.details["admission_reason"], "pressure");
        assert!(observe(false, &Value::Null, "job").is_ok());
    }

    #[test]
    fn no_wait_refuses_only_queued_jobs_and_restores_scope() {
        let admission =
            json!({"pending_admission_reason": "pressure", "queue_position": 3, "queue_length": 5});
        {
            let _scope = QueueWaitScope::enter(Some(Duration::ZERO));
            assert!(observe(false, &Value::Null, "job").is_ok());
            let error = observe(true, &admission, "job").unwrap_err();
            assert_eq!(error.code, ErrorCode::RunnerCapacityBusy);
            assert_eq!(error.details["blocker"], admission);
        }
        assert!(observe(true, &admission, "job").is_ok());
    }
}
