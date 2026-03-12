use clap::Args;

use homeboy::context;

use super::utils::args::HiddenJsonArgs;
use super::CmdResult;

#[derive(Args)]
pub struct InitArgs {
    /// Show all components, extensions, projects, and servers
    #[arg(long, short = 'a')]
    pub all: bool,

    #[command(flatten)]
    pub json_args: HiddenJsonArgs,
}

pub type InitOutput = homeboy::context::report::ContextReport;

pub fn run(args: InitArgs, _global: &super::GlobalArgs) -> CmdResult<InitOutput> {
    let mut report = context::build_report(args.all, "init")?;
    report
        .next_steps
        .insert(0, "Deprecated: prefer `homeboy status --full`.".to_string());
    Ok((report, 0))
}
