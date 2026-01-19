use clap::Args;
use serde::Serialize;

use homeboy::module::ModuleRunner;

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

    /// Show only errors, suppress warnings
    #[arg(long)]
    errors_only: bool,

    /// Override settings as key=value pairs
    #[arg(long, value_parser = parse_key_val)]
    setting: Vec<(String, String)>,
}

#[derive(Serialize)]
pub struct LintOutput {
    status: String,
    component: String,
    stdout: String,
    stderr: String,
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

pub fn run_json(args: LintArgs) -> CmdResult<LintOutput> {
    let output = ModuleRunner::new(&args.component, "lint-runner.sh")
        .settings(&args.setting)
        .env_if(args.fix, "HOMEBOY_AUTO_FIX", "1")
        .env_if(args.summary, "HOMEBOY_SUMMARY_MODE", "1")
        .env_opt("HOMEBOY_LINT_FILE", &args.file)
        .env_opt("HOMEBOY_LINT_GLOB", &args.glob)
        .env_if(args.errors_only, "HOMEBOY_ERRORS_ONLY", "1")
        .run()?;

    let status = if output.success { "passed" } else { "failed" };

    let hints = if !output.success && !args.fix {
        Some(vec![
            format!(
                "Run 'homeboy lint {} --fix' to auto-fix formatting issues",
                args.component
            ),
            "Some issues may require manual fixes".to_string(),
        ])
    } else {
        None
    };

    Ok((
        LintOutput {
            status: status.to_string(),
            component: args.component,
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.exit_code,
            hints,
        },
        output.exit_code,
    ))
}
