//! Declarative registrations for command families migrated off parallel fan-out lists.

/// Expands the Ops command descriptors into a consumer macro.
///
/// Each row binds a command module to its parsed Clap variant and JSON handler.
///
/// This table does **not** declare the command modules. It used to: a
/// `register_ops_command_modules!` consumer expanded `$(pub mod $module;)*`.
/// rustfmt resolves the module tree by parsing and does not expand
/// `macro_rules!`, so every module declared that way was invisible to
/// `cargo fmt --all` -- fifteen subtrees, 33 files, drifting unformatted. The
/// declarations now live as literal `pub mod` lines in `commands/mod.rs`.
/// Removing them from here cost no safety: `$handler` names
/// `crate::commands::<module>::run`, so a row without a declared module does
/// not compile.
///
/// Contract metadata (a `CommandSpec`) is deliberately **not** a column here. It
/// lives once, in [`ops_command_spec`], because `spec.rs` splices the ops rows
/// into a hand-ordered `COMMANDS` array at fifteen non-contiguous positions —
/// a shape this block-emitting macro cannot produce. Carrying a second copy of
/// the spec expressions in this table bought nothing: neither consumer ever
/// referenced it, so the copies expanded to no code while still requiring every
/// safety-metadata edit to be made twice.
///
/// Registry/parser parity is enforced by
/// `cli_surface::tests::command_registry_manifest_and_docs_metadata_align`,
/// which asserts a bijection between the clap subcommand set and
/// `COMMAND_SPECS`. That guard is stronger than macro co-location: it catches a
/// missing spec for *any* command, not just the ops family.
#[macro_export]
macro_rules! ops_command_descriptors {
    ($consumer:ident) => {
        $consumer! {
            (ssh, Ssh, crate::commands::ssh::run),
            (server, Server, crate::commands::server::run),
            (db, Db, crate::commands::db::run),
            (file, File, crate::commands::file::run),
            (logs, Logs, crate::commands::logs::run),
            (triage, Triage, crate::commands::triage::run),
            (deploy, Deploy, crate::commands::deploy::run),
            (harvest, Harvest, crate::commands::harvest::run),
            (daemon, Daemon, crate::commands::daemon::run),
            (
                deferred_workload,
                DeferredWorkload,
                crate::commands::deferred_workload::run
            ),
            (schedule, Schedule, crate::commands::schedule::run),
            (status, Status, crate::commands::status::run),
            (git, Git, crate::commands::git::run),
            (self_cmd, SelfCmd, crate::commands::self_cmd::run),
            (api, Api, crate::commands::api::run),
            (upgrade, Upgrade, crate::commands::upgrade::run),
        }
    };
}

/// Canonical `CommandSpec` table for the ops command family.
///
/// This is the single source of truth for ops contract metadata. It is expanded
/// inside `command_contract` (`spec.rs`), which cannot name `crate::commands`
/// types, so it deliberately holds no Args type or handler binding — those live
/// in [`ops_command_descriptors`], which is expanded only on the CLI side.
///
/// The per-name arms exist because `spec.rs` interleaves these rows with
/// non-ops entries in one hand-ordered array; a single block-emitting arm could
/// not be spliced into those positions.
#[macro_export]
macro_rules! ops_command_spec {
    (ssh) => { command_spec("ssh", CommandJsonFamily::Ops) };
    (server) => { CommandSpec { subcommand_safety: SERVER_SUBCOMMAND_SAFETY, ..command_spec("server", CommandJsonFamily::Ops) } };
    (db) => { CommandSpec { subcommand_safety: DB_SUBCOMMAND_SAFETY, ..command_spec("db", CommandJsonFamily::Ops) } };
    (file) => { CommandSpec { subcommand_safety: FILE_SUBCOMMAND_SAFETY, ..command_spec("file", CommandJsonFamily::Ops) } };
    (logs) => { command_spec("logs", CommandJsonFamily::Ops) };
    (triage) => { command_spec_with_safety("triage", CommandJsonFamily::Ops, operator_safety(None, TRIAGE_DANGEROUS_FLAGS)) };
    (deploy) => { command_spec_with_safety("deploy", CommandJsonFamily::Ops, operator_safety(Some("--dry-run"), DEPLOY_DANGEROUS_FLAGS)) };
    (harvest) => { command_spec_with_safety("harvest", CommandJsonFamily::Ops, operator_safety(Some("--dry-run"), &["--apply"])) };
    (daemon) => { command_spec("daemon", CommandJsonFamily::Ops) };
    (deferred_workload) => { command_spec("deferred-workload", CommandJsonFamily::Ops) };
    (schedule) => { command_spec("schedule", CommandJsonFamily::Ops) };
    (status) => { command_spec("status", CommandJsonFamily::Ops) };
    (git) => { CommandSpec { subcommand_safety: GIT_SUBCOMMAND_SAFETY, ..command_spec("git", CommandJsonFamily::Ops) } };
    (self_cmd) => { CommandSpec { subcommand_safety: SELF_SUBCOMMAND_SAFETY, ..command_spec_with_output_notes("self", CommandJsonFamily::Ops, "inspects the active Homeboy runtime and renders built-in CLI documentation") } };
    (api) => { CommandSpec { subcommand_safety: API_SUBCOMMAND_SAFETY, ..command_spec("api", CommandJsonFamily::Ops) } };
    (upgrade) => { command_spec_with_output_notes_and_safety("upgrade", CommandJsonFamily::Ops, "upgrades the active Homeboy binary, extensions, runners, and services unless --check or skip flags are used", operator_safety(None, UPGRADE_DANGEROUS_FLAGS)) };
}
