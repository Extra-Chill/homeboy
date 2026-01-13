pub type CmdResult<T> = homeboy_core::Result<(T, i32)>;

pub(crate) struct GlobalArgs {
    pub(crate) dry_run: bool,
}

pub mod api;
pub mod auth;
pub mod build;
pub mod changelog;
pub mod cli;
pub mod component;
pub mod config;
pub mod context;
pub mod db;
pub mod deploy;
pub mod docs;
pub mod doctor;
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
) -> homeboy_core::Result<(String, i32)> {
    match command {
        crate::Commands::Docs(args) => docs::run_markdown(args),
        crate::Commands::Init(args) => init::run_markdown(args),
        crate::Commands::Changelog(args) => changelog::run_markdown(args),
        _ => Err(homeboy_core::Error::other(
            "Invalid raw markdown response mode".to_string(),
        )),
    }
}

pub(crate) fn run_json(
    command: crate::Commands,
    global: &GlobalArgs,
) -> (homeboy_core::Result<homeboy_core::output::CmdSuccess>, i32) {
    match command {
        crate::Commands::Init(_) => {
            let err = homeboy_core::Error::other("Init uses markdown output mode".to_string());
            homeboy_core::output::map_cmd_result_to_json::<serde_json::Value>(Err(err))
        }
        crate::Commands::Project(args) => homeboy_core::output::map_cmd_result_to_json(
            project::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Ssh(args) => homeboy_core::output::map_cmd_result_to_json(
            ssh::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Server(args) => homeboy_core::output::map_cmd_result_to_json(
            server::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Db(args) => homeboy_core::output::map_cmd_result_to_json(
            db::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::File(args) => homeboy_core::output::map_cmd_result_to_json(
            file::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Logs(args) => homeboy_core::output::map_cmd_result_to_json(
            logs::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Deploy(args) => homeboy_core::output::map_cmd_result_to_json(
            deploy::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Component(args) => homeboy_core::output::map_cmd_result_to_json(
            component::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Config(args) => homeboy_core::output::map_cmd_result_to_json(
            config::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Context(args) => homeboy_core::output::map_cmd_result_to_json(
            context::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Module(args) => homeboy_core::output::map_cmd_result_to_json(
            module::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Docs(args) => homeboy_core::output::map_cmd_result_to_json(
            docs::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Changelog(args) => homeboy_core::output::map_cmd_result_to_json(
            changelog::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Git(args) => homeboy_core::output::map_cmd_result_to_json(
            git::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Version(args) => {
            homeboy_core::output::map_cmd_result_to_json(version::run(args, global))
        }
        crate::Commands::Build(args) => homeboy_core::output::map_cmd_result_to_json(
            build::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Doctor(args) => homeboy_core::output::map_cmd_result_to_json(
            doctor::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Auth(args) => homeboy_core::output::map_cmd_result_to_json(
            auth::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::Api(args) => homeboy_core::output::map_cmd_result_to_json(
            api::run(args, global).map(|(data, exit_code)| (data, vec![], exit_code)),
        ),
        crate::Commands::List => {
            let err = homeboy_core::Error::other("List uses raw output mode".to_string());
            homeboy_core::output::map_cmd_result_to_json::<serde_json::Value>(Err(err))
        }
    }
}
