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
}

#[derive(Serialize)]
pub struct TestOutput {
    status: String,
    component: String,
    stdout: String,
    stderr: String,
    exit_code: i32,
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
        .run()?;

    let status = if output.success { "passed" } else { "failed" };

    Ok((
        TestOutput {
            status: status.to_string(),
            component: args.component,
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.exit_code,
        },
        output.exit_code,
    ))
}
