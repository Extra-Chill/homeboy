use clap::Args;
use serde::Serialize;

use homeboy::component;
use homeboy::deploy::{self, ReleaseStateStatus};
use homeboy::project;
use homeboy::release::{self, BatchReleaseResult, ReleaseCommandInput, ReleaseCommandResult};

use super::utils::args::{DryRunArgs, HiddenJsonArgs};
use super::CmdResult;

#[derive(Args)]
pub struct ReleaseArgs {
    /// Component ID(s) to release
    pub components: Vec<String>,

    /// Release all components in a project that need a version bump
    #[arg(long, short = 'p')]
    pub project: Option<String>,

    /// Only release components with unreleased code commits (use with --project)
    #[arg(long)]
    pub outdated: bool,

    /// Override local_path for version file lookup (single component only)
    #[arg(long)]
    pub path: Option<String>,

    #[command(flatten)]
    dry_run_args: DryRunArgs,

    #[command(flatten)]
    _json: HiddenJsonArgs,

    /// Deploy to all projects using this component after release
    #[arg(long)]
    deploy: bool,

    /// Recover from an interrupted release (tag + push current version)
    #[arg(long)]
    recover: bool,

    /// Skip pre-release lint and test checks
    #[arg(long)]
    skip_checks: bool,

    /// Force a specific version bump: major, minor, patch, or an explicit version (e.g. 2.0.0).
    /// Overrides auto-detection from commit history.
    #[arg(long)]
    bump: Option<String>,

    /// Skip publish/package steps (version bump + tag + push only).
    /// Use when CI handles publishing after the tag is pushed.
    #[arg(long)]
    skip_publish: bool,

    /// Skip the GitHub Release creation step (tag + notes on github.com).
    /// Use when CI or another pipeline already creates GitHub Releases.
    #[arg(long)]
    no_github_release: bool,

    /// Git identity for release commits and tags.
    /// Use "bot" for the default CI bot identity, or "Name <email>" for custom.
    /// When set, configures git user.name and user.email before committing.
    #[arg(long)]
    git_identity: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "command", rename = "release")]
pub struct ReleaseOutput {
    pub result: ReleaseCommandResult,
}

#[derive(Serialize)]
#[serde(tag = "command", rename = "release.batch")]
pub struct BatchReleaseOutput {
    pub result: BatchReleaseResult,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum ReleaseCommandOutput {
    Single(ReleaseOutput),
    Batch(BatchReleaseOutput),
}

#[cfg(test)]
impl ReleaseArgs {
    /// Construct ReleaseArgs programmatically for tests and internal callers.
    fn from_parts(
        components: Vec<String>,
        project: Option<String>,
        outdated: bool,
        path: Option<String>,
        dry_run: bool,
        deploy: bool,
        recover: bool,
        skip_checks: bool,
        skip_publish: bool,
        bump: Option<String>,
    ) -> Self {
        Self {
            components,
            project,
            outdated,
            path,
            dry_run_args: DryRunArgs { dry_run },
            _json: HiddenJsonArgs::default(),
            deploy,
            recover,
            skip_checks,
            bump,
            skip_publish,
            no_github_release: false,
            git_identity: None,
        }
    }
}

pub fn run(
    args: ReleaseArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<ReleaseCommandOutput> {
    let positional = resolve_positional_args(&args)?;
    let component_ids = resolve_component_ids(&args, &positional.components)?;

    let bump_override = resolve_bump_override(&args, positional.bump);

    // Single component: use the original single-release flow
    if component_ids.len() == 1 {
        let component_id = &component_ids[0];
        let (result, exit_code) = release::run_command(ReleaseCommandInput {
            component_id: component_id.clone(),
            path_override: args.path.clone(),
            dry_run: args.dry_run_args.dry_run,
            deploy: args.deploy,
            recover: args.recover,
            skip_checks: args.skip_checks,
            bump_override: bump_override.clone(),
            skip_publish: args.skip_publish,
            skip_github_release: args.no_github_release,
            git_identity: args.git_identity.clone(),
        })?;

        return Ok((
            ReleaseCommandOutput::Single(ReleaseOutput { result }),
            exit_code,
        ));
    }

    // Multiple components: batch release
    if args.path.is_some() {
        return Err(homeboy::Error::validation_invalid_argument(
            "path",
            "--path is not supported for batch releases (multiple components)",
            None,
            None,
        ));
    }
    if args.recover {
        return Err(homeboy::Error::validation_invalid_argument(
            "recover",
            "--recover is not supported for batch releases — run recovery per-component",
            None,
            None,
        ));
    }

    let input_template = ReleaseCommandInput {
        component_id: String::new(), // overridden per component
        path_override: None,
        dry_run: args.dry_run_args.dry_run,
        deploy: args.deploy,
        recover: false,
        skip_checks: args.skip_checks,
        bump_override,
        skip_publish: args.skip_publish,
        skip_github_release: args.no_github_release,
        git_identity: args.git_identity.clone(),
    };

    let batch_result = release::run_batch(&component_ids, &input_template);
    let exit_code = if batch_result.summary.failed > 0 {
        1
    } else {
        0
    };

    Ok((
        ReleaseCommandOutput::Batch(BatchReleaseOutput {
            result: batch_result,
        }),
        exit_code,
    ))
}

/// Resolve which components to release from CLI arguments.
///
/// Priority:
/// 1. `--project <id>` + `--outdated` — components with unreleased code commits
/// 2. `--project <id>` — all components in the project that need a bump
/// 3. Positional component IDs
fn resolve_component_ids(
    args: &ReleaseArgs,
    components: &[String],
) -> homeboy::Result<Vec<String>> {
    if let Some(ref project_id) = args.project {
        let proj = project::load(project_id)?;
        let components = project::resolve_project_components(&proj)?;

        if components.is_empty() {
            return Err(homeboy::Error::validation_invalid_argument(
                "project",
                format!("Project '{}' has no components attached", project_id),
                Some(project_id.to_string()),
                None,
            ));
        }

        // Filter to components that need releasing
        let releasable: Vec<String> = components
            .iter()
            .filter(|c| {
                let state = deploy::calculate_release_state(c);
                let status = state
                    .as_ref()
                    .map(|s| s.status())
                    .unwrap_or(ReleaseStateStatus::Unknown);

                if args.outdated {
                    // --outdated: only components with unreleased code commits
                    matches!(status, ReleaseStateStatus::NeedsBump)
                } else {
                    // Without --outdated: anything that's not clean
                    matches!(
                        status,
                        ReleaseStateStatus::NeedsBump | ReleaseStateStatus::DocsOnly
                    )
                }
            })
            .map(|c| c.id.clone())
            .collect();

        if releasable.is_empty() {
            let filter_desc = if args.outdated {
                "with unreleased code commits"
            } else {
                "that need a version bump"
            };
            return Err(homeboy::Error::validation_invalid_argument(
                "project",
                format!("No components {} in project '{}'", filter_desc, project_id),
                Some(project_id.to_string()),
                Some(vec![format!("Check with: homeboy status {}", project_id)]),
            ));
        }

        homeboy::log_status!(
            "release",
            "Resolved {} component(s) from project '{}': {}",
            releasable.len(),
            project_id,
            releasable.join(", ")
        );

        return Ok(releasable);
    }

    // Positional component IDs
    if components.is_empty() {
        // Try CWD-based component detection
        match component::resolve_effective(None, None, None) {
            Ok(comp) => Ok(vec![comp.id]),
            Err(_) => Err(homeboy::Error::validation_missing_argument(vec![
                "component ID(s), or --project <project-id>".to_string(),
            ])),
        }
    } else {
        Ok(components.to_vec())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PositionalReleaseArgs {
    components: Vec<String>,
    bump: Option<String>,
}

fn resolve_positional_args(args: &ReleaseArgs) -> homeboy::Result<PositionalReleaseArgs> {
    let mut components = args.components.clone();

    if args.project.is_some() || components.len() < 2 {
        return Ok(PositionalReleaseArgs {
            components,
            bump: None,
        });
    }

    let maybe_bump = components.last().map(|value| value.to_lowercase());
    let Some(bump) = maybe_bump.filter(|value| is_bump_keyword(value)) else {
        return Ok(PositionalReleaseArgs {
            components,
            bump: None,
        });
    };

    if args.bump.is_some() {
        return Err(homeboy::Error::validation_invalid_argument(
            "bump",
            "Use either a positional bump type or --bump, not both",
            Some(bump),
            Some(vec![
                "Example: homeboy release my-component patch".to_string()
            ]),
        ));
    }

    components.pop();
    Ok(PositionalReleaseArgs {
        components,
        bump: Some(bump),
    })
}

fn is_bump_keyword(value: &str) -> bool {
    matches!(value, "major" | "minor" | "patch")
}

fn resolve_bump_override(args: &ReleaseArgs, positional_bump: Option<String>) -> Option<String> {
    if let Some(ref bump) = args.bump {
        Some(bump.clone())
    } else {
        positional_bump
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(components: &[&str]) -> ReleaseArgs {
        ReleaseArgs::from_parts(
            components.iter().map(|value| value.to_string()).collect(),
            None,
            false,
            None,
            true,
            false,
            false,
            false,
            false,
            None,
        )
    }

    #[test]
    fn positional_bump_is_not_treated_as_component() {
        let parsed = resolve_positional_args(&args(&["api", "patch"])).unwrap();

        assert_eq!(parsed.components, vec!["api"]);
        assert_eq!(parsed.bump.as_deref(), Some("patch"));
    }

    #[test]
    fn positional_bump_applies_to_batch_components() {
        let parsed = resolve_positional_args(&args(&["api", "web", "minor"])).unwrap();

        assert_eq!(parsed.components, vec!["api", "web"]);
        assert_eq!(parsed.bump.as_deref(), Some("minor"));
    }

    #[test]
    fn single_component_named_like_bump_stays_component() {
        let parsed = resolve_positional_args(&args(&["patch"])).unwrap();

        assert_eq!(parsed.components, vec!["patch"]);
        assert_eq!(parsed.bump, None);
    }

    #[test]
    fn positional_bump_conflicts_with_explicit_bump() {
        let mut release_args = args(&["api", "major"]);
        release_args.bump = Some("minor".to_string());

        let err = resolve_positional_args(&release_args).unwrap_err();
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("positional bump"));
    }

    #[test]
    fn positional_bump_flows_into_bump_override() {
        let release_args = args(&["api", "patch"]);
        let positional = resolve_positional_args(&release_args).unwrap();

        assert_eq!(
            resolve_bump_override(&release_args, positional.bump).as_deref(),
            Some("patch")
        );
    }
}
