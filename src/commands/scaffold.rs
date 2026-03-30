use clap::{Args, Subcommand};
use homeboy::engine::execution_context::{self, ResolveOptions};
use homeboy::scaffold::{run_scaffold_workflow, ScaffoldWorkflowOutput};

use super::utils::args::{HiddenJsonArgs, PositionalComponentArgs};
use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct ScaffoldArgs {
    #[command(subcommand)]
    command: ScaffoldCommand,
}

#[derive(Subcommand)]
enum ScaffoldCommand {
    /// Scaffold test files from project conventions
    Test(TestScaffoldArgs),
}

#[derive(Args)]
struct TestScaffoldArgs {
    #[command(flatten)]
    comp: PositionalComponentArgs,

    /// Scaffold a specific source file (relative to component root)
    #[arg(long, value_name = "FILE")]
    file: Option<String>,

    /// Write scaffold files to disk (default: dry-run)
    #[arg(long)]
    write: bool,

    #[command(flatten)]
    _json: HiddenJsonArgs,
}

pub fn run(args: ScaffoldArgs, _global: &GlobalArgs) -> CmdResult<ScaffoldWorkflowOutput> {
    match args.command {
        ScaffoldCommand::Test(args) => run_test_scaffold(args),
    }
}

fn run_test_scaffold(args: TestScaffoldArgs) -> CmdResult<ScaffoldWorkflowOutput> {
    let effective_id = args.comp.resolve_id()?;

    let ctx = execution_context::resolve(&ResolveOptions::source_only(
        &effective_id,
        args.comp.path.clone(),
    ))?;

    let result = run_scaffold_workflow(
        &effective_id,
        &ctx.component,
        args.file.as_deref(),
        args.write,
    )?;

    Ok((result, 0))
}
