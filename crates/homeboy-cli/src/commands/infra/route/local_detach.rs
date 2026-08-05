//! Local-placement Cook detachment.
//!
//! Cook advertises a durable run id "persisted before materialization" as the
//! handle that makes a cook observable and recoverable independently of the
//! caller. On Lab placement a runner daemon owns the provider attempt, so that
//! promise holds. On local placement it did not: the cook ran in the calling
//! client's process group, so a client that hit its command timeout took the
//! cook down with it — `local_provider_ownership.state: "owner_dead"`, the run
//! cancelled, the durable record good only for reading a tombstone (#11476).
//!
//! The remedy is the one the operator was reaching for by hand with `setsid
//! nohup ... &`: the launcher re-executes the same Cook in its own session and
//! returns a bounded handoff. Nothing about the cook's own lifecycle changes —
//! it is the same controller-owned cook, executing the same argv, writing the
//! same durable records. Only its parentage does.
//!
//! This composes with, rather than contradicts, provider process-group
//! containment (#11477): containment makes a cook's children die *with* the
//! cook; detachment makes the cook survive *its launcher*. The detached cook is
//! the containment owner, so a cancelled or killed cook still reaps its
//! provider tree.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use homeboy::cli_surface::{Cli, Commands};
use homeboy::core::Error;
use serde_json::{json, Value};

const HANDOFF_SCHEMA: &str = "homeboy/agent-task-cook-local-detach-handoff/v1";
pub(crate) const DETACHED_COOK_CHILD_ENV: &str = "HOMEBOY_DETACHED_COOK_HANDOFF_CHILD";

/// Whether this invocation is a locally-placed Cook asking to be detached.
fn is_local_detached_cook(cli: &Cli) -> bool {
    cli.detach_after_handoff
        && cli.placement == homeboy::cli_surface::Placement::Local
        && matches!(
            cli.command,
            Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
                command: crate::commands::agent_task::AgentTaskCommand::Cook(_),
            })
        )
}

/// Serve `--placement local --detach-after-handoff` by re-executing this exact
/// Cook in its own session and returning a bounded handoff.
///
/// `runner_side` is true when this process is a Lab offload subprocess, a
/// managed-runner placement, or a runner-resident execution. There the request
/// is genuinely unserveable: the process is already the runner's owned
/// execution of one attempt and has no controller lifecycle to hand off, so
/// detaching would orphan work the runner believes it owns.
pub(super) fn intercept_local_detached_cook(
    cli: &Cli,
    normalized_args: &[String],
    runner_side: bool,
) -> homeboy::core::Result<Option<i32>> {
    if !is_local_detached_cook(cli) {
        return Ok(None);
    }
    if runner_side {
        return Err(runner_side_detach_error());
    }

    let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
        command: crate::commands::agent_task::AgentTaskCommand::Cook(cook),
    }) = &cli.command
    else {
        return Ok(None);
    };

    let requested_cook_id = cook.dispatch.run_id.clone();
    let cook_id = requested_cook_id
        .clone()
        .unwrap_or_else(|| format!("cook-detached-{}", uuid::Uuid::new_v4()));
    // Request validation is safe and required before acceptance. Runtime
    // pinning, recovery, provider discovery, and workspace setup are owned by
    // the detached controller after the handoff has become durable.
    crate::commands::agent_task::run::validate_cook_request(cook)?;
    let run_id = homeboy::agents::agent_tasks::lifecycle::cook_attempt_run_id(&cook_id, 1);
    let started = Instant::now();
    let mut child_args =
        detached_cook_child_args(normalized_args, &cook_id, requested_cook_id.is_some());
    let session_root = detached_session_root(&cook_id)?;
    // A detached child cannot answer a `--prompt -`: its stdin is closed and the
    // bytes live only in the launcher's pipe. Capture them here so the exact
    // prompt survives the handoff instead of the cook stalling on an empty read.
    materialize_stdin_prompt(&mut child_args, &session_root)?;
    let log_path = session_root.join("cook.log");
    let handoff_path = session_root.join("handoff.json");
    let mut handoff = DetachedCookHandoff {
        state: DetachedHandoffState::Accepted,
        run_id,
        waited_ms: 0,
        phase_timings_ms: json!({}),
    };
    // The acceptance record precedes child startup so interruption cannot lose
    // the caller's requested identity or the recovery handoff.
    persist_handoff(
        &handoff_path,
        &handoff_envelope(&cook_id, None, &log_path, &handoff),
    )?;
    let child = spawn_detached_cook(&child_args, &log_path)?;
    handoff.waited_ms = started.elapsed().as_millis() as u64;
    handoff.phase_timings_ms = json!({
        "essential_validation_and_durable_handoff": handoff.waited_ms,
        "global_recovery": "deferred_to_detached_owner",
        "controller_runtime": "deferred_to_detached_owner",
        "provider_startup": "deferred_to_detached_owner",
    });
    let envelope = handoff_envelope(&cook_id, Some(child.id()), &log_path, &handoff);
    persist_handoff(&handoff_path, &envelope)?;
    crate::commands::agent_task::run::announce_durable_cook_identity(
        Some(&cook_id),
        &handoff.run_id,
    );
    let stdout = serde_json::to_string_pretty(&envelope).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize detached local cook handoff".to_string()),
        )
    })?;
    println!("{stdout}");
    Ok(Some(0))
}

/// The one context where local detachment stays a rejection.
fn runner_side_detach_error() -> Error {
    Error::validation_invalid_argument(
        "detach-after-handoff",
        "agent-task cook cannot detach after handoff with --placement local inside a runner-owned execution because the runner already owns this attempt",
        None,
        Some(vec![
            "Detach from the controller that dispatches the attempt, not from the runner process executing it.".to_string(),
        ]),
    )
}

/// The argv the detached cook executes.
///
/// It is the caller's own argv with two edits: the detach request is consumed
/// by the launcher, and the cook id is pinned so the launcher can name the run
/// it just handed off. Everything else is preserved byte for byte — a detached
/// cook that silently differs from the requested one is worse than no detach.
fn detached_cook_child_args(
    normalized_args: &[String],
    cook_id: &str,
    has_explicit_cook_id: bool,
) -> Vec<String> {
    let mut args: Vec<String> = normalized_args
        .iter()
        .skip(1)
        .filter(|arg| {
            arg.as_str() != "--detach-after-handoff" && !arg.starts_with("--detach-after-handoff=")
        })
        .cloned()
        .collect();
    if !has_explicit_cook_id {
        args.push("--run-id".to_string());
        args.push(cook_id.to_string());
    }
    args
}

/// Replace a `--prompt -` stdin request with a file the detached cook can read.
///
/// Returns the materialized path when a rewrite happened.
fn materialize_stdin_prompt(
    args: &mut [String],
    session_root: &Path,
) -> homeboy::core::Result<Option<PathBuf>> {
    materialize_prompt_from(args, session_root, &mut std::io::stdin().lock())
}

/// The reader is a parameter so the capture can be exercised without a test
/// reaching for the harness's own stdin, which may never reach EOF.
fn materialize_prompt_from(
    args: &mut [String],
    session_root: &Path,
    source: &mut impl Read,
) -> homeboy::core::Result<Option<PathBuf>> {
    let Some(index) = stdin_prompt_index(args) else {
        return Ok(None);
    };
    let mut prompt = Vec::new();
    source.read_to_end(&mut prompt).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("read stdin prompt for a detached local cook".to_string()),
        )
    })?;
    let path = session_root.join("prompt.txt");
    std::fs::write(&path, &prompt)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    args[index] = if args[index] == "-" {
        format!("@{}", path.display())
    } else {
        format!("--prompt=@{}", path.display())
    };
    Ok(Some(path))
}

/// Index of the argv element holding a stdin prompt request, if any.
///
/// Both spellings clap accepts are covered: a separated `--prompt -` names the
/// following element, an attached `--prompt=-` names itself.
fn stdin_prompt_index(args: &[String]) -> Option<usize> {
    args.iter().enumerate().find_map(|(index, arg)| {
        if arg == "--prompt" && args.get(index + 1).is_some_and(|value| value == "-") {
            Some(index + 1)
        } else if arg == "--prompt=-" {
            Some(index)
        } else {
            None
        }
    })
}

/// Per-cook scratch directory for the launcher's captured stdio and prompt.
fn detached_session_root(cook_id: &str) -> homeboy::core::Result<PathBuf> {
    let root = homeboy::core::paths::homeboy_data()?
        .join("agent-task-detached")
        .join(homeboy::core::paths::sanitize_path_segment(cook_id));
    std::fs::create_dir_all(&root)
        .map_err(|error| Error::internal_io(error.to_string(), Some(root.display().to_string())))?;
    Ok(root)
}

fn spawn_detached_cook(
    args: &[String],
    log_path: &Path,
) -> homeboy::core::Result<std::process::Child> {
    let exe = std::env::current_exe().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve current executable for a detached local cook".to_string()),
        )
    })?;
    let log = std::fs::File::create(log_path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(log_path.display().to_string()))
    })?;
    let log_err = log.try_clone().map_err(|error| {
        Error::internal_io(error.to_string(), Some(log_path.display().to_string()))
    })?;

    let mut command = Command::new(exe);
    command
        .args(args)
        .env(DETACHED_COOK_CHILD_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    homeboy::core::process::detach_from_caller_session(&mut command);
    command.spawn().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("spawn detached local cook".to_string()),
        )
    })
}

/// What the launcher could prove about the cook before returning.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DetachedCookHandoff {
    state: DetachedHandoffState,
    run_id: String,
    waited_ms: u64,
    phase_timings_ms: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetachedHandoffState {
    /// The launcher persisted the requested Cook identity and handoff before
    /// starting the detached controller.
    Accepted,
}

impl DetachedHandoffState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
        }
    }
}

fn persist_handoff(path: &Path, handoff: &Value) -> homeboy::core::Result<()> {
    let bytes = serde_json::to_vec_pretty(handoff).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize detached Cook handoff".to_string()),
        )
    })?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes).map_err(|error| {
        Error::internal_io(error.to_string(), Some(temporary.display().to_string()))
    })?;
    std::fs::rename(&temporary, path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))
}

/// The launcher's only output: a bounded, machine-readable handoff naming the
/// durable handle and the evidence needed to follow or stop the cook.
fn handoff_envelope(
    cook_id: &str,
    pid: Option<u32>,
    log_path: &Path,
    handoff: &DetachedCookHandoff,
) -> Value {
    json!({
        "schema": HANDOFF_SCHEMA,
        "placement": "local",
        "detached": true,
        "cook_id": cook_id,
        "run_id": handoff.run_id,
        "pid": pid,
        "launcher_log": log_path.display().to_string(),
        "handoff": {
            "state": handoff.state.as_str(),
            "waited_ms": handoff.waited_ms,
            "phase_timings_ms": handoff.phase_timings_ms,
        },
        "status_command": format!("homeboy agent-task status {cook_id}"),
        "logs_command": format!("homeboy agent-task logs {cook_id}"),
        "cancel_command": format!("homeboy agent-task cancel {cook_id}"),
        // The detached cook, not this launcher, owns any `--output-file`: it
        // writes the final Cook report there when the run completes.
        "output_file_owner": "detached_cook",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn cook_cli(extra: &[&str]) -> (Cli, Vec<String>) {
        let mut argv = vec!["homeboy"];
        argv.extend_from_slice(extra);
        argv.extend_from_slice(&[
            "agent-task",
            "cook",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "repo@branch",
            "--verify",
            "true",
        ]);
        let cli = Cli::try_parse_from(argv.clone()).expect("parse cook invocation");
        (cli, args(&argv))
    }

    /// The rejection this replaces was unconditional. Preserving it inside a
    /// runner-owned execution is the whole reason `runner_side` exists.
    #[test]
    fn a_runner_owned_execution_still_refuses_to_detach() {
        let (cli, normalized) = cook_cli(&["--placement", "local", "--detach-after-handoff"]);

        let error = intercept_local_detached_cook(&cli, &normalized, true)
            .expect_err("a runner-owned execution cannot detach");

        assert!(
            error
                .message
                .contains("cannot detach after handoff with --placement local"),
            "{}",
            error.message
        );
        assert!(
            error.message.contains("runner-owned execution"),
            "{}",
            error.message
        );
    }

    /// Nothing but a locally-placed, detach-requesting Cook may be intercepted:
    /// every other invocation has to fall through to normal routing untouched.
    /// These cases must not spawn anything.
    #[test]
    fn only_a_locally_placed_detaching_cook_is_intercepted() {
        for extra in [
            vec!["--placement", "local"],
            vec!["--placement", "local", "--wait"],
            vec!["--detach-after-handoff"],
            vec!["--placement", "lab-or-local", "--detach-after-handoff"],
            vec![],
        ] {
            let (cli, normalized) = cook_cli(&extra);

            assert!(!is_local_detached_cook(&cli), "{extra:?}");
            assert_eq!(
                intercept_local_detached_cook(&cli, &normalized, false).expect("no interception"),
                None,
                "{extra:?}"
            );
        }
    }

    #[test]
    fn a_non_cook_command_is_never_intercepted() {
        let cli = Cli::try_parse_from(["homeboy", "--placement", "local", "status"])
            .expect("parse status invocation");

        assert!(!is_local_detached_cook(&cli));
    }

    #[test]
    fn child_args_drop_the_detach_request_and_pin_a_generated_cook_id() {
        let normalized = args(&[
            "homeboy",
            "--placement",
            "local",
            "--detach-after-handoff",
            "agent-task",
            "cook",
            "--prompt",
            "fix it",
        ]);

        let child = detached_cook_child_args(&normalized, "cook-generated", false);

        assert_eq!(
            child,
            args(&[
                "--placement",
                "local",
                "agent-task",
                "cook",
                "--prompt",
                "fix it",
                "--run-id",
                "cook-generated",
            ])
        );
    }

    #[test]
    fn child_args_preserve_an_explicit_cook_id_without_duplicating_it() {
        let normalized = args(&[
            "homeboy",
            "--detach-after-handoff",
            "--placement",
            "local",
            "agent-task",
            "cook",
            "--run-id",
            "cook-explicit",
            "--prompt",
            "fix it",
        ]);

        let child = detached_cook_child_args(&normalized, "cook-explicit", true);

        assert_eq!(
            child.iter().filter(|arg| *arg == "--run-id").count(),
            1,
            "{child:?}"
        );
        assert!(
            !child.iter().any(|arg| arg == "--detach-after-handoff"),
            "{child:?}"
        );
        assert_eq!(child.last().map(String::as_str), Some("fix it"));
    }

    /// The re-executed cook must be the requested cook. Anything the launcher
    /// silently drops is work the operator asked for and did not get.
    #[test]
    fn child_args_preserve_every_other_flag_verbatim() {
        let normalized = args(&[
            "homeboy",
            "--placement",
            "local",
            "--detach-after-handoff",
            "--allow-dirty-lab-workspace",
            "agent-task",
            "cook",
            "--to-worktree",
            "repo@branch",
            "--verify",
            "true",
            "--max-attempts",
            "5",
            "--no-finalize",
        ]);

        let child = detached_cook_child_args(&normalized, "cook-generated", false);

        for expected in [
            "--allow-dirty-lab-workspace",
            "--to-worktree",
            "repo@branch",
            "--verify",
            "--max-attempts",
            "5",
            "--no-finalize",
        ] {
            assert!(child.iter().any(|arg| arg == expected), "{child:?}");
        }
    }

    /// A detached child's stdin is closed, and the piped bytes live only in the
    /// launcher's pipe. Losing them would strand the cook on an empty prompt.
    #[test]
    fn stdin_prompt_is_captured_verbatim_for_the_detached_cook() {
        let session = tempfile::tempdir().expect("session root");
        let mut child = args(&["agent-task", "cook", "--prompt", "-"]);
        let piped = "fix `$THING`\n\nwith a trailing newline\n";

        let path = materialize_prompt_from(
            &mut child,
            session.path(),
            &mut std::io::Cursor::new(piped.as_bytes()),
        )
        .expect("materialize stdin prompt")
        .expect("a stdin prompt was present");

        assert_eq!(std::fs::read_to_string(&path).expect("prompt file"), piped);
        assert_eq!(child.last(), Some(&format!("@{}", path.display())));
    }

    #[test]
    fn a_prompt_that_is_not_stdin_is_left_alone() {
        let session = tempfile::tempdir().expect("session root");
        let mut child = args(&["agent-task", "cook", "--prompt", "@task.md"]);

        let rewritten = materialize_prompt_from(
            &mut child,
            session.path(),
            &mut std::io::Cursor::new(b"unused".as_slice()),
        )
        .expect("inspect prompt");

        assert_eq!(rewritten, None);
        assert_eq!(child.last().map(String::as_str), Some("@task.md"));
    }

    /// A literal `-` that is the value of some other option is not a prompt.
    #[test]
    fn only_the_prompt_option_claims_a_stdin_marker() {
        assert_eq!(
            stdin_prompt_index(&args(&["--base", "-", "--prompt", "@task.md"])),
            None
        );
        assert_eq!(
            stdin_prompt_index(&args(&["--prompt", "-", "--no-finalize"])),
            Some(1)
        );
        assert_eq!(
            stdin_prompt_index(&args(&["--prompt=-", "--no-finalize"])),
            Some(0)
        );
    }

    #[test]
    fn an_attached_stdin_prompt_keeps_its_option_when_rewritten() {
        let session = tempfile::tempdir().expect("session root");
        let mut child = args(&["agent-task", "cook", "--prompt=-", "--no-finalize"]);

        let path = materialize_prompt_from(
            &mut child,
            session.path(),
            &mut std::io::Cursor::new(b"piped".as_slice()),
        )
        .expect("materialize stdin prompt")
        .expect("a stdin prompt was present");

        assert_eq!(child[2], format!("--prompt=@{}", path.display()));
    }

    #[test]
    fn an_accepted_handoff_reports_the_addressable_ids_and_follow_up_commands() {
        let handoff = DetachedCookHandoff {
            state: DetachedHandoffState::Accepted,
            run_id: "cook-11476-attempt-1-ab12cd34".to_string(),
            waited_ms: 240,
            phase_timings_ms: json!({ "essential_validation_and_durable_handoff": 240 }),
        };

        let envelope = handoff_envelope(
            "cook-11476",
            Some(4242),
            Path::new("/data/agent-task-detached/cook-11476/cook.log"),
            &handoff,
        );

        assert_eq!(envelope["schema"], HANDOFF_SCHEMA);
        assert_eq!(envelope["placement"], "local");
        assert_eq!(envelope["detached"], true);
        assert_eq!(envelope["cook_id"], "cook-11476");
        assert_eq!(envelope["run_id"], "cook-11476-attempt-1-ab12cd34");
        assert_eq!(envelope["pid"], 4242);
        assert_eq!(envelope["handoff"]["state"], "accepted");
        assert_eq!(
            envelope["handoff"]["phase_timings_ms"]["essential_validation_and_durable_handoff"],
            240
        );
        assert_eq!(
            envelope["status_command"],
            "homeboy agent-task status cook-11476"
        );
        assert_eq!(
            envelope["cancel_command"],
            "homeboy agent-task cancel cook-11476"
        );
    }

    #[test]
    fn accepted_handoff_is_durable_before_the_child_starts() {
        let directory = tempfile::tempdir().expect("handoff directory");
        let path = directory.path().join("handoff.json");
        let handoff = json!({ "state": "accepted", "cook_id": "cook-11476" });

        persist_handoff(&path, &handoff).expect("persist handoff");

        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(path).expect("read handoff"))
                .expect("handoff json"),
            handoff
        );
    }
}
