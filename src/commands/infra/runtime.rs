use clap::{Args, Subcommand};
use serde::Serialize;

use crate::commands::CmdResult;

#[derive(Args)]
pub struct RuntimeArgs {
    #[command(subcommand)]
    command: RuntimeCommand,
}

#[derive(Subcommand)]
enum RuntimeCommand {
    /// Inspect core-bundled runtime helper paths exposed to extension runners.
    Helper {
        #[command(subcommand)]
        command: RuntimeHelperCommand,
    },
    /// Refresh a shared runtime package from a source repository or directory.
    Refresh {
        /// Runtime package ID to materialize.
        runtime_id: String,

        /// Git URL, repo root, or runtime package directory to install from.
        #[arg(long)]
        source: String,

        /// Git ref to check out for URL sources (branch, tag, or commit).
        #[arg(long = "ref")]
        revision: Option<String>,
    },
}

#[derive(Subcommand)]
enum RuntimeHelperCommand {
    /// Print the materialized path for a known core runtime helper.
    Path {
        /// Print only the path, for shell bootstrap usage.
        #[arg(long)]
        plain: bool,

        /// Known helper filename or injected HOMEBOY_RUNTIME_* env var name.
        helper: String,
    },
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum RuntimeOutput {
    HelperPath(RuntimeHelperPathOutput),
    RuntimePackageRefresh(RuntimePackageRefreshOutput),
}

#[derive(Serialize)]
pub struct RuntimeHelperPathOutput {
    command: String,
    helper: String,
    path: String,
}

#[derive(Serialize)]
pub struct RuntimePackageRefreshOutput {
    command: String,
    runtime_id: String,
    source: String,
    path: String,
    manifest_path: String,
    source_revision: Option<String>,
    replaced_existing: bool,
}

pub fn run(args: RuntimeArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<RuntimeOutput> {
    match args.command {
        RuntimeCommand::Helper { command } => match command {
            RuntimeHelperCommand::Path { helper, .. } => helper_path(&helper),
        },
        RuntimeCommand::Refresh {
            runtime_id,
            source,
            revision,
        } => refresh_runtime_package(&runtime_id, &source, revision.as_deref()),
    }
}

pub fn is_plain_mode(args: &RuntimeArgs) -> bool {
    match &args.command {
        RuntimeCommand::Helper { command } => match command {
            RuntimeHelperCommand::Path { plain, .. } => *plain,
        },
        RuntimeCommand::Refresh { .. } => false,
    }
}

impl RuntimeArgs {
    pub(crate) fn is_refresh_command(&self) -> bool {
        matches!(self.command, RuntimeCommand::Refresh { .. })
    }
}

pub fn run_plain_text(args: RuntimeArgs) -> homeboy::core::Result<(String, i32)> {
    match args.command {
        RuntimeCommand::Helper { command } => match command {
            RuntimeHelperCommand::Path { helper, .. } => {
                let path = homeboy::core::extension::helper_path(&helper)?;
                Ok((format!("{}\n", path.to_string_lossy()), 0))
            }
        },
        RuntimeCommand::Refresh { .. } => unreachable!("runtime refresh has no plain mode"),
    }
}

fn helper_path(helper: &str) -> CmdResult<RuntimeOutput> {
    let path = homeboy::core::extension::helper_path(helper)?;

    Ok((
        RuntimeOutput::HelperPath(RuntimeHelperPathOutput {
            command: "runtime.helper.path".to_string(),
            helper: helper.to_string(),
            path: path.to_string_lossy().to_string(),
        }),
        0,
    ))
}

fn refresh_runtime_package(
    runtime_id: &str,
    source: &str,
    revision: Option<&str>,
) -> CmdResult<RuntimeOutput> {
    let result = homeboy::core::runtime_package::refresh(runtime_id, source, revision)?;

    Ok((
        RuntimeOutput::RuntimePackageRefresh(RuntimePackageRefreshOutput {
            command: "runtime.refresh".to_string(),
            runtime_id: result.runtime_id,
            source: result.source,
            path: result.path.to_string_lossy().to_string(),
            manifest_path: result.manifest_path.to_string_lossy().to_string(),
            source_revision: result.source_revision,
            replaced_existing: result.replaced_existing,
        }),
        0,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_path_resolves_core_helper() {
        crate::test_support::with_isolated_home(|_| {
            let (output, exit_code) = helper_path("command-capture.sh").unwrap();

            assert_eq!(exit_code, 0);
            let RuntimeOutput::HelperPath(output) = output else {
                panic!("expected helper path output");
            };
            assert!(output.path.ends_with("command-capture.sh"));
            assert!(std::path::Path::new(&output.path).is_file());
        });
    }

    #[test]
    fn helper_path_plain_prints_only_path() {
        crate::test_support::with_isolated_home(|_| {
            let args = RuntimeArgs {
                command: RuntimeCommand::Helper {
                    command: RuntimeHelperCommand::Path {
                        plain: true,
                        helper: "runner-prelude.sh".to_string(),
                    },
                },
            };

            let (output, exit_code) = run_plain_text(args).unwrap();

            assert_eq!(exit_code, 0);
            assert!(output.ends_with("runner-prelude.sh\n"));
            assert!(std::path::Path::new(output.trim()).is_file());
        });
    }

    #[test]
    fn refresh_runtime_package_reports_materialized_path() {
        crate::test_support::with_isolated_home(|_| {
            let source = tempfile::TempDir::new().expect("source tempdir");
            let package = source.path().join("agent-runtimes/neutral-runtime");
            std::fs::create_dir_all(&package).expect("runtime package dir");
            std::fs::write(
                package.join("neutral-runtime.json"),
                r#"{
  "schema": "homeboy/agent-runtime-manifest/v1",
  "id": "neutral-runtime"
}"#,
            )
            .expect("runtime package manifest");

            let (output, exit_code) =
                refresh_runtime_package("neutral-runtime", &source.path().to_string_lossy(), None)
                    .expect("refresh runtime package");

            assert_eq!(exit_code, 0);
            let RuntimeOutput::RuntimePackageRefresh(output) = output else {
                panic!("expected runtime package refresh output");
            };
            assert_eq!(output.runtime_id, "neutral-runtime");
            assert!(output.path.ends_with("agent-runtimes/neutral-runtime"));
            assert!(std::path::Path::new(&output.manifest_path).is_file());
        });
    }
}
