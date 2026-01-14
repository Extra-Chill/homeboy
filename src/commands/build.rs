use clap::Args;
use homeboy::build;

use crate::commands::CmdResult;

#[derive(Args)]
pub struct BuildArgs {
    /// JSON input spec for bulk operations: {"componentIds": ["id1", "id2"]}
    #[arg(long)]
    pub json: Option<String>,

    /// Component ID (single build)
    pub component_id: Option<String>,
}

pub fn run(
    args: BuildArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<build::BuildResult> {
    // --json takes precedence, otherwise use component_id (auto-detected by core)
    let input = args.json.or(args.component_id).ok_or_else(|| {
        homeboy::Error::validation_invalid_argument(
            "input",
            "Provide component ID or JSON spec",
            None,
            None,
        )
    })?;

    build::run(&input)
}
