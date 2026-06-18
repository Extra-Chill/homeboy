use crate::cli_surface::Commands;

use super::{map, JsonRun};
use crate::commands::{
    api, auth, ci, daemon, db, deploy, deps, doctor, file, git, http, issues, logs, self_cmd,
    server, ssh, status, triage, upgrade, GlobalArgs,
};

pub(super) fn dispatch(command: Commands, global: &GlobalArgs) -> JsonRun {
    match command {
        Commands::Status(args) => map(status::run(args, global)),
        Commands::Ci(args) => map(ci::run(args, global)),
        Commands::Ssh(args) => map(ssh::run(args, global)),
        Commands::Server(args) => map(server::run(args, global)),
        Commands::Db(args) => map(db::run(args, global)),
        Commands::Deps(args) => map(deps::run(args, global)),
        Commands::Doctor(args) => map(doctor::run(args, global)),
        Commands::File(args) => map(file::run(args, global)),
        Commands::Logs(args) => map(logs::run(args, global)),
        Commands::Triage(args) => map(triage::run(args, global)),
        Commands::Deploy(args) => map(deploy::run(args, global)),
        Commands::Daemon(args) => map(daemon::run(args, global)),
        Commands::Git(args) => map(git::run(args, global)),
        Commands::Issues(args) => map(issues::run(args, global)),
        Commands::SelfCmd(args) => map(self_cmd::run(args, global)),
        Commands::Auth(args) => map(auth::run(args, global)),
        Commands::Api(args) => map(api::run(args, global)),
        Commands::Http(args) => map(http::run(args, global)),
        Commands::Upgrade(args) => map(upgrade::run(args, global)),
        _ => unreachable!("command routed to wrong JSON output family"),
    }
}
