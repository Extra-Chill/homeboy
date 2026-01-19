use clap::Args;
use serde::Serialize;

use homeboy::module::ModuleRunner;

use super::CmdResult;

#[derive(Args)]
pub struct TestArgs {
    /// Component name to test
    component: String,

    /// Skip linting before running tests
    #[arg(long)]
    skip_lint: bool,

    /// Override settings as key=value pairs
    #[arg(long, value_parser = parse_key_val)]
    setting: Vec<(String, String)>,

    /// Additional arguments to pass to the test runner (after --)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[derive(Serialize)]
pub struct TestOutput {
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

pub fn run_json(args: TestArgs) -> CmdResult<TestOutput> {
    let output = ModuleRunner::new(&args.component, "test-runner.sh")
        .settings(&args.setting)
        .env_if(args.skip_lint, "HOMEBOY_SKIP_LINT", "1")
        .script_args(&args.args)
        .run()?;

    let status = if output.success { "passed" } else { "failed" };

    let mut hints = Vec::new();

    // Filter hint when tests fail and no passthrough args were used
    if !output.success && args.args.is_empty() {
        hints.push(format!(
            "To run specific tests: homeboy test {} -- --filter=TestName",
            args.component
        ));
    }

    // Capability hint when not using passthrough args
    if args.args.is_empty() {
        hints.push("Pass args to test runner: homeboy test <component> -- [args]".to_string());
    }

    // Always include docs reference
    hints.push("Full options: homeboy docs commands/test".to_string());

    let hints = if hints.is_empty() { None } else { Some(hints) };

    Ok((
        TestOutput {
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
