use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::git::{commit, tag, CommitOptions};
use homeboy::release::{self, ReleasePlan, ReleaseRun};
use homeboy::version::{read_version, set_version, VersionTargetInfo};

use super::release::BumpType;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_created: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_name: Option<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum VersionOutput {
    Show(VersionShowOutput),
    Set(VersionSetOutput),
    Bump(VersionBumpOutput),
}

#[derive(Args)]
pub struct VersionArgs {
    #[command(subcommand)]
    command: VersionCommand,
}

#[derive(Subcommand)]
enum VersionCommand {
    /// Show current version (default: homeboy binary)
    Show {
        /// Component ID (optional - shows homeboy binary version when omitted)
        component_id: Option<String>,
    },
    /// Set version directly (without incrementing or changelog finalization)
    #[command(visible_aliases = ["edit", "merge"])]
    Set {
        /// Component ID
        component_id: Option<String>,

        /// New version (e.g., 1.2.3)
        new_version: String,
    },
    /// Bump version with semantic versioning (alias for `release`)
    Bump {
        /// Component ID
        component_id: String,

        /// Version bump type (patch, minor, major)
        bump_type: BumpType,

        /// Preview what will happen without making changes
        #[arg(long)]
        dry_run: bool,
    },
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

pub struct VersionSetOutput {
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
    git_commit: Option<GitCommitInfo>,
}

#[derive(Serialize)]
pub struct VersionBumpOutput {
    command: String,
    component_id: String,
    bump_type: String,
    dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<ReleasePlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run: Option<ReleaseRun>,
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
        VersionCommand::Set {
            component_id,
            new_version,
        } => {
            // Core validates componentId
            let result = set_version(component_id.as_deref(), &new_version)?;

            // Auto-commit version and changelog changes
            let git_commit = create_version_commit(
                component_id.as_deref(),
                &result.new_version,
                &result.targets,
                &result.changelog_path,
                true,
            );

            Ok((
                VersionOutput::Set(VersionSetOutput {
                    command: "version.set".to_string(),
                    component_id,
                    old_version: result.old_version,
                    new_version: result.new_version,
                    targets: result.targets,
                    changelog_path: result.changelog_path,
                    changelog_finalized: result.changelog_finalized,
                    changelog_changed: result.changelog_changed,
                    git_commit,
                }),
                0,
            ))
        }
        VersionCommand::Bump {
            component_id,
            bump_type,
            dry_run,
        } => {
            let options = release::ReleaseOptions {
                bump_type: bump_type.as_str().to_string(),
                dry_run,
            };

            if dry_run {
                let plan = release::plan(&component_id, &options)?;
                Ok((
                    VersionOutput::Bump(VersionBumpOutput {
                        command: "version.bump".to_string(),
                        component_id,
                        bump_type: options.bump_type,
                        dry_run: true,
                        plan: Some(plan),
                        run: None,
                    }),
                    0,
                ))
            } else {
                let run_result = release::run(&component_id, &options)?;
                Ok((
                    VersionOutput::Bump(VersionBumpOutput {
                        command: "version.bump".to_string(),
                        component_id,
                        bump_type: options.bump_type,
                        dry_run: false,
                        plan: None,
                        run: Some(run_result),
                    }),
                    0,
                ))
            }
        }
    }
}

/// Creates a git commit and optionally a tag for version changes.
fn create_version_commit(
    component_id: Option<&str>,
    new_version: &str,
    targets: &[VersionTargetInfo],
    changelog_path: &str,
    create_tag: bool,
) -> Option<GitCommitInfo> {
    // Get component's local_path and git repo root for path relativization
    let local_path = component_id
        .and_then(|id| homeboy::component::load(id).ok())
        .map(|c| c.local_path)
        .unwrap_or_default();

    // Use git repo root for path relativization (handles components in subdirectories)
    let repo_root = homeboy::git::get_git_root(&local_path).unwrap_or(local_path.clone());

    // Convert absolute paths to relative paths (relative to git repo root) for staging
    let files_to_stage: Vec<String> = targets
        .iter()
        .map(|t| {
            std::path::Path::new(&t.full_path)
                .strip_prefix(&repo_root)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| t.full_path.clone())
        })
        .chain(
            // Also relativize changelog path if non-empty
            (!changelog_path.is_empty())
                .then(|| {
                    std::path::Path::new(changelog_path)
                        .strip_prefix(&repo_root)
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| changelog_path.to_string())
                })
                .into_iter(),
        )
        .collect();

    let commit_message = format!("release: v{}", new_version);

    let options = CommitOptions {
        staged_only: false,
        files: Some(files_to_stage.clone()),
        exclude: None,
        amend: false,
    };

    match commit(component_id, Some(&commit_message), options) {
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

            // Create tag after successful commit
            let (tag_created, tag_name) = if create_tag && output.success {
                let tag_name = format!("v{}", new_version);
                let tag_message = format!("Release {}", tag_name);
                match tag(component_id, Some(&tag_name), Some(&tag_message)) {
                    Ok(tag_output) => {
                        if tag_output.success {
                            eprintln!(
                                "[version] Tagged {}. For automated packaging/publishing, configure a release pipeline: homeboy docs release",
                                tag_name
                            );
                        }
                        (Some(tag_output.success), Some(tag_name))
                    }
                    Err(_) => (Some(false), Some(tag_name)),
                }
            } else {
                (None, None)
            };

            Some(GitCommitInfo {
                success: output.success,
                message: commit_message,
                files_staged: files_to_stage,
                stdout,
                stderr,
                tag_created,
                tag_name,
            })
        }
        Err(e) => Some(GitCommitInfo {
            success: false,
            message: commit_message.clone(),
            files_staged: files_to_stage.clone(),
            stdout: None,
            stderr: Some(format!(
                "Commit failed: {}. Version files modified but not committed: {}. Recovery: git add -A && git commit -m \"{}\" OR git checkout -- {}",
                e,
                files_to_stage.join(", "),
                commit_message,
                files_to_stage.join(" ")
            )),
            tag_created: None,
            tag_name: None,
        }),
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
