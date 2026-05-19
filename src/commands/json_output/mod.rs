use serde_json::Value;

use crate::cli_surface::Commands;

use super::{
    api, audit, auth, bench, build, changelog, changes, component, config, daemon, db, deploy,
    deps, docs, doctor, extension, file, fleet, git, http, issues, lint, logs, observe, project,
    refactor, release, report, review, rig, runner, runs, self_cmd, server, ssh, stack, status,
    test, trace, triage, undo, upgrade, version, GlobalArgs,
};

/// Dispatch a command to its handler and map the structured result to JSON.
pub fn run(command: Commands, global: &GlobalArgs) -> (homeboy::core::Result<Value>, i32) {
    crate::commands::utils::tty::status("homeboy is working...");

    dispatch(command, global)
}

fn dispatch(command: Commands, global: &GlobalArgs) -> (homeboy::core::Result<Value>, i32) {
    match command {
        Commands::Status(args) => map(status::run(args, global)),
        Commands::Test(args) => map(test::run(args, global)),
        Commands::Bench(args) => map(bench::run(args, global)),
        Commands::Trace(args) => map(trace::run(args, global)),
        Commands::Observe(args) => map(observe::run(args, global)),
        Commands::Lint(args) => map(lint::run(args, global)),
        Commands::Project(args) => map(project::run(args, global)),
        Commands::Ssh(args) => map(ssh::run(args, global)),
        Commands::Server(args) => map(server::run(args, global)),
        Commands::Db(args) => map(db::run(args, global)),
        Commands::Deps(args) => map(deps::run(args, global)),
        Commands::Doctor(args) => map(doctor::run(args, global)),
        Commands::File(args) => map(file::run(args, global)),
        Commands::Fleet(args) => map(fleet::run(args, global)),
        Commands::Logs(args) => map(logs::run(args, global)),
        Commands::Triage(args) => map(triage::run(args, global)),
        Commands::Deploy(args) => map(deploy::run(args, global)),
        Commands::Component(args) => map(component::run(args, global)),
        Commands::Config(args) => map(config::run(args, global)),
        Commands::Daemon(args) => map(daemon::run(args, global)),
        Commands::Extension(args) => map(extension::run(args, global)),
        Commands::Docs(args) => map(docs::run(args, global)),
        Commands::Changelog(args) => map(changelog::run(args, global)),
        Commands::Git(args) => map(git::run(args, global)),
        Commands::Issues(args) => map(issues::run(args, global)),
        Commands::Version(args) => map(version::run(args, global)),
        Commands::Build(args) => map(build::run(args, global)),
        Commands::Changes(args) => map(changes::run(args, global)),
        Commands::Release(args) => map(release::run(args, global)),
        Commands::Report(args) => map(report::run(args, global)),
        Commands::Review(args) => map(review::run(args, global)),
        Commands::Audit(args) => map(audit::run(args, global)),
        Commands::Refactor(args) => map(refactor::run(args, global)),
        Commands::Rig(args) => map(rig::run(args, global)),
        Commands::Runner(args) => map(runner::run(args, global)),
        Commands::Runs(args) => map(runs::run(args, global)),
        Commands::SelfCmd(args) => map(self_cmd::run(args, global)),
        Commands::Stack(args) => map(stack::run(args, global)),
        Commands::Undo(args) => map(undo::run(args, global)),
        Commands::Auth(args) => map(auth::run(args, global)),
        Commands::Api(args) => map(api::run(args, global)),
        Commands::Http(args) => map(http::run(args, global)),
        Commands::Upgrade(args) => map(upgrade::run(args, global)),
        Commands::List => unsupported_raw_command("List command uses raw output mode"),
    }
}

fn map<T: serde::Serialize>(result: super::CmdResult<T>) -> (homeboy::core::Result<Value>, i32) {
    crate::commands::utils::response::map_cmd_result_to_json(result)
}

fn unsupported_raw_command(message: &'static str) -> (homeboy::core::Result<Value>, i32) {
    let err = homeboy::core::Error::validation_invalid_argument("output_mode", message, None, None);
    crate::commands::utils::response::map_cmd_result_to_json::<Value>(Err(err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_json_dispatch_reports_raw_output_mode() {
        let (result, exit_code) = dispatch(Commands::List, &GlobalArgs {});

        assert_ne!(exit_code, 0);
        assert!(result
            .expect_err("list should not dispatch as JSON")
            .to_string()
            .contains("raw output mode"));
    }
}
