use clap::Args;
use serde::Serialize;

use homeboy::component;
use homeboy::git;
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

    /// Lint only files modified in the working tree (staged, unstaged, untracked)
    #[arg(long)]
    changed_only: bool,

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
    // Resolve glob from --changed-only flag
    let effective_glob = if args.changed_only {
        let component = component::load(&args.component)?;
        let uncommitted = git::get_uncommitted_changes(&component.local_path)?;

        // Collect all changed files
        let mut changed_files: Vec<String> = Vec::new();
        changed_files.extend(uncommitted.staged);
        changed_files.extend(uncommitted.unstaged);
        changed_files.extend(uncommitted.untracked);

        // Filter to only .php files
        let php_files: Vec<&str> = changed_files
            .iter()
            .filter(|f| f.ends_with(".php"))
            .map(|s| s.as_str())
            .collect();

        if php_files.is_empty() {
            // No PHP files changed - return early with success
            return Ok((
                LintOutput {
                    status: "passed".to_string(),
                    component: args.component,
                    stdout: "No PHP files in working tree changes".to_string(),
                    stderr: String::new(),
                    exit_code: 0,
                    hints: None,
                },
                0,
            ));
        }

        // Build brace expansion glob: {file1.php,file2.php,...}
        if php_files.len() == 1 {
            Some(php_files[0].to_string())
        } else {
            Some(format!("{{{}}}", php_files.join(",")))
        }
    } else {
        args.glob.clone()
    };

    let output = ModuleRunner::new(&args.component, "lint-runner.sh")
        .settings(&args.setting)
        .env_if(args.fix, "HOMEBOY_AUTO_FIX", "1")
        .env_if(args.summary, "HOMEBOY_SUMMARY_MODE", "1")
        .env_opt("HOMEBOY_LINT_FILE", &args.file)
        .env_opt("HOMEBOY_LINT_GLOB", &effective_glob)
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
