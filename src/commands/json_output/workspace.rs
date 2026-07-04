use crate::cli_surface::Commands;

use super::{map, JsonRun};
use crate::commands::{
    activity, agent_task, cleanup, component, config, contract, extension, project, refactor,
    release, report, rig, runner, runs, runtime, stack, tunnel, worktree, GlobalArgs,
};

pub(super) fn dispatch(command: Commands, global: &GlobalArgs) -> JsonRun {
    match command {
        Commands::Activity(args) => map(activity::run(args, global)),
        Commands::AgentTask(args) => map(agent_task::run(args, global)),
        Commands::Project(args) => map(project::run(args, global)),
        Commands::Component(args) => map(component::run(args, global)),
        Commands::Config(args) => map(config::run(args, global)),
        Commands::Contract(args) => map(contract::run(args, global)),
        Commands::Extension(args) => map(extension::run(args, global)),
        Commands::Cleanup(args) => map(cleanup::run(args, global)),
        Commands::Release(args) => map(release::run(args, global)),
        Commands::Report(args) => map(report::run(args, global)),
        Commands::Refactor(args) => map(refactor::run(args, global)),
        Commands::Rig(args) => map(rig::run(args, global)),
        Commands::Runner(args) => map(runner::run(args, global)),
        Commands::Runtime(args) => map(runtime::run(args, global)),
        Commands::Worktree(args) => map(worktree::run(args, global)),
        Commands::Tunnel(args) => map(tunnel::run(args, global)),
        Commands::Runs(args) => map(runs::run(args, global)),
        Commands::Stack(args) => map(stack::run(args, global)),
        _ => unreachable!("command routed to wrong JSON output family"),
    }
}
