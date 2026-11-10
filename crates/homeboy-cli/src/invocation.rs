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
            for key in ["run_id", "cook_id", "run", "cook"] {
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
