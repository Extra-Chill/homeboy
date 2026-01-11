use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use homeboy_core::doctor::{Doctor, DoctorScope, FailOn};

#[derive(Args)]
pub struct DoctorArgs {
    #[command(subcommand)]
    command: DoctorCommand,
}

#[derive(Subcommand)]
enum DoctorCommand {
    /// Scan Homeboy configuration and report issues
    Scan(DoctorScanArgs),

    /// Safely remove unknown top-level keys from config JSON
    Cleanup(DoctorCleanupArgs),
}

#[derive(Args)]
pub struct DoctorScanArgs {
    /// Scope of configuration to scan
    #[arg(long, value_enum, default_value_t = ScopeArg::All)]
    pub scope: ScopeArg,

    /// Scan a specific JSON file path instead of a scope
    #[arg(long)]
    pub file: Option<String>,

    /// Fail with non-zero exit if warnings are found
    #[arg(long, value_enum, default_value_t = FailOnArg::Error)]
    pub fail_on: FailOnArg,
}

#[derive(Args)]
pub struct DoctorCleanupArgs {
    /// Scope of configuration to clean up
    #[arg(long, value_enum, default_value_t = ScopeArg::All)]
    pub scope: ScopeArg,

    /// Clean up a specific JSON file path instead of a scope
    #[arg(long)]
    pub file: Option<String>,

    /// Preview changes without writing files
    #[arg(long)]
    pub dry_run: bool,

    /// Fail with non-zero exit if warnings are found after cleanup
    #[arg(long, value_enum, default_value_t = FailOnArg::Error)]
    pub fail_on: FailOnArg,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum ScopeArg {
    All,
    App,
    Projects,
    Servers,
    Components,
    Modules,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum FailOnArg {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorScanOutput {
    #[serde(flatten)]
    pub report: homeboy_core::doctor::DoctorReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCleanupOutput {
    pub cleanup: homeboy_core::doctor::DoctorCleanupReport,
    pub scan: homeboy_core::doctor::DoctorReport,
}

pub fn run(
    args: DoctorArgs,
    _global: &crate::commands::GlobalArgs,
) -> homeboy_core::Result<(serde_json::Value, i32)> {
    match args.command {
        DoctorCommand::Scan(args) => {
            let scan_result = if let Some(path) = args.file.as_deref() {
                Doctor::scan_file(std::path::Path::new(path))?
            } else {
                Doctor::scan(scope_to_core(args.scope))?
            };

            let exit_code = Doctor::exit_code(&scan_result, fail_to_core(args.fail_on));

            let output = DoctorScanOutput {
                report: scan_result.report,
            };

            let value = serde_json::to_value(output)
                .map_err(|e| homeboy_core::Error::internal_json(e.to_string(), None))?;

            Ok((value, exit_code))
        }
        DoctorCommand::Cleanup(args) => {
            let cleanup_and_scan = if let Some(path) = args.file.as_deref() {
                Doctor::cleanup_file(std::path::Path::new(path), args.dry_run)?
            } else {
                Doctor::cleanup(scope_to_core(args.scope), args.dry_run)?
            };

            let exit_code =
                Doctor::exit_code_from_report(&cleanup_and_scan.scan, fail_to_core(args.fail_on));

            let output = DoctorCleanupOutput {
                cleanup: cleanup_and_scan.cleanup,
                scan: cleanup_and_scan.scan,
            };

            let value = serde_json::to_value(output)
                .map_err(|e| homeboy_core::Error::internal_json(e.to_string(), None))?;

            Ok((value, exit_code))
        }
    }
}

fn scope_to_core(scope: ScopeArg) -> DoctorScope {
    match scope {
        ScopeArg::All => DoctorScope::All,
        ScopeArg::App => DoctorScope::App,
        ScopeArg::Projects => DoctorScope::Projects,
        ScopeArg::Servers => DoctorScope::Servers,
        ScopeArg::Components => DoctorScope::Components,
        ScopeArg::Modules => DoctorScope::Modules,
    }
}

fn fail_to_core(fail_on: FailOnArg) -> FailOn {
    match fail_on {
        FailOnArg::Error => FailOn::Error,
        FailOnArg::Warning => FailOn::Warning,
    }
}
