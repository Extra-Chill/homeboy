//! Retain wait ownership when a durable supervisor stops watching a child.
//! Never use waitpid(-1): other execution owners still need their exit statuses.
use std::ops::{Deref, DerefMut};
use std::process::Child;

pub struct ReapingChild(Option<Child>);

impl From<Child> for ReapingChild {
    fn from(child: Child) -> Self {
        Self(Some(child))
    }
}
impl Deref for ReapingChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        self.0.as_ref().expect("owned child")
    }
}
impl DerefMut for ReapingChild {
    fn deref_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("owned child")
    }
}
impl Drop for ReapingChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            // Detached durable work must survive supervisor errors; waiting is
            // asynchronous and does not signal or tear down its process tree.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn supervisor_reaps_exited_child() {
        let child = ReapingChild::from(
            std::process::Command::new("/bin/sh")
                .args(["-c", "exit 0"])
                .spawn()
                .unwrap(),
        );
        let proc = std::path::PathBuf::from(format!("/proc/{}", child.id()));
        drop(child);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while proc.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!proc.exists(), "supervisor left its exited child unreaped");
    }
}
