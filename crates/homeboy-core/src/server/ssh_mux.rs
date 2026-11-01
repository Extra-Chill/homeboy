//! Homeboy-owned SSH connection multiplexing for one unit of work (#16806).
//!
//! A deploy issues many short ssh, scp, and rsync calls to the same server.
//! Inside a [`scope`], every non-interactive connection to one
//! `user@host:port` (and identity) shares a single ControlMaster: the first
//! call opens it with `ControlMaster=auto`, later calls attach to its socket,
//! and the outermost scope closes it with `ssh -O exit` when it ends — on
//! success, on an error return, and on a panic unwind.
//!
//! Sockets live in a private (0700) per-scope directory under `/tmp` with a
//! short hashed file name, so the path stays well under the Unix socket path
//! limit even after ssh appends its 17-byte temporary suffix. `ControlPersist`
//! is bounded so a master orphaned by a killed process still exits on its own.
//! When a master cannot be created, ssh falls back to a plain connection.

use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};

/// Idle lifetime of a master after its last client detaches. The scope closes
/// it explicitly; this only bounds a master orphaned by a killed process.
pub(crate) const CONTROL_PERSIST: &str = "60";

struct MuxState {
    depth: usize,
    dir: tempfile::TempDir,
    /// Control socket path -> `user@host` destination used to close it.
    masters: BTreeMap<PathBuf, String>,
}

static STATE: Mutex<Option<MuxState>> = Mutex::new(None);

fn state() -> MutexGuard<'static, Option<MuxState>> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Guard for a multiplexed unit of work. Scopes nest; the connections close
/// when the outermost guard drops.
#[must_use = "connections are only multiplexed while the scope guard is alive"]
pub struct MuxScope {
    active: bool,
}

/// Multiplex SSH connections opened by this process until the guard drops.
pub fn scope() -> MuxScope {
    let mut state = state();
    if let Some(state) = state.as_mut() {
        state.depth += 1;
        return MuxScope { active: true };
    }
    match socket_dir() {
        Some(dir) => {
            *state = Some(MuxState {
                depth: 1,
                dir,
                masters: BTreeMap::new(),
            });
            MuxScope { active: true }
        }
        None => MuxScope { active: false },
    }
}

impl MuxScope {
    /// Directory holding this scope's control sockets, if multiplexing is on.
    pub fn socket_dir(&self) -> Option<PathBuf> {
        state()
            .as_ref()
            .filter(|_| self.active)
            .map(|state| state.dir.path().to_path_buf())
    }
}

impl Drop for MuxScope {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let finished = {
            let mut state = state();
            match state.as_mut() {
                Some(active) if active.depth > 1 => {
                    active.depth -= 1;
                    None
                }
                _ => state.take(),
            }
        };
        if let Some(finished) = finished {
            close(finished);
        }
    }
}

fn close(state: MuxState) {
    for (control_path, destination) in &state.masters {
        if !control_path.exists() {
            continue;
        }
        let _ = Command::new("ssh")
            .arg("-o")
            .arg(format!("ControlPath={}", control_path.display()))
            .args(["-o", "BatchMode=yes", "-O", "exit"])
            .arg(destination)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    // Dropping the TempDir removes the directory and any stale socket.
}

#[cfg(unix)]
fn socket_dir() -> Option<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("hb-ssh-")
        .tempdir_in("/tmp")
        .ok()
}

#[cfg(not(unix))]
fn socket_dir() -> Option<tempfile::TempDir> {
    // Win32-OpenSSH has no ControlMaster support.
    None
}

/// The control socket for `user@host:port` within the active scope, if any.
pub(crate) fn control_path(
    user: &str,
    host: &str,
    port: u16,
    identity_file: Option<&str>,
) -> Option<PathBuf> {
    let mut state = state();
    let state = state.as_mut()?;
    let mut hasher = DefaultHasher::new();
    (user, host, port, identity_file).hash(&mut hasher);
    let path = state.dir.path().join(format!("{:016x}", hasher.finish()));
    state
        .masters
        .entry(path.clone())
        .or_insert_with(|| format!("{user}@{host}"));
    Some(path)
}
