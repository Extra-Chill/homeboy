use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use homeboy::git::{commit, get_uncommitted_changes, stage_files, tag, CommitOptions};
use homeboy::version::{
    bump_version, increment_version, read_version, run_pre_bump_commands, set_version,
    validate_changelog_for_bump, VersionTargetInfo,
};

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
    /// Show current version (default: homeboy binary)
    Show {
        /// Component ID (optional - shows homeboy binary version when omitted)
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

        /// Skip automatic git tag after bump
        #[arg(long)]
        no_tag: bool,

        /// Component ID
        component_id: Option<String>,

        /// Version bump type (positional: patch, minor, major)
        bump_type: Option<BumpType>,

        /// Version bump type (alternative to positional)
        #[arg(long, value_enum)]
        level: Option<BumpType>,
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
    changelog_path: String,
    changelog_finalized: bool,
    changelog_changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_commit: Option<GitCommitInfo>,
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
            no_tag,
            bump_type,
            level,
            component_id,
        } => {
            let effective_bump = bump_type.or(level).ok_or_else(|| {
                homeboy::error::Error::validation_invalid_argument(
                    "bump_type",
                    "Missing bump type",
                    None,
                    Some(vec![
                        "Use positional: homeboy version bump <component> patch".to_string(),
                        "Or use flag: homeboy version bump <component> --level=patch".to_string(),
                    ]),
                )
            })?;

            // Execute pre-bump commands before clean-tree check
            if let Some(ref id) = component_id {
                if let Ok(component) = homeboy::component::load(id) {
                    if !component.pre_version_bump_commands.is_empty() {
                        run_pre_bump_commands(
                            &component.pre_version_bump_commands,
                            &component.local_path,
                        )?;
                    }
                }
            }

            // Auto-stage uncommitted changelog changes before clean-tree check
            if let Some(ref id) = component_id {
                if let Ok(component) = homeboy::component::load(id) {
                    if let Some(ref changelog_target) = component.changelog_target {
                        let uncommitted = get_uncommitted_changes(&component.local_path)?;
                        if uncommitted.has_changes {
                            // Check if ONLY the changelog file has uncommitted changes (ignore untracked)
                            let all_uncommitted: Vec<&str> = uncommitted
                                .staged
                                .iter()
                                .chain(uncommitted.unstaged.iter())
                                .map(|s| s.as_str())
                                .collect();

                            let only_changelog = !all_uncommitted.is_empty()
                                && all_uncommitted.iter().all(|f| {
                                    *f == changelog_target || f.ends_with(changelog_target)
                                });

                            if only_changelog {
                                eprintln!(
                                    "[version] Auto-staging changelog changes: {}",
                                    changelog_target
                                );
                                stage_files(&component.local_path, &[changelog_target.as_str()])?;
                            }
                        }
                    }
                }
            }

            // Require clean working tree before version bump
            if let Some(ref id) = component_id {
                if let Ok(component) = homeboy::component::load(id) {
                    let uncommitted = get_uncommitted_changes(&component.local_path)?;
                    if uncommitted.has_changes {
                        let mut details = vec![];
                        if !uncommitted.staged.is_empty() {
                            details.push(format!("Staged: {}", uncommitted.staged.join(", ")));
                        }
                        if !uncommitted.unstaged.is_empty() {
                            details.push(format!("Unstaged: {}", uncommitted.unstaged.join(", ")));
                        }
                        if !uncommitted.untracked.is_empty() {
                            details.push(format!("Untracked: {}", uncommitted.untracked.join(", ")));
                        }
                        return Err(homeboy::error::Error::validation_invalid_argument(
                            "workingTree",
                            "Working tree has uncommitted changes",
                            Some(details.join("\n")),
                            Some(vec![
                                "Version bump only commits version targets and changelog - your code changes will be left behind.".to_string(),
                                "1. Document changes: homeboy changelog add <component> -m \"...\" for each change".to_string(),
                                "2. Commit everything: git add -A && git commit -m \"<description>\"".to_string(),
                                "3. Run version bump: homeboy version bump <component> <level>".to_string(),
                            ]),
                        ));
                    }
                }
            }

            if dry_run {
                let info = read_version(component_id.as_deref())?;

                let new_version =
                    increment_version(&info.version, effective_bump.as_str()).ok_or_else(|| {
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

                // Validate changelog without making changes
                let changelog_validation = if let Some(ref id) = component_id {
                    match homeboy::component::load(id) {
                        Ok(component) => {
                            match validate_changelog_for_bump(&component, &info.version, &new_version) {
                                Ok(validation) => validation,
                                Err(e) => {
                                    eprintln!(
                                        "[version] [dry-run] Changelog validation would fail: {}",
                                        e.message
                                    );
                                    return Err(e);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("[version] [dry-run] Could not validate changelog: {}", e);
                            return Err(e);
                        }
                    }
                } else {
                    return Err(homeboy::error::Error::validation_invalid_argument(
                        "componentId",
                        "Missing componentId for changelog validation",
                        None,
                        None,
                    ));
                };

                return Ok((
                    VersionOutput::Bump(VersionBumpOutput {
                        command: "version.bump".to_string(),
                        component_id,
                        old_version: info.version,
                        new_version,
                        targets: info.targets,
                        changelog_path: changelog_validation.changelog_path,
                        changelog_finalized: changelog_validation.changelog_finalized,
                        changelog_changed: changelog_validation.changelog_changed,
                        dry_run: Some(true),
                        git_commit: None,
                    }),
                    0,
                ));
            }

            let result = bump_version(component_id.as_deref(), effective_bump.as_str())?;

            // Auto-commit unless --no-commit
            let git_commit = if no_commit {
                None
            } else {
                create_version_commit(
                    component_id.as_deref(),
                    &result.new_version,
                    &result.targets,
                    &result.changelog_path,
                    !no_tag,
                )
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
    let mut files_to_stage: Vec<String> = targets.iter().map(|t| t.full_path.clone()).collect();

    if !changelog_path.is_empty() {
        files_to_stage.push(changelog_path.to_string());
    }

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
