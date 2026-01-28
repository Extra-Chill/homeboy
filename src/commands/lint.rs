use clap::Args;
use serde::Serialize;

use homeboy::component::{self, Component};
use homeboy::error::Error;
use homeboy::git;
use homeboy::module::{self, ModuleRunner};

use super::CmdResult;

#[derive(Args)]
pub struct LintArgs {
    /// Component name to lint
    component: String,

    /// Auto-fix formatting issues before validating
    #[arg(long)]
    fix: bool,

    /// Show compact summary instead of full output
    #[arg(long)]
    summary: bool,

    /// Lint only a single file (path relative to component root)
    #[arg(long)]
    file: Option<String>,

    /// Lint only files matching glob pattern (e.g., "inc/**/*.php")
    #[arg(long)]
    glob: Option<String>,

    /// Lint only files modified in the working tree (staged, unstaged, untracked)
    #[arg(long)]
    changed_only: bool,

    /// Show only errors, suppress warnings
    #[arg(long)]
    errors_only: bool,

    /// Only check specific sniffs (comma-separated codes)
    #[arg(long)]
    sniffs: Option<String>,

    /// Exclude sniffs from checking (comma-separated codes)
    #[arg(long)]
    exclude_sniffs: Option<String>,

    /// Filter by category: security, i18n, yoda, whitespace
    #[arg(long)]
    category: Option<String>,

    /// Override settings as key=value pairs
    #[arg(long, value_parser = parse_key_val)]
    setting: Vec<(String, String)>,

    /// Accept --json for compatibility (output is JSON by default)
    #[arg(long, hide = true)]
    json: bool,
}

#[derive(Serialize)]
pub struct LintOutput {
    status: String,
    component: String,
    exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    hints: Option<Vec<String>>,
}

fn parse_key_val(s: &str) -> Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=value: no `=` found in `{s}`"))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}

fn resolve_lint_script(component: &Component) -> homeboy::error::Result<String> {
    let modules = component.modules.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "component",
            format!("Component '{}' has no modules configured", component.id),
            None,
            None,
        )
    })?;

    let module_id = if modules.contains_key("wordpress") {
        "wordpress"
    } else {
        modules.keys().next().ok_or_else(|| {
            Error::validation_invalid_argument(
                "component",
                format!("Component '{}' has no modules configured", component.id),
                None,
                None,
            )
        })?
    };

    let manifest = module::load_module(module_id)?;

    manifest
        .lint_script()
        .map(|s| s.to_string())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "module",
                format!(
                    "Module '{}' does not have lint infrastructure configured (missing lint.module_script)",
                    module_id
                ),
                None,
                None,
            )
        })
}

pub fn run_json(args: LintArgs) -> CmdResult<LintOutput> {
    let component = component::load(&args.component)?;
    let script_path = resolve_lint_script(&component)?;

    // Resolve glob from --changed-only flag
    let effective_glob = if args.changed_only {
        let uncommitted = git::get_uncommitted_changes(&component.local_path)?;

        // Collect all changed files
        let mut changed_files: Vec<String> = Vec::new();
        changed_files.extend(uncommitted.staged);
        changed_files.extend(uncommitted.unstaged);
        changed_files.extend(uncommitted.untracked);

        if changed_files.is_empty() {
            println!("No files in working tree changes");
            return Ok((
                LintOutput {
                    status: "passed".to_string(),
                    component: args.component,
                    exit_code: 0,
                    hints: None,
                },
                0,
            ));
        }

        // Pass ALL files to module - let lint runner filter to relevant types
        if changed_files.len() == 1 {
            Some(changed_files[0].clone())
        } else {
            Some(format!("{{{}}}", changed_files.join(",")))
        }
    } else {
        args.glob.clone()
    };

    let output = ModuleRunner::new(&args.component, &script_path)
        .settings(&args.setting)
        .env_if(args.fix, "HOMEBOY_AUTO_FIX", "1")
        .env_if(args.summary, "HOMEBOY_SUMMARY_MODE", "1")
        .env_opt("HOMEBOY_LINT_FILE", &args.file)
        .env_opt("HOMEBOY_LINT_GLOB", &effective_glob)
        .env_if(args.errors_only, "HOMEBOY_ERRORS_ONLY", "1")
        .env_opt("HOMEBOY_SNIFFS", &args.sniffs)
        .env_opt("HOMEBOY_EXCLUDE_SNIFFS", &args.exclude_sniffs)
        .env_opt("HOMEBOY_CATEGORY", &args.category)
        .run()?;

    let status = if output.success { "passed" } else { "failed" };

    let mut hints = Vec::new();

    // Fix hint when linting fails
    if !output.success && !args.fix {
        hints.push(format!(
            "Run 'homeboy lint {} --fix' to auto-fix formatting issues",
            args.component
        ));
        hints.push("Some issues may require manual fixes".to_string());
    }

    // Capability hints when running component-wide lint (no targeting options used)
    if args.file.is_none() && args.glob.is_none() && !args.changed_only {
        hints.push(
            "For targeted linting: --file <path>, --glob <pattern>, or --changed-only".to_string(),
        );
    }

    // Always include docs reference
    hints.push("Full options: homeboy docs commands/lint".to_string());

    let hints = if hints.is_empty() { None } else { Some(hints) };

    Ok((
        LintOutput {
            status: status.to_string(),
            component: args.component,
            exit_code: output.exit_code,
            hints,
        },
        output.exit_code,
    ))
}
