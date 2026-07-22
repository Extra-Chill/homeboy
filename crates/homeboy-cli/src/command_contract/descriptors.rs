//! Declarative registrations for command families migrated off parallel fan-out lists.

/// Expands the Ops command descriptors into a consumer macro.
///
/// Each row owns the command module, parsed Clap variant, contract metadata, and
/// JSON handler binding. Consumers select the fields they need while preserving
/// command-owned dynamic output and Lab predicates.
#[macro_export]
macro_rules! ops_command_descriptors {
    ($consumer:ident) => {
        $consumer! {
            (ssh, Ssh, crate::commands::ssh::SshArgs, command_spec("ssh", CommandJsonFamily::Ops), crate::commands::ssh::run),
            (server, Server, crate::commands::server::ServerArgs, CommandSpec { subcommand_safety: SERVER_SUBCOMMAND_SAFETY, ..command_spec("server", CommandJsonFamily::Ops) }, crate::commands::server::run),
            (db, Db, crate::commands::db::DbArgs, command_spec("db", CommandJsonFamily::Ops), crate::commands::db::run),
            (file, File, crate::commands::file::FileArgs, CommandSpec { subcommand_safety: FILE_SUBCOMMAND_SAFETY, ..command_spec("file", CommandJsonFamily::Ops) }, crate::commands::file::run),
            (logs, Logs, crate::commands::logs::LogsArgs, command_spec("logs", CommandJsonFamily::Ops), crate::commands::logs::run),
            (triage, Triage, crate::commands::triage::TriageArgs, command_spec_with_safety("triage", CommandJsonFamily::Ops, operator_safety(None, TRIAGE_DANGEROUS_FLAGS)), crate::commands::triage::run),
            (deploy, Deploy, crate::commands::deploy::DeployArgs, command_spec_with_safety("deploy", CommandJsonFamily::Ops, operator_safety(Some("--dry-run"), DEPLOY_DANGEROUS_FLAGS)), crate::commands::deploy::run),
            (daemon, Daemon, crate::commands::daemon::DaemonArgs, command_spec("daemon", CommandJsonFamily::Ops), crate::commands::daemon::run),
            (status, Status, crate::commands::status::StatusArgs, command_spec("status", CommandJsonFamily::Ops), crate::commands::status::run),
            (git, Git, crate::commands::git::GitArgs, command_spec("git", CommandJsonFamily::Ops), crate::commands::git::run),
            (self_cmd, SelfCmd, crate::commands::self_cmd::SelfArgs, command_spec_with_output_notes("self", CommandJsonFamily::Ops, "inspects the active Homeboy runtime and renders built-in CLI documentation"), crate::commands::self_cmd::run),
            (api, Api, crate::commands::api::ApiArgs, CommandSpec { subcommand_safety: API_SUBCOMMAND_SAFETY, ..command_spec("api", CommandJsonFamily::Ops) }, crate::commands::api::run),
            (upgrade, Upgrade, crate::commands::upgrade::UpgradeArgs, command_spec_with_output_notes_and_safety("upgrade", CommandJsonFamily::Ops, "upgrades the active Homeboy binary, extensions, runners, and services unless --check or skip flags are used", operator_safety(None, UPGRADE_DANGEROUS_FLAGS)), crate::commands::upgrade::run),
        }
    };
}

/// Commands-free spec table for the ops command family.
///
/// This mirrors the `$spec` field of [`ops_command_descriptors`] but omits the
/// `crate::commands` Args type and handler binding, so it can be expanded inside
/// `command_contract` (e.g. `spec.rs`) without depending on the `commands`
/// module. The full descriptor macro (with Args + handler) is expanded only on
/// the CLI side.
#[macro_export]
macro_rules! ops_command_spec {
    (ssh) => { command_spec("ssh", CommandJsonFamily::Ops) };
    (server) => { CommandSpec { subcommand_safety: SERVER_SUBCOMMAND_SAFETY, ..command_spec("server", CommandJsonFamily::Ops) } };
    (db) => { command_spec("db", CommandJsonFamily::Ops) };
    (file) => { CommandSpec { subcommand_safety: FILE_SUBCOMMAND_SAFETY, ..command_spec("file", CommandJsonFamily::Ops) } };
    (logs) => { command_spec("logs", CommandJsonFamily::Ops) };
    (triage) => { command_spec_with_safety("triage", CommandJsonFamily::Ops, operator_safety(None, TRIAGE_DANGEROUS_FLAGS)) };
    (deploy) => { command_spec_with_safety("deploy", CommandJsonFamily::Ops, operator_safety(Some("--dry-run"), DEPLOY_DANGEROUS_FLAGS)) };
    (daemon) => { command_spec("daemon", CommandJsonFamily::Ops) };
    (status) => { command_spec("status", CommandJsonFamily::Ops) };
    (git) => { command_spec("git", CommandJsonFamily::Ops) };
    (self_cmd) => { command_spec_with_output_notes("self", CommandJsonFamily::Ops, "inspects the active Homeboy runtime and renders built-in CLI documentation") };
    (api) => { CommandSpec { subcommand_safety: API_SUBCOMMAND_SAFETY, ..command_spec("api", CommandJsonFamily::Ops) } };
    (upgrade) => { command_spec_with_output_notes_and_safety("upgrade", CommandJsonFamily::Ops, "upgrades the active Homeboy binary, extensions, runners, and services unless --check or skip flags are used", operator_safety(None, UPGRADE_DANGEROUS_FLAGS)) };
}
