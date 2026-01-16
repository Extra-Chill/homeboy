use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use homeboy::git::{commit, CommitOptions};
use homeboy::version::{bump_version, increment_version, read_version, set_version, VersionTargetInfo};

use super::CmdResult;

#[derive(Serialize)]
pub struct GitCommitInfo {
    pub success: bool,
    pub message: String,
    pub files_staged: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum VersionOutput {
    Show(VersionShowOutput),
    Bump(VersionBumpOutput),
    Set(VersionSetOutput),
}

#[derive(Args)]
pub struct VersionArgs {
    #[command(subcommand)]
    command: VersionCommand,
}

#[derive(Subcommand)]
enum VersionCommand {
    /// Show current version of a component
    Show {
        /// Component ID
        component_id: Option<String>,
    },
    /// Bump version of a component and finalize changelog
    Bump {
        /// Simulate the bump without making any changes
        #[arg(long)]
        dry_run: bool,

        /// Skip automatic git commit after bump
        #[arg(long)]
        no_commit: bool,

        /// Component ID
        component_id: Option<String>,

        /// Version bump type (patch, minor, major)
        #[arg(last = true)]
        bump_type: BumpType,
    },
    /// Set version directly (without incrementing or changelog finalization)
    #[command(visible_aliases = ["edit", "merge"])]
    Set {
        /// Component ID
        component_id: Option<String>,

        /// New version (e.g., 1.2.3)
        new_version: String,
    },
}

#[derive(Clone, ValueEnum)]
enum BumpType {
    Patch,
    Minor,
    Major,
}

impl BumpType {
    fn as_str(&self) -> &'static str {
        match self {
            BumpType::Patch => "patch",
            BumpType::Minor => "minor",
            BumpType::Major => "major",
        }
    }
}

#[derive(Serialize)]

pub struct VersionShowOutput {
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    component_id: Option<String>,
    pub version: String,
    targets: Vec<VersionTargetInfo>,
}

#[derive(Serialize)]

pub struct VersionBumpOutput {
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    component_id: Option<String>,
    old_version: String,
    new_version: String,
    targets: Vec<VersionTargetInfo>,
    changelog_path: String,
    changelog_finalized: bool,
    changelog_changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    dry_run: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_commit: Option<GitCommitInfo>,
}

#[derive(Serialize)]

pub struct VersionSetOutput {
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    component_id: Option<String>,
    old_version: String,
    new_version: String,
    targets: Vec<VersionTargetInfo>,
}

pub fn run(args: VersionArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<VersionOutput> {
    match args.command {
        VersionCommand::Show { component_id } => {
            let info = read_version(component_id.as_deref())?;

            Ok((
                VersionOutput::Show(VersionShowOutput {
                    command: "version.show".to_string(),
                    component_id,
                    version: info.version,
                    targets: info.targets,
                }),
                0,
            ))
        }
        VersionCommand::Bump {
            dry_run,
            no_commit,
            bump_type,
            component_id,
        } => {
            if dry_run {
                let info = read_version(component_id.as_deref())?;

                let new_version =
                    increment_version(&info.version, bump_type.as_str()).ok_or_else(|| {
                        homeboy::error::Error::validation_invalid_argument(
                            "version",
                            format!("Invalid version format: {}", info.version),
                            None,
                            Some(vec![info.version.clone()]),
                        )
                    })?;

                eprintln!(
                    "[version] [dry-run] Would bump {} -> {}",
                    info.version, new_version
                );

                return Ok((
                    VersionOutput::Bump(VersionBumpOutput {
                        command: "version.bump".to_string(),
                        component_id,
                        old_version: info.version,
                        new_version,
                        targets: info.targets,
                        changelog_path: String::new(),
                        changelog_finalized: false,
                        changelog_changed: false,
                        dry_run: Some(true),
                        git_commit: None,
                    }),
                    0,
                ));
            }

            let result = bump_version(component_id.as_deref(), bump_type.as_str())?;

            // Auto-commit unless --no-commit
            let git_commit = if no_commit {
                None
            } else {
                // Collect files to stage: version targets + changelog
                let mut files_to_stage: Vec<String> = result
                    .targets
                    .iter()
                    .map(|t| t.full_path.clone())
                    .collect();

                if !result.changelog_path.is_empty() {
                    files_to_stage.push(result.changelog_path.clone());
                }

                let commit_message = format!("release: v{}", result.new_version);

                let options = CommitOptions {
                    staged_only: false,
                    files: Some(files_to_stage.clone()),
                    exclude: None,
                };

                // Attempt commit - graceful failure (version files already updated)
                match commit(component_id.as_deref(), Some(&commit_message), options) {
                    Ok(output) => {
                        let stdout = if output.stdout.is_empty() {
                            None
                        } else {
                            Some(output.stdout)
                        };
                        let stderr = if output.stderr.is_empty() {
                            None
                        } else {
                            Some(output.stderr)
                        };
                        Some(GitCommitInfo {
                            success: output.success,
                            message: commit_message,
                            files_staged: files_to_stage,
                            stdout,
                            stderr,
                        })
                    }
                    Err(e) => {
                        // Report failure but don't rollback version changes
                        Some(GitCommitInfo {
                            success: false,
                            message: commit_message,
                            files_staged: files_to_stage,
                            stdout: None,
                            stderr: Some(e.to_string()),
                        })
                    }
                }
            };

            Ok((
                VersionOutput::Bump(VersionBumpOutput {
                    command: "version.bump".to_string(),
                    component_id,
                    old_version: result.old_version,
                    new_version: result.new_version,
                    targets: result.targets,
                    changelog_path: result.changelog_path,
                    changelog_finalized: result.changelog_finalized,
                    changelog_changed: result.changelog_changed,
                    dry_run: Some(false),
                    git_commit,
                }),
                0,
            ))
        }
        VersionCommand::Set {
            component_id,
            new_version,
        } => {
            // Core validates componentId
            let result = set_version(component_id.as_deref(), &new_version)?;

            Ok((
                VersionOutput::Set(VersionSetOutput {
                    command: "version.set".to_string(),
                    component_id,
                    old_version: result.old_version,
                    new_version: result.new_version,
                    targets: result.targets,
                }),
                0,
            ))
        }
    }
}

pub fn show_version_output(component_id: &str) -> homeboy::Result<(VersionShowOutput, i32)> {
    let info = read_version(Some(component_id))?;

    Ok((
        VersionShowOutput {
            command: "version.show".to_string(),
            component_id: Some(component_id.to_string()),
            version: info.version,
            targets: info.targets,
        },
        0,
    ))
}
