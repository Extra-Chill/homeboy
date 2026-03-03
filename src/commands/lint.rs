use clap::Args;
use serde::Serialize;

use homeboy::component::{self, Component};
use homeboy::error::Error;
use homeboy::extension::{self, ExtensionRunner};
use homeboy::git;

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
    #[arg(long, conflicts_with = "changed_since")]
    changed_only: bool,

    /// Lint only files changed since a git ref (branch, tag, or SHA) — CI-friendly
    #[arg(long, conflicts_with = "changed_only")]
    changed_since: Option<String>,

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
    #[arg(long, value_parser = super::parse_key_val)]
    setting: Vec<(String, String)>,

    /// Override local_path for this lint run (use a workspace clone or temp checkout)
    #[arg(long)]
    path: Option<String>,

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

fn resolve_lint_script(component: &Component) -> homeboy::error::Result<String> {
    let extensions = component.extensions.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "component",
            format!("Component '{}' has no extensions configured", component.id),
            None,
            None,
        )
        .with_hint(format!(
            "Add a extension: homeboy component set {} --extension <extension_id>",
            component.id
        ))
    })?;

    let extension_id = if extensions.contains_key("wordpress") {
        "wordpress"
    } else {
        extensions.keys().next().ok_or_else(|| {
            Error::validation_invalid_argument(
                "component",
                format!("Component '{}' has no extensions configured", component.id),
                None,
                None,
            )
            .with_hint(format!(
                "Add a extension: homeboy component set {} --extension <extension_id>",
                component.id
            ))
        })?
    };

    let manifest = extension::load_extension(extension_id)?;

    manifest
        .lint_script()
        .map(|s| s.to_string())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "extension",
                format!(
                    "Extension '{}' does not have lint infrastructure configured (missing lint.extension_script)",
                    extension_id
                ),
                None,
                None,
            )
        })
}

pub fn run(args: LintArgs, _global: &super::GlobalArgs) -> CmdResult<LintOutput> {
    let mut component = component::load(&args.component)?;
    if let Some(ref path) = args.path {
        component.local_path = path.clone();
    }
    let script_path = resolve_lint_script(&component)?;

    // Resolve glob from --changed-only or --changed-since flags
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

        // Make paths absolute so lint runners can find files regardless of
        // the shell's working directory (git status returns repo-relative paths)
        let abs_files: Vec<String> = changed_files
            .iter()
            .map(|f| format!("{}/{}", component.local_path, f))
            .collect();

        // Pass ALL files to extension - let lint runner filter to relevant types
        if abs_files.len() == 1 {
            Some(abs_files[0].clone())
        } else {
            Some(format!("{{{}}}", abs_files.join(",")))
        }
    } else if let Some(ref git_ref) = args.changed_since {
        let changed_files = git::get_files_changed_since(&component.local_path, git_ref)?;

        if changed_files.is_empty() {
            println!("No files changed since {}", git_ref);
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

        // Make paths absolute (git diff returns repo-relative paths)
        let abs_files: Vec<String> = changed_files
            .iter()
            .map(|f| format!("{}/{}", component.local_path, f))
            .collect();

        if abs_files.len() == 1 {
            Some(abs_files[0].clone())
        } else {
            Some(format!("{{{}}}", abs_files.join(",")))
        }
    } else {
        args.glob.clone()
    };

    let output = ExtensionRunner::new(&args.component, &script_path)
        .path_override(args.path.clone())
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
    if args.file.is_none()
        && args.glob.is_none()
        && !args.changed_only
        && args.changed_since.is_none()
    {
        hints.push(
            "For targeted linting: --file <path>, --glob <pattern>, --changed-only, or --changed-since <ref>".to_string(),
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
