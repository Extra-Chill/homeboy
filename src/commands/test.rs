use clap::Args;
use serde::Serialize;

use homeboy::component::{self, Component};
use homeboy::error::Error;
use homeboy::extension::{self, ExtensionRunner};
use homeboy::utils::io;

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

    /// Collect code coverage (requires xdebug/pcov for PHP, cargo-tarpaulin for Rust)
    #[arg(long)]
    coverage: bool,

    /// Minimum coverage percentage — fail if below this threshold (implies --coverage)
    #[arg(long, value_name = "PERCENT")]
    coverage_min: Option<f64>,

    /// Override settings as key=value pairs
    #[arg(long, value_parser = super::parse_key_val)]
    setting: Vec<(String, String)>,

    /// Override local_path for this test run (use a workspace clone or temp checkout)
    #[arg(long)]
    path: Option<String>,

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
    coverage: Option<CoverageOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hints: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct CoverageOutput {
    lines_pct: f64,
    lines_total: u64,
    lines_covered: u64,
    methods_pct: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    uncovered_files: Vec<UncoveredFile>,
}

#[derive(Serialize)]
pub struct UncoveredFile {
    file: String,
    line_pct: f64,
}

/// Attempt to auto-detect the extension for a component based on contextual clues.
fn auto_detect_extension(component: &Component) -> Option<String> {
    // Check build_command for extension references (e.g., "extensions/wordpress/scripts/build")
    if let Some(ref cmd) = component.build_command {
        if cmd.contains("extensions/wordpress") {
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

fn no_extensions_error(component: &Component) -> Error {
    Error::validation_invalid_argument(
        "component",
        format!(
            "Component '{}' has no extensions configured and none could be auto-detected",
            component.id
        ),
        None,
        None,
    )
    .with_hint(format!(
        "Add a extension: homeboy component set {} --extension wordpress",
        component.id
    ))
}

fn resolve_test_script(component: &Component) -> homeboy::error::Result<String> {
    let extension_id_owned: String;
    let extension_id: &str = if let Some(ref extensions) = component.extensions {
        if extensions.contains_key("wordpress") {
            "wordpress"
        } else if let Some(key) = extensions.keys().next() {
            key.as_str()
        } else if let Some(detected) = auto_detect_extension(component) {
            extension_id_owned = detected;
            &extension_id_owned
        } else {
            return Err(no_extensions_error(component));
        }
    } else if let Some(detected) = auto_detect_extension(component) {
        extension_id_owned = detected;
        &extension_id_owned
    } else {
        return Err(no_extensions_error(component));
    };

    let manifest = extension::load_extension(extension_id)?;

    manifest
        .test_script()
        .map(|s| s.to_string())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "extension",
                format!(
                    "Extension '{}' does not have test infrastructure configured (missing test.extension_script)",
                    extension_id
                ),
                None,
                None,
            )
        })
}

pub fn run(args: TestArgs, _global: &super::GlobalArgs) -> CmdResult<TestOutput> {
    let mut component = component::load(&args.component)?;
    if let Some(ref path) = args.path {
        component.local_path = path.clone();
    }
    let script_path = resolve_test_script(&component)?;

    // Coverage is enabled by --coverage or --coverage-min
    let coverage_enabled = args.coverage || args.coverage_min.is_some();

    // Create temp file for coverage output
    let coverage_file = if coverage_enabled {
        Some(std::env::temp_dir().join(format!("homeboy-coverage-{}.json", std::process::id())))
    } else {
        None
    };

    let mut runner = ExtensionRunner::new(&args.component, &script_path)
        .path_override(args.path.clone())
        .settings(&args.setting)
        .env_if(args.skip_lint, "HOMEBOY_SKIP_LINT", "1")
        .env_if(args.fix, "HOMEBOY_AUTO_FIX", "1")
        .env_if(coverage_enabled, "HOMEBOY_COVERAGE", "1");

    if let Some(ref file) = coverage_file {
        runner = runner.env("HOMEBOY_COVERAGE_FILE", &file.to_string_lossy());
    }

    if let Some(min) = args.coverage_min {
        runner = runner.env("HOMEBOY_COVERAGE_MIN", &format!("{}", min));
    }

    let output = runner.script_args(&args.args).run()?;

    let status = if output.success { "passed" } else { "failed" };

    // Read coverage results if available
    let coverage = coverage_file
        .as_ref()
        .and_then(|f| parse_coverage_file(f).ok());

    // Clean up coverage temp file
    if let Some(ref f) = coverage_file {
        let _ = std::fs::remove_file(f);
    }

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

    // Coverage hint when not using coverage
    if !coverage_enabled {
        hints.push(format!(
            "Collect coverage: homeboy test {} --coverage",
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
            coverage,
            hints,
        },
        output.exit_code,
    ))
}

/// Parse the coverage JSON file written by the extension test runner.
fn parse_coverage_file(path: &std::path::Path) -> std::result::Result<CoverageOutput, ()> {
    let content = io::read_file(path, "read coverage file").map_err(|_| ())?;
    let data: serde_json::Value = serde_json::from_str(&content).map_err(|_| ())?;

    let totals = data.get("totals").ok_or(())?;
    let lines = totals.get("lines").ok_or(())?;
    let methods = totals.get("methods").ok_or(())?;

    let lines_pct = lines.get("pct").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let lines_total = lines.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
    let lines_covered = lines.get("covered").and_then(|v| v.as_u64()).unwrap_or(0);
    let methods_pct = methods.get("pct").and_then(|v| v.as_f64()).unwrap_or(0.0);

    // Collect files below 50% coverage as "uncovered"
    let uncovered_files = data
        .get("files")
        .and_then(|f| f.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(|f| {
                    let pct = f.get("line_pct").and_then(|v| v.as_f64())?;
                    if pct < 50.0 {
                        Some(UncoveredFile {
                            file: f
                                .get("file")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?")
                                .to_string(),
                            line_pct: pct,
                        })
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(CoverageOutput {
        lines_pct,
        lines_total,
        lines_covered,
        methods_pct,
        uncovered_files,
    })
}
