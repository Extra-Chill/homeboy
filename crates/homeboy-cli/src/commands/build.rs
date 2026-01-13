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

pub fn run(args: BuildArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<build::BuildResult> {
    let input = match (&args.json, &args.component_id) {
        (Some(json), _) => json.as_str(),
        (None, Some(id)) => id.as_str(),
        (None, None) => {
            return Err(homeboy::Error::validation_invalid_argument(
                "input",
                "Provide component ID or --json spec",
                None,
                None,
            ))
        }
    };

    build::run(input)
}
