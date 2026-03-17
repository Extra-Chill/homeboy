use clap::Args;
use serde::Serialize;
use std::path::PathBuf;

use homeboy::component;
use homeboy::engine::codebase_scan::{self, ScanConfig};
use homeboy::engine::validate_write::{self, ValidationResult};

use super::CmdResult;

#[derive(Args)]
pub struct ValidateArgs {
    /// Component ID (optional — auto-discovers from CWD if omitted)
    pub component_id: Option<String>,

    /// Override source path for validation
    #[arg(long)]
    pub path: Option<String>,
}

#[derive(Serialize)]
pub struct ValidateOutput {
    command: String,
    #[serde(flatten)]
    result: ValidationResult,
}

pub fn run(args: ValidateArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<ValidateOutput> {
    let comp =
        component::resolve_effective(args.component_id.as_deref(), args.path.as_deref(), None)?;

    let root = PathBuf::from(&comp.local_path);

    // Collect one source file so the validator can resolve the correct extension.
    // For project-level validators (cargo check, tsc), the file list doesn't
    // matter — they check the whole project. We just need one to detect the language.
    let changed_files: Vec<PathBuf> = codebase_scan::walk_files(&root, &ScanConfig::default())
        .into_iter()
        .take(1)
        .collect();

    let result = validate_write::validate_only(&root, &changed_files)?;

    let exit_code = if result.success { 0 } else { 1 };

    Ok((
        ValidateOutput {
            command: "validate".to_string(),
            result,
        },
        exit_code,
    ))
}
