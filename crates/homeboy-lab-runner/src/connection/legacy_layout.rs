//! One-time move off the pre-#16752 `daemon-generations` layout.
//!
//! Older Homeboy started runner daemons under
//! `~/.config/homeboy/daemon-generations/<runner>/controllers/<controller>/<dir>`
//! (`primary`, or a rotated 48-hex generation). Every lifecycle command now
//! targets [`super::daemon_state_dir`], so before a daemon is started there,
//! this step stops each idle legacy daemon of this (runner, controller) and
//! refuses to continue next to a busy one: its jobs are scoped to the legacy
//! directory, and a second daemon beside it would strand them.
//!
//! This step exists only to move runners off that layout (epic
//! Extra-Chill/homeboy#16680). Delete this module and its call sites once no
//! runner has a `daemon-generations` directory.

use super::*;

const LEGACY_DIR_MARKER: &str = "homeboy-legacy-daemon-dir";
const LEGACY_STEP_TIMEOUT: Duration = Duration::from_secs(120);

/// A legacy daemon that still has active jobs. Connect fails rather than
/// starting a second daemon beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LegacyDaemonBusy {
    pub state_dir: String,
    pub active_jobs: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LegacyLayoutError {
    Busy(LegacyDaemonBusy),
    Other(String),
}

impl LegacyLayoutError {
    pub(super) fn message(&self, runner_id: &str) -> String {
        match self {
            Self::Busy(busy) => format!(
                "runner `{runner_id}` has a daemon in legacy state dir `{}` with {} active job(s). Those jobs cannot move to the new runner-daemons state dir; wait for them to finish (or cancel them), then run `homeboy runner connect {}` again.",
                busy.state_dir,
                busy.active_jobs,
                shell::quote_arg(runner_id),
            ),
            Self::Other(message) => format!(
                "moving runner `{runner_id}` off the legacy daemon-generations layout failed: {message}"
            ),
        }
    }

    pub(super) fn into_error(self, runner_id: &str) -> Error {
        let message = self.message(runner_id);
        let mut error = Error::validation_invalid_argument(
            "runner",
            message,
            Some(runner_id.to_string()),
            None,
        );
        if let Self::Busy(busy) = &self {
            error.details = serde_json::json!({
                "classification": "legacy_daemon_busy",
                "legacy_state_dir": busy.state_dir,
                "active_jobs": busy.active_jobs,
            });
        }
        error
    }
}

/// Shell that runs `homeboy daemon <args>` against every legacy state dir of
/// this (runner, controller), each output preceded by a marker line.
fn legacy_daemon_script(runner_id: &str, controller_id: &str, homeboy: &str, args: &str) -> String {
    format!(
        r#"for dir in "$HOME/.config/homeboy/daemon-generations/{runner}/controllers/{controller}"/*/; do
  [ -d "$dir" ] || continue
  dir="${{dir%/}}"
  printf '\n{LEGACY_DIR_MARKER}\t%s\n' "$dir"
  HOMEBOY_DAEMON_STATE_DIR="$dir" {homeboy} daemon {args} 2>/dev/null </dev/null || true
done"#,
        runner = paths::sanitize_path_segment(runner_id),
        controller = super::controller_scope_segment(controller_id),
        homeboy = shell::quote_arg(homeboy),
    )
}

fn legacy_outputs(stdout: &str) -> Vec<(String, String)> {
    let mut blocks: Vec<(String, String)> = Vec::new();
    for line in stdout.lines() {
        if let Some(dir) = line.strip_prefix(&format!("{LEGACY_DIR_MARKER}\t")) {
            blocks.push((dir.to_string(), String::new()));
        } else if let Some((_, output)) = blocks.last_mut() {
            output.push_str(line);
            output.push('\n');
        }
    }
    blocks
}

fn legacy_data(
    dir: &str,
    verb: &str,
    output: &str,
) -> std::result::Result<Value, LegacyLayoutError> {
    let envelope = parse_envelope(output).map_err(|error| {
        LegacyLayoutError::Other(format!(
            "`daemon {verb}` in legacy state dir `{dir}` returned no readable result ({error}); inspect or remove that dir"
        ))
    })?;
    if !envelope.success {
        return Err(LegacyLayoutError::Other(format!(
            "`daemon {verb}` in legacy state dir `{dir}` failed: {}; inspect or remove that dir",
            envelope.error.unwrap_or(Value::Null)
        )));
    }
    Ok(envelope.data.unwrap_or(Value::Null))
}

/// Stop every idle legacy daemon of this (runner, controller) with the normal
/// `daemon stop`, after proving none has active jobs. Returns the lease ids of
/// the legacy daemons found, so the caller can drop a session pointing at one.
/// The legacy dirs stay on disk for operator cleanup.
pub(super) fn stop_legacy_daemons(
    mut run: impl FnMut(&str) -> std::result::Result<String, String>,
    runner_id: &str,
    controller_id: &str,
    homeboy: &str,
) -> std::result::Result<Vec<String>, LegacyLayoutError> {
    let statuses = legacy_outputs(
        &run(&legacy_daemon_script(
            runner_id,
            controller_id,
            homeboy,
            "status",
        ))
        .map_err(LegacyLayoutError::Other)?,
    );
    if statuses.is_empty() {
        return Ok(Vec::new());
    }
    let mut leases = Vec::new();
    for (dir, output) in &statuses {
        let data = legacy_data(dir, "status", output)?;
        let active_jobs = remote_daemon_active_jobs(&data);
        if active_jobs > 0 {
            return Err(LegacyLayoutError::Busy(LegacyDaemonBusy {
                state_dir: dir.clone(),
                active_jobs,
            }));
        }
        leases.extend(
            remote_daemon_status_from_data(&data)
                .daemon
                .and_then(|daemon| daemon.lease_id)
                .filter(|lease| !lease.is_empty()),
        );
    }
    let stops = run(&legacy_daemon_script(
        runner_id,
        controller_id,
        homeboy,
        "stop",
    ))
    .map_err(LegacyLayoutError::Other)?;
    for (dir, output) in legacy_outputs(&stops) {
        let data = legacy_data(&dir, "stop", &output)?;
        // A job admitted between the status read and the stop.
        let active_jobs = data
            .pointer("/termination_evidence/active_jobs")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        if active_jobs > 0 {
            return Err(LegacyLayoutError::Busy(LegacyDaemonBusy {
                state_dir: dir,
                active_jobs,
            }));
        }
    }
    Ok(leases)
}

/// [`stop_legacy_daemons`] over SSH for this controller.
pub(super) fn stop_remote_legacy_daemons(
    client: &SshClient,
    homeboy: &str,
    runner_id: &str,
) -> std::result::Result<Vec<String>, LegacyLayoutError> {
    stop_legacy_daemons(
        |script| {
            let output = client.execute_with_timeout(script, LEGACY_STEP_TIMEOUT);
            if output.success {
                Ok(output.stdout)
            } else {
                Err(command_failure_message(
                    "legacy daemon-generations step",
                    &output,
                ))
            }
        },
        runner_id,
        &controller_id(),
        homeboy,
    )
}

/// The legacy step for a direct runner, before its daemon starts in the new
/// state dir. A failure is the connect report.
pub(super) fn move_direct_runner(
    client: &SshClient,
    homeboy: &str,
    runner_id: &str,
) -> std::result::Result<(), Box<(RunnerConnectReport, i32)>> {
    let failure = |kind, message| {
        Box::new(failed_connect(
            runner_id,
            session_path(runner_id).unwrap_or_default(),
            kind,
            message,
        ))
    };
    match stop_remote_legacy_daemons(client, homeboy, runner_id) {
        Ok(stopped) => forget_stopped_legacy_session(runner_id, &stopped)
            .map_err(|error| failure(RunnerFailureKind::DaemonStartupFailure, error.message)),
        Err(error) => {
            let kind = match error {
                LegacyLayoutError::Busy(_) => RunnerFailureKind::LegacyDaemonBusy,
                LegacyLayoutError::Other(_) => RunnerFailureKind::DaemonStartupFailure,
            };
            Err(failure(kind, error.message(runner_id)))
        }
    }
}

/// Drop the recorded session when its lease belonged to a stopped legacy
/// daemon, so connect proceeds to the daemon in the new state dir.
pub(super) fn forget_stopped_legacy_session(
    runner_id: &str,
    stopped_leases: &[String],
) -> Result<()> {
    if stopped_leases.is_empty() {
        return Ok(());
    }
    let session = match read_session(runner_id) {
        Ok(Some(session)) => session,
        Ok(None) => return Ok(()),
        Err(error) if error.code == homeboy_core::error::ErrorCode::ConfigInvalidJson => {
            return Ok(())
        }
        Err(error) => return Err(error),
    };
    if session
        .remote_daemon_lease_id
        .as_ref()
        .is_some_and(|lease| stopped_leases.contains(lease))
    {
        terminate_tunnel_if_owned(&session);
        if remove_session_if_matches(runner_id, &session)? {
            let _ = remove_ownership_if_matches(runner_id, &session)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_root(home: &std::path::Path) -> std::path::PathBuf {
        home.join(format!(
            ".config/homeboy/daemon-generations/homeboy-lab/controllers/{}",
            super::super::controller_scope_segment("controller-a")
        ))
    }

    /// A fake homeboy: `busy` dirs report one active job, every other dir an
    /// idle lease named after the dir. Each stop is logged.
    fn fake_homeboy(home: &std::path::Path) -> std::path::PathBuf {
        let fake = home.join("homeboy");
        homeboy_test_support::write_executable(
            &fake,
            r#"#!/bin/sh
name=$(basename "$HOMEBOY_DAEMON_STATE_DIR")
case "$2" in
  status)
    jobs=0
    [ "$name" = busy ] && jobs=1
    printf '{"success":true,"data":{"action":"status","fresh":true,"daemon":{"address":"127.0.0.1:1","pid":7,"lease_id":"lease-%s"},"freshness":{"active_jobs":%s}}}\n' "$name" "$jobs"
    ;;
  stop)
    printf '%s\n' "$name" >> "$HOME/stopped.log"
    printf '{"success":true,"data":{"action":"stop","stopped":true,"state_path":"x"}}\n'
    ;;
esac
"#,
            0o755,
        )
        .unwrap();
        fake
    }

    fn run_local(
        home: &std::path::Path,
    ) -> impl FnMut(&str) -> std::result::Result<String, String> + '_ {
        move |script| {
            let output = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(script)
                .env("HOME", home)
                .output()
                .map_err(|error| error.to_string())?;
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        }
    }

    #[test]
    fn idle_legacy_daemons_are_stopped_including_primary_and_dirs_stay() {
        let home = tempfile::tempdir().unwrap();
        let root = legacy_root(home.path());
        let generation = "0123456789abcdef0123456789abcdef0123456789abcdef";
        for name in ["primary", generation] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        let fake = fake_homeboy(home.path());
        let leases = stop_legacy_daemons(
            run_local(home.path()),
            "homeboy-lab",
            "controller-a",
            &fake.display().to_string(),
        )
        .expect("idle legacy daemons stop");
        let mut leases = leases;
        leases.sort();
        let mut expected = vec![format!("lease-{generation}"), "lease-primary".to_string()];
        expected.sort();
        assert_eq!(leases, expected);
        let stopped = std::fs::read_to_string(home.path().join("stopped.log")).unwrap();
        assert!(stopped.lines().any(|line| line == "primary"));
        assert!(stopped.lines().any(|line| line == generation));
        assert!(root.join("primary").is_dir(), "legacy dirs stay on disk");
    }

    #[test]
    fn a_busy_legacy_daemon_refuses_connect_and_stops_nothing() {
        let home = tempfile::tempdir().unwrap();
        let root = legacy_root(home.path());
        for name in ["primary", "busy"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        let fake = fake_homeboy(home.path());
        let error = stop_legacy_daemons(
            run_local(home.path()),
            "homeboy-lab",
            "controller-a",
            &fake.display().to_string(),
        )
        .expect_err("a busy legacy daemon refuses");
        let busy_dir = root.join("busy").display().to_string();
        assert_eq!(
            error,
            LegacyLayoutError::Busy(LegacyDaemonBusy {
                state_dir: busy_dir.clone(),
                active_jobs: 1,
            })
        );
        let message = error.message("homeboy-lab");
        assert!(message.contains(&busy_dir));
        assert!(message.contains("1 active job(s)"));
        assert!(!home.path().join("stopped.log").exists());
    }

    #[test]
    fn no_legacy_layout_is_a_no_op() {
        let home = tempfile::tempdir().unwrap();
        let fake = fake_homeboy(home.path());
        let leases = stop_legacy_daemons(
            run_local(home.path()),
            "homeboy-lab",
            "controller-a",
            &fake.display().to_string(),
        )
        .unwrap();
        assert!(leases.is_empty());
    }
}
