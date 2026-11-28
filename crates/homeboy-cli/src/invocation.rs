//! Best-effort runtime-boundary telemetry. No arguments or diagnostic text leave this boundary.
use clap::ArgMatches;
use homeboy::core::observation::store::Invocation;
use std::cell::RefCell;
use std::time::Instant;

thread_local! {
    static CURRENT: RefCell<Option<Invocation>> = const { RefCell::new(None) };
    static START: RefCell<Option<(String, Instant)>> = const { RefCell::new(None) };
}

pub(crate) fn identify(matches: &ArgMatches) {
    CURRENT.with_borrow_mut(|current| {
        let Some(row) = current else { return };
        let mut cursor = matches;
        let mut path = Vec::new();
        loop {
            for key in ["run_id", "cook_id", "run", "cook", "id"] {
                if let Ok(Some(id)) = cursor.try_get_one::<String>(key) {
                    row.run_id = opaque(id);
                }
            }
            let Some((name, child)) = cursor.subcommand() else {
                break;
            };
            path.push(name);
            cursor = child;
        }
        row.command_path = path.join(" ").chars().take(256).collect();
        row.read_only = declared_read_only(&path);
    });
}

/// A path the command registry explicitly declares read-only. Silence is not
/// a declaration, so an undeclared path still counts as a possible mutation.
fn declared_read_only(path: &[&str]) -> bool {
    let Some((command, subcommand)) = path.split_first() else {
        return false;
    };
    crate::command_contract::registered_command(command)
        .and_then(|spec| spec.path_safety(subcommand))
        .is_some_and(|entry| !entry.safety.mutates)
}

/// Record that the parsed command only reads Cook or workflow state.
pub(crate) fn mark_read_only(read_only: bool) {
    CURRENT.with_borrow_mut(|current| {
        if let Some(row) = current {
            row.read_only |= read_only;
        }
    });
}

fn opaque(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c)))
    .then(|| value.to_string())
}

pub(crate) fn error(error: &homeboy::core::Error) {
    CURRENT.with_borrow_mut(|current| {
        if let Some(row) = current {
            row.error_code = Some(error.code.as_str().to_string());
            row.error_field = error
                .details
                .get("field")
                .and_then(|v| v.as_str())
                .and_then(opaque);
        }
    });
}

/// Record the command result's own `status` when it is an opaque token.
pub(crate) fn outcome(result: &serde_json::Value) {
    CURRENT.with_borrow_mut(|current| {
        if let Some(row) = current {
            row.outcome = result
                .get("status")
                .and_then(|v| v.as_str())
                .and_then(opaque);
        }
    });
}

pub(crate) fn finish(exit: std::process::ExitCode) {
    let row = CURRENT.with_borrow_mut(Option::take);
    let start = START.with_borrow_mut(Option::take);
    if let (Some(mut row), Some((started, timer))) = (row, start) {
        row.duration_ms = timer.elapsed().as_millis().min(u64::MAX as u128) as u64;
        row.exit_code = (0..=255)
            .find(|code| exit == std::process::ExitCode::from(*code as u8))
            .unwrap_or(1);
        // Telemetry appends to an existing run store and never creates Homeboy
        // state: a preview or rejected command on a fresh home stays read-only.
        let Ok(path) = homeboy::core::observation::store::database_path() else {
            return;
        };
        if !path.exists() {
            return;
        }
        if let Ok(store) =
            homeboy::core::observation::ObservationStore::open_initialized_for_lifecycle_at(path)
        {
            let _ = store.record_invocation(
                &row,
                &homeboy_upgrade::upgrade::current_build_version(),
                &started,
            );
        }
    }
}

pub(crate) fn parse_exit(success: bool) {
    CURRENT.with_borrow_mut(|row| {
        if let Some(row) = row {
            row.command_path = "unparsed".into();
            if !success {
                row.error_code = Some("validation.invalid_argument".into());
            }
        }
    });
    finish(std::process::ExitCode::from(if success { 0 } else { 2 }));
}

/// Who is asking, for durable records of operator actions such as a cancel:
/// the process ancestry, the Cook attempt it runs inside (the first ancestor
/// whose working directory is an attempt's controller scratch), the SSH
/// client when there is one, and the caller session when one is declared.
pub(crate) fn requester() -> String {
    let ancestry = ancestry();
    let mut requester = format!(
        "pid {}, ancestry {}",
        std::process::id(),
        ancestry
            .iter()
            .map(|(pid, name, _)| format!("{name}[{pid}]"))
            .collect::<Vec<_>>()
            .join(" < ")
    );
    if let Some(attempt) = enclosing_cook_attempt(&ancestry) {
        requester.push_str(&format!(", inside Cook attempt `{attempt}`"));
    }
    if let Some(client) = std::env::var("SSH_CONNECTION")
        .ok()
        .and_then(|value| value.split_whitespace().next().map(str::to_string))
    {
        requester.push_str(&format!(", ssh from {client}"));
    }
    if let Some(session) =
        CURRENT.with_borrow(|row| row.as_ref().and_then(|row| row.caller_session.clone()))
    {
        requester.push_str(&format!(", caller session `{session}`"));
    }
    requester
}

/// The Cook attempt this process runs inside: the first ancestor whose working
/// directory is an attempt's controller scratch.
fn enclosing_cook_attempt(
    ancestry: &[(u32, String, Option<std::path::PathBuf>)],
) -> Option<String> {
    let root = homeboy::core::paths::controller_scratch_store()
        .ok()?
        .join("attempts");
    ancestry.iter().find_map(|(_, _, cwd)| {
        cwd.as_deref()?
            .strip_prefix(&root)
            .ok()?
            .components()
            .next()
            .map(|attempt| attempt.as_os_str().to_string_lossy().into_owned())
    })
}

/// Refuse a control-plane mutation issued from inside a Cook attempt: an
/// attempt's agent owns its task, not the controller's other work.
pub(crate) fn refuse_inside_cook_attempt(verb: &str, allowed: bool) -> homeboy::core::Result<()> {
    if allowed {
        return Ok(());
    }
    match enclosing_cook_attempt(&ancestry()) {
        Some(attempt) => Err(homeboy::core::Error::validation_invalid_argument(
            "allow_inside_cook",
            format!(
                "`{verb}` is a controller mutation and this process runs inside Cook attempt `{attempt}`; pass --allow-inside-cook to run it from a Cook"
            ),
            Some(attempt),
            None,
        )),
        None => Ok(()),
    }
}

/// This process and its parent chain up to init: pid, name, cwd.
fn ancestry() -> Vec<(u32, String, Option<std::path::PathBuf>)> {
    let mut chain = Vec::new();
    let mut pid = std::process::id();
    while pid > 1 && chain.len() < 16 {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            break;
        };
        // `pid (comm) state ppid ...`: comm may contain spaces and parens.
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            break;
        };
        let name = stat[open + 1..close].to_string();
        let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).ok();
        chain.push((pid, name, cwd));
        pid = stat[close + 1..]
            .split_whitespace()
            .nth(1)
            .and_then(|ppid| ppid.parse().ok())
            .unwrap_or(0);
    }
    chain
}

pub(crate) fn run(f: impl FnOnce() -> std::process::ExitCode) -> std::process::ExitCode {
    START.with_borrow_mut(|start| *start = Some((chrono::Utc::now().to_rfc3339(), Instant::now())));
    let caller_session = [
        "HOMEBOY_CALLER_SESSION",
        "HOMEBOY_CALLER_CONTEXT",
        "PI_SESSION_ID",
        "CLAUDE_SESSION_ID",
        "CODEX_THREAD_ID",
    ]
    .into_iter()
    .find_map(|key| std::env::var(key).ok().and_then(|v| opaque(&v)));
    CURRENT.with_borrow_mut(|row| {
        *row = Some(Invocation {
            caller_session,
            ..Default::default()
        })
    });
    let exit = f();
    if CURRENT.with_borrow(|row| row.as_ref().is_some_and(|row| !row.command_path.is_empty())) {
        finish(exit);
    } else {
        CURRENT.with_borrow_mut(|row| *row = None);
        START.with_borrow_mut(|start| *start = None);
    }
    exit
}
