use crate::cli_surface::Commands;

use super::{map, JsonRun};
use crate::commands::{
    activity, agent_task, cleanup, component, config, extension, project, refactor, release,
    report, rig, runner, runs, runtime, stack, tunnel, worktree,
};

pub(super) fn dispatch(command: Commands) -> JsonRun {
    match command {
        Commands::Activity(args) => map(activity::run(args)),
        Commands::AgentTask(args) => map(agent_task::run(args)),
        Commands::Project(args) => map(project::run(args)),
        Commands::Component(args) => map(component::run(args)),
        Commands::Config(args) => map(config::run(args)),
        Commands::Extension(args) => map(extension::run(args)),
        Commands::Cleanup(args) => map(cleanup::run(args)),
        Commands::Release(args) => map(release::run(args)),
        Commands::Report(args) => map(report::run(args)),
        Commands::Refactor(args) => map(refactor::run(args)),
        Commands::Rig(args) => map(rig::run(args)),
        Commands::Runner(args) => map(runner::run(args)),
        Commands::Runtime(args) => map(runtime::run(args)),
        Commands::Worktree(args) => map(worktree::run(args)),
        Commands::Tunnel(args) => map(tunnel::run(args)),
        Commands::Runs(args) => map(runs::run(args)),
        Commands::Stack(args) => map(stack::run(args)),
        _ => unreachable!("command routed to wrong JSON output family"),
    }
}
