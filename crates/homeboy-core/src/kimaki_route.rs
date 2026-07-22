//! Best-effort kimaki origin-thread resolver.
//!
//! When homeboy runs inside a kimaki-launched session this module resolves the
//! Discord thread that originated the session so a run-completion notification
//! can report back automatically — without the operator passing
//! `--notification-route`.
//!
//! Resolution is layered and additive:
//!
//! 1. **Explicit CLI / env route** (handled by [`from_cli_or_env`] upstream).
//! 2. **Kimaki session → thread mapping** (this module).
//!
//! The resolver reads kimaki's published `discord-sessions.db` SQLite database
//! (read-only) at `${KIMAKI_DATA_DIR}/discord-sessions.db` and maps the
//! current kimaki session to its originating Discord thread.  The guild id is
//! sourced from [`KIMAKI_DISCORD_GUILD_ID_ENV`] (or a fallback env/config
//! value).
//!
//! ## Required session identity
//!
//! The env currently does **not** expose the kimaki `session_id` (`ses_…`)
//! directly.  For this resolver to activate, the kimaki launcher must export
//! one of:
//!
//! - [`KIMAKI_SESSION_ID_ENV`] (`KIMAKI_SESSION_ID`) — the canonical path.
//!
//! If the session id is not obtainable the resolver silently returns `None`,
//! preserving today's behaviour (no automatic report-back).
//!
//! ## Contract
//!
//! This module is **homeboy-owned** — it reads kimaki's published mapping but
//! does not mutate it.  Any lookup failure is best-effort: `None` is returned
//! and the cook proceeds normally.

use std::path::PathBuf;

use homeboy_lab_contract::notification_route::NOTIFICATION_ROUTE_ENV;
use homeboy_lab_contract::notification_route::{NotificationRoute, NOTIFICATION_TRANSPORT_ENV};

pub(crate) const KIMAKI_DATA_DIR_ENV: &str = "KIMAKI_DATA_DIR";
pub(crate) const KIMAKI_SESSION_ID_ENV: &str = "KIMAKI_SESSION_ID";
pub(crate) const KIMAKI_DISCORD_GUILD_ID_ENV: &str = "KIMAKI_DISCORD_GUILD_ID";

const DISCORD_TRANSPORT: &str = "discord.run-completion";
const DISCORD_ROUTE_PREFIX: &str = "discord:v1:thread:";

/// Attempt to resolve a notification route from the kimaki origin thread.
///
/// Returns `Some(NotificationRoute)` only when **all** of the following hold:
///
/// - `KIMAKI_DATA_DIR` is set and points to an accessible directory.
/// - `KIMAKI_SESSION_ID` is set (the kimaki launcher exports it).
/// - `KIMAKI_DISCORD_GUILD_ID` is set (the discord guild for thread routing).
/// - The `discord-sessions.db` SQLite database exists and contains a mapping
///   for the given session.
///
/// Any failure at any step is best-effort: `None` is returned.
pub fn resolve_origin_route() -> Option<NotificationRoute> {
    let data_dir = std::env::var(KIMAKI_DATA_DIR_ENV).ok()?;
    let session_id = std::env::var(KIMAKI_SESSION_ID_ENV).ok()?;
    let guild_id = std::env::var(KIMAKI_DISCORD_GUILD_ID_ENV).ok()?;

    if session_id.is_empty() || guild_id.is_empty() {
        return None;
    }

    let db_path = PathBuf::from(data_dir).join("discord-sessions.db");
    let thread_id = lookup_thread_id(&db_path, &session_id)?;

    let route_value = format!("{DISCORD_ROUTE_PREFIX}{guild_id}:{thread_id}");
    NotificationRoute::new(DISCORD_TRANSPORT, route_value).ok()
}

/// Look up the discord `thread_id` for `session_id` in the kimaki
/// `discord-sessions.db`.
///
/// Returns `None` on any error (file not found, query failure, no mapping)
/// — this resolver is strictly best-effort.
fn lookup_thread_id(db_path: &std::path::Path, session_id: &str) -> Option<String> {
    let connection = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;

    let thread_id: String = connection
        .query_row(
            "SELECT thread_id FROM thread_sessions WHERE session_id = ?1 LIMIT 1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )
        .ok()?;

    if thread_id.is_empty() {
        None
    } else {
        Some(thread_id)
    }
}

/// Return whether the current process appears to be running inside a kimaki
/// session (i.e. the kimaki data dir is present).
pub fn is_kimaki_session() -> bool {
    std::env::var(KIMAKI_DATA_DIR_ENV).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn temp_db(thread_id: &str, session_id: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("discord-sessions.db");
        let conn = rusqlite::Connection::open(&db_path).expect("open sqlite");
        conn.execute_batch(
            "CREATE TABLE thread_sessions (
                thread_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL
            );",
        )
        .expect("create table");
        conn.execute(
            "INSERT INTO thread_sessions (thread_id, session_id) VALUES (?1, ?2)",
            rusqlite::params![thread_id, session_id],
        )
        .expect("insert row");
        dir
    }

    #[test]
    fn resolves_route_from_kimaki_session() {
        let _lock = env_lock().lock().unwrap();
        let session_id = "ses_test_resolve";
        let thread_id = "1234567890";
        let guild_id = "9876543210";

        let dir = temp_db(thread_id, session_id);

        let old_data_dir = std::env::var(KIMAKI_DATA_DIR_ENV).ok();
        let old_session = std::env::var(KIMAKI_SESSION_ID_ENV).ok();
        let old_guild = std::env::var(KIMAKI_DISCORD_GUILD_ID_ENV).ok();

        std::env::set_var(KIMAKI_DATA_DIR_ENV, dir.path().to_str().unwrap());
        std::env::set_var(KIMAKI_SESSION_ID_ENV, session_id);
        std::env::set_var(KIMAKI_DISCORD_GUILD_ID_ENV, guild_id);

        let route = resolve_origin_route().expect("should resolve route");
        assert_eq!(route.transport, DISCORD_TRANSPORT);
        assert_eq!(
            route.route,
            format!("{DISCORD_ROUTE_PREFIX}{guild_id}:{thread_id}")
        );

        restore_env(KIMAKI_DATA_DIR_ENV, old_data_dir);
        restore_env(KIMAKI_SESSION_ID_ENV, old_session);
        restore_env(KIMAKI_DISCORD_GUILD_ID_ENV, old_guild);
    }

    #[test]
    fn returns_none_without_kimaki_data_dir() {
        let _lock = env_lock().lock().unwrap();
        let old_data_dir = std::env::var(KIMAKI_DATA_DIR_ENV).ok();
        let old_session = std::env::var(KIMAKI_SESSION_ID_ENV).ok();
        let old_guild = std::env::var(KIMAKI_DISCORD_GUILD_ID_ENV).ok();

        std::env::remove_var(KIMAKI_DATA_DIR_ENV);
        std::env::remove_var(KIMAKI_SESSION_ID_ENV);
        std::env::remove_var(KIMAKI_DISCORD_GUILD_ID_ENV);

        assert!(resolve_origin_route().is_none());

        restore_env(KIMAKI_DATA_DIR_ENV, old_data_dir);
        restore_env(KIMAKI_SESSION_ID_ENV, old_session);
        restore_env(KIMAKI_DISCORD_GUILD_ID_ENV, old_guild);
    }

    #[test]
    fn returns_none_with_missing_session_id() {
        let _lock = env_lock().lock().unwrap();
        let old_data_dir = std::env::var(KIMAKI_DATA_DIR_ENV).ok();
        let old_session = std::env::var(KIMAKI_SESSION_ID_ENV).ok();
        let old_guild = std::env::var(KIMAKI_DISCORD_GUILD_ID_ENV).ok();

        let dir = tempfile::tempdir().expect("tempdir");
        std::env::set_var(KIMAKI_DATA_DIR_ENV, dir.path().to_str().unwrap());
        std::env::remove_var(KIMAKI_SESSION_ID_ENV);
        std::env::set_var(KIMAKI_DISCORD_GUILD_ID_ENV, "guild");

        assert!(resolve_origin_route().is_none());

        restore_env(KIMAKI_DATA_DIR_ENV, old_data_dir);
        restore_env(KIMAKI_SESSION_ID_ENV, old_session);
        restore_env(KIMAKI_DISCORD_GUILD_ID_ENV, old_guild);
    }

    #[test]
    fn returns_none_with_missing_guild_id() {
        let _lock = env_lock().lock().unwrap();
        let old_data_dir = std::env::var(KIMAKI_DATA_DIR_ENV).ok();
        let old_session = std::env::var(KIMAKI_SESSION_ID_ENV).ok();
        let old_guild = std::env::var(KIMAKI_DISCORD_GUILD_ID_ENV).ok();

        let dir = tempfile::tempdir().expect("tempdir");
        std::env::set_var(KIMAKI_DATA_DIR_ENV, dir.path().to_str().unwrap());
        std::env::set_var(KIMAKI_SESSION_ID_ENV, "ses_test");
        std::env::remove_var(KIMAKI_DISCORD_GUILD_ID_ENV);

        assert!(resolve_origin_route().is_none());

        restore_env(KIMAKI_DATA_DIR_ENV, old_data_dir);
        restore_env(KIMAKI_SESSION_ID_ENV, old_session);
        restore_env(KIMAKI_DISCORD_GUILD_ID_ENV, old_guild);
    }

    #[test]
    fn returns_none_when_session_not_in_db() {
        let _lock = env_lock().lock().unwrap();
        let session_id = "ses_not_found";
        let guild_id = "guild123";

        let dir = temp_db("some_thread", "other_session");

        let old_data_dir = std::env::var(KIMAKI_DATA_DIR_ENV).ok();
        let old_session = std::env::var(KIMAKI_SESSION_ID_ENV).ok();
        let old_guild = std::env::var(KIMAKI_DISCORD_GUILD_ID_ENV).ok();

        std::env::set_var(KIMAKI_DATA_DIR_ENV, dir.path().to_str().unwrap());
        std::env::set_var(KIMAKI_SESSION_ID_ENV, session_id);
        std::env::set_var(KIMAKI_DISCORD_GUILD_ID_ENV, guild_id);

        assert!(resolve_origin_route().is_none());

        restore_env(KIMAKI_DATA_DIR_ENV, old_data_dir);
        restore_env(KIMAKI_SESSION_ID_ENV, old_session);
        restore_env(KIMAKI_DISCORD_GUILD_ID_ENV, old_guild);
    }

    #[test]
    fn is_kimaki_session_reflects_env() {
        let _lock = env_lock().lock().unwrap();
        let old = std::env::var(KIMAKI_DATA_DIR_ENV).ok();

        std::env::remove_var(KIMAKI_DATA_DIR_ENV);
        assert!(!is_kimaki_session());

        std::env::set_var(KIMAKI_DATA_DIR_ENV, "/tmp");
        assert!(is_kimaki_session());

        restore_env(KIMAKI_DATA_DIR_ENV, old);
    }

    #[test]
    fn explicit_cli_env_route_takes_precedence() {
        let _lock = env_lock().lock().unwrap();
        let session_id = "ses_should_not_matter";
        let thread_id = "9999";
        let guild_id = "1111";

        let dir = temp_db(thread_id, session_id);

        let old_data_dir = std::env::var(KIMAKI_DATA_DIR_ENV).ok();
        let old_session = std::env::var(KIMAKI_SESSION_ID_ENV).ok();
        let old_guild = std::env::var(KIMAKI_DISCORD_GUILD_ID_ENV).ok();
        let old_notif_transport = std::env::var(NOTIFICATION_TRANSPORT_ENV).ok();
        let old_notif_route = std::env::var(NOTIFICATION_ROUTE_ENV).ok();

        std::env::set_var(KIMAKI_DATA_DIR_ENV, dir.path().to_str().unwrap());
        std::env::set_var(KIMAKI_SESSION_ID_ENV, session_id);
        std::env::set_var(KIMAKI_DISCORD_GUILD_ID_ENV, guild_id);
        std::env::set_var(NOTIFICATION_TRANSPORT_ENV, "explicit.transport");
        std::env::set_var(NOTIFICATION_ROUTE_ENV, "explicit-route");

        let route = resolve_origin_route().expect("kimaki should still resolve");
        assert_eq!(route.transport, DISCORD_TRANSPORT);

        let cli_route = homeboy_lab_contract::notification_route::from_cli_or_env(
            Some("explicit.transport"),
            Some("explicit-route"),
        )
        .expect("cli route")
        .expect("should have route");
        assert_eq!(cli_route.transport, "explicit.transport");
        assert_eq!(cli_route.route, "explicit-route");

        restore_env(KIMAKI_DATA_DIR_ENV, old_data_dir);
        restore_env(KIMAKI_SESSION_ID_ENV, old_session);
        restore_env(KIMAKI_DISCORD_GUILD_ID_ENV, old_guild);
        restore_env(NOTIFICATION_TRANSPORT_ENV, old_notif_transport);
        restore_env(NOTIFICATION_ROUTE_ENV, old_notif_route);
    }

    fn restore_env(key: &str, old: Option<String>) {
        match old {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}
