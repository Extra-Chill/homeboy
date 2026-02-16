use clap::Args;
use serde::Serialize;

use homeboy::component::{self, Component};
use homeboy::error::Error;
use homeboy::module::{self, ModuleRunner};

use super::CmdResult;

#[derive(Args)]
pub struct TestArgs {
    /// Component name to test
    component: String,

    /// Skip linting before running tests
    #[arg(long)]
    skip_lint: bool,

    /// Auto-fix linting issues before running tests
    #[arg(long)]
    fix: bool,

    /// Override settings as key=value pairs
    #[arg(long, value_parser = parse_key_val)]
    setting: Vec<(String, String)>,

    /// Additional arguments to pass to the test runner (after --)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,

    /// Accept --json for compatibility (output is JSON by default)
    #[arg(long, hide = true)]
    json: bool,
}

#[derive(Serialize)]
pub struct TestOutput {
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

/// Attempt to auto-detect the module for a component based on contextual clues.
fn auto_detect_module(component: &Component) -> Option<String> {
    // Check build_command for module references (e.g., "modules/wordpress/scripts/build")
    if let Some(ref cmd) = component.build_command {
        if cmd.contains("modules/wordpress") {
            return Some("wordpress".to_string());
        }
    }

    // Check for composer.json in local_path (indicates WordPress/PHP component)
    let expanded = shellexpand::tilde(&component.local_path);
    let composer_path = std::path::Path::new(expanded.as_ref()).join("composer.json");
    if composer_path.exists() {
        return Some("wordpress".to_string());
    }

    None
}

fn no_modules_error(component: &Component) -> Error {
    Error::validation_invalid_argument(
        "component",
        format!("Component '{}' has no modules configured and none could be auto-detected", component.id),
        None,
        None,
    )
    .with_hint(format!(
        "Add a module: homeboy component set {} --module wordpress",
        component.id
    ))
}

fn resolve_test_script(component: &Component) -> homeboy::error::Result<String> {
    let module_id_owned: String;
    let module_id: &str = if let Some(ref modules) = component.modules {
        if modules.contains_key("wordpress") {
            "wordpress"
        } else if let Some(key) = modules.keys().next() {
            key.as_str()
        } else if let Some(detected) = auto_detect_module(component) {
            module_id_owned = detected;
            &module_id_owned
        } else {
            return Err(no_modules_error(component));
        }
    } else if let Some(detected) = auto_detect_module(component) {
        module_id_owned = detected;
        &module_id_owned
    } else {
        return Err(no_modules_error(component));
    };

    let manifest = module::load_module(module_id)?;

    manifest
        .test_script()
        .map(|s| s.to_string())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "module",
                format!(
                    "Module '{}' does not have test infrastructure configured (missing test.module_script)",
                    module_id
                ),
                None,
                None,
            )
        })
}

pub fn run_json(args: TestArgs) -> CmdResult<TestOutput> {
    let component = component::load(&args.component)?;
    let script_path = resolve_test_script(&component)?;

    let output = ModuleRunner::new(&args.component, &script_path)
        .settings(&args.setting)
        .env_if(args.skip_lint, "HOMEBOY_SKIP_LINT", "1")
        .env_if(args.fix, "HOMEBOY_AUTO_FIX", "1")
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

    // Fix hint when lint is enabled (default) and --fix not used
    if !args.skip_lint && !args.fix {
        hints.push(format!(
            "Auto-fix lint issues: homeboy test {} --fix",
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
            exit_code: output.exit_code,
            hints,
        },
        output.exit_code,
    ))
}
