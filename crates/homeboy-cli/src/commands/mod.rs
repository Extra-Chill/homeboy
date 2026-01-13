pub type CmdResult<T> = homeboy::Result<(T, i32)>;

pub(crate) struct GlobalArgs {
    pub(crate) dry_run: bool,
}

pub mod api;
pub mod auth;
pub mod build;
pub mod changelog;
pub mod cli;
pub mod component;
pub mod context;
pub mod db;
pub mod deploy;
pub mod docs;
pub mod file;
pub mod git;
pub mod init;
pub mod logs;
pub mod module;
pub mod project;
pub mod server;
pub mod ssh;
pub mod version;

pub(crate) fn run_markdown(
    command: crate::Commands,
    _global: &GlobalArgs,
) -> homeboy::Result<(String, i32)> {
    match command {
        crate::Commands::Docs(args) => docs::run_markdown(args),
        crate::Commands::Init(args) => init::run_markdown(args),
        crate::Commands::Changelog(args) => changelog::run_markdown(args),
        _ => Err(homeboy::Error::validation_invalid_argument(
            "output_mode",
            "Command does not support markdown output",
            None,
            None,
        )),
    }
}

pub(crate) fn run_json(
    command: crate::Commands,
    global: &GlobalArgs,
) -> (homeboy::Result<crate::output::CmdSuccess>, i32) {
    match command {
        crate::Commands::Init(_) => {
            let err = homeboy::Error::validation_invalid_argument(
                "output_mode",
                "Init command uses markdown output mode",
                None,
                None,
            );
            crate::output::map_cmd_result_to_json::<serde_json::Value>(Err(err))
        }
        crate::Commands::Project(args) => crate::output::map_cmd_result_to_json(
            project::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Ssh(args) => crate::output::map_cmd_result_to_json(
            ssh::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Server(args) => crate::output::map_cmd_result_to_json(
            server::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Db(args) => crate::output::map_cmd_result_to_json(
            db::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::File(args) => crate::output::map_cmd_result_to_json(
            file::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Logs(args) => crate::output::map_cmd_result_to_json(
            logs::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Deploy(args) => crate::output::map_cmd_result_to_json(
            deploy::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Component(args) => crate::output::map_cmd_result_to_json(
            component::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Context(args) => crate::output::map_cmd_result_to_json(
            context::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Module(args) => crate::output::map_cmd_result_to_json(
            module::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Docs(args) => crate::output::map_cmd_result_to_json(
            docs::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Changelog(args) => crate::output::map_cmd_result_to_json(
            changelog::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Git(args) => crate::output::map_cmd_result_to_json(
            git::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Version(args) => {
            crate::output::map_cmd_result_to_json(version::run(args, global))
        }
        crate::Commands::Build(args) => crate::output::map_cmd_result_to_json(
            build::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Auth(args) => crate::output::map_cmd_result_to_json(
            auth::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Api(args) => crate::output::map_cmd_result_to_json(
            api::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::List => {
            let err = homeboy::Error::validation_invalid_argument(
                "output_mode",
                "List command uses raw output mode",
                None,
                None,
            );
            crate::output::map_cmd_result_to_json::<serde_json::Value>(Err(err))
        }
    }
}
