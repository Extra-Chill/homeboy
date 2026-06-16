use clap::Args;
use serde::Serialize;

use homeboy::core::component;
use homeboy::core::deploy::{self, ReleaseStateStatus};
use homeboy::core::release::{
    self, BatchReleaseResult, ReleaseCommandInput, ReleaseCommandResult, ReleasePipelineOptions,
};
use homeboy::core::scope::{self, Scope};

use super::utils::args::DryRunArgs;
use super::CmdResult;

#[derive(Args)]
pub struct ReleaseArgs {
    /// Component ID(s) to release
    pub components: Vec<String>,

    /// Release all components in a project that need a release
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

    /// Deploy to all projects using this component after release
    #[arg(long)]
    deploy: bool,

    /// Recover from an interrupted release (tag + push current version)
    #[arg(long)]
    recover: bool,

    /// With --recover: if the release tag exists but points at a commit behind
    /// HEAD (e.g. config-only commits landed after tagging), move the tag to
    /// HEAD instead of refusing. Guarded — the tagged commit must be an
    /// ancestor of HEAD, HEAD must satisfy the version targets, and no GitHub
    /// Release may exist for the tag.
    #[arg(long)]
    retag: bool,

    /// Finish the release pipeline for an already-versioned, already-tagged HEAD.
    /// Skips changelog/version/git mutation steps and runs package, GitHub Release,
    /// publish, cleanup, and post-release hooks against the tag pointing at HEAD.
    #[arg(long)]
    head: bool,

    /// Use existing release artifacts from this directory instead of running release.package.
    /// Requires --head.
    #[arg(long, value_name = "DIR")]
    from_artifacts: Option<String>,

    /// Skip pre-release quality checks.
    ///
    /// Bare `--skip-checks` skips ALL quality gates (audit, lint, test).
    /// `--skip-checks=lint` (or `audit`/`test`, comma- or space-separated)
    /// skips only the named checks while leaving working_tree, remote_sync,
    /// and the remaining quality checks active.
    #[arg(long, num_args = 0.., value_name = "CHECK", value_delimiter = ',')]
    skip_checks: Option<Vec<String>>,

    /// Force a specific version bump: major, minor, patch, or an explicit version (e.g. 2.0.0).
    /// Overrides auto-detection from commit history.
    #[arg(long)]
    bump: Option<String>,

    /// Allow an explicit bump lower than Homeboy's commit-derived recommendation.
    #[arg(long)]
    force_lower_bump: bool,

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

impl ReleaseArgs {
    fn pipeline_options(&self) -> ReleasePipelineOptions {
        ReleasePipelineOptions {
            deploy: self.deploy,
            skip_publish: self.skip_publish,
            head: self.head,
            from_artifacts: self.from_artifacts.clone(),
        }
    }

    /// Resolve `--skip-checks` into (skip-all, granular-check-list).
    ///
    /// - Flag absent → `(false, [])`: run every quality gate.
    /// - Bare `--skip-checks` → `(true, [])`: skip all quality gates.
    /// - `--skip-checks=lint` (or `audit`/`test`, repeatable/comma-separated) →
    ///   `(false, ["lint"])`: skip only the named gates.
    ///
    /// Unknown check names are rejected so a typo never silently runs the gate.
    fn resolve_skip_checks(&self) -> homeboy::core::Result<(bool, Vec<String>)> {
        const SKIPPABLE_CHECKS: [&str; 3] = ["audit", "lint", "test"];
        match &self.skip_checks {
            None => Ok((false, Vec::new())),
            Some(values) if values.is_empty() => Ok((true, Vec::new())),
            Some(values) => {
                let mut granular = Vec::new();
                for value in values {
                    let check = value.trim().to_ascii_lowercase();
                    let normalized = if check == "tests" {
                        "test"
                    } else {
                        check.as_str()
                    };
                    if !SKIPPABLE_CHECKS.contains(&normalized) {
                        return Err(homeboy::core::Error::validation_invalid_argument(
                            "skip-checks",
                            format!(
                                "Unknown check '{}' for --skip-checks. Valid checks: {}",
                                value,
                                SKIPPABLE_CHECKS.join(", ")
                            ),
                            Some(value.clone()),
                            Some(vec![
                                "Use --skip-checks (no value) to skip all quality checks"
                                    .to_string(),
                                "Use --skip-checks=lint to skip only the lint gate".to_string(),
                            ]),
                        ));
                    }
                    if !granular.iter().any(|c: &String| c == normalized) {
                        granular.push(normalized.to_string());
                    }
                }
                Ok((false, granular))
            }
        }
    }
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
        head: bool,
        from_artifacts: Option<String>,
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
            deploy,
            recover,
            retag: false,
            head,
            from_artifacts,
            skip_checks: if skip_checks { Some(Vec::new()) } else { None },
            bump,
            force_lower_bump: false,
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
    let component_ids = resolve_component_ids(&args, &args.components)?;
    let bump_override = args.bump.clone();
    let (skip_checks, skip_checks_granular) = args.resolve_skip_checks()?;

    // Single component: use the original single-release flow
    if component_ids.len() == 1 {
        let component_id = &component_ids[0];
        let (result, exit_code) = release::run_command(ReleaseCommandInput {
            component_id: component_id.clone(),
            path_override: args.path.clone(),
            dry_run: args.dry_run_args.dry_run,
            recover: args.recover,
            retag: args.retag,
            skip_checks,
            skip_checks_granular: skip_checks_granular.clone(),
            bump_override: bump_override.clone(),
            force_lower_bump: args.force_lower_bump,
            pipeline: args.pipeline_options(),
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
        return Err(homeboy::core::Error::validation_invalid_argument(
            "path",
            "--path is not supported for batch releases (multiple components)",
            None,
            None,
        ));
    }
    if args.recover {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "recover",
            "--recover is not supported for batch releases — run recovery per-component",
            None,
            None,
        ));
    }
    if args.head {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "head",
            "--head is not supported for batch releases — finish one component release at a time",
            None,
            None,
        ));
    }
    if args.from_artifacts.is_some() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "from-artifacts",
            "--from-artifacts requires --head and is not supported for batch releases",
            args.from_artifacts.clone(),
            None,
        ));
    }

    let input_template = ReleaseCommandInput {
        component_id: String::new(), // overridden per component
        path_override: None,
        dry_run: args.dry_run_args.dry_run,
        recover: false,
        retag: false,
        skip_checks,
        skip_checks_granular,
        bump_override,
        force_lower_bump: args.force_lower_bump,
        pipeline: ReleasePipelineOptions {
            deploy: args.deploy,
            skip_publish: args.skip_publish,
            head: false,
            from_artifacts: None,
        },
        skip_github_release: args.no_github_release,
        git_identity: args.git_identity.clone(),
    };

    let batch_result = release::run_batch(&component_ids, &input_template);
    // A batch that produced zero releases (all components skipped, none failed)
    // exits with the dedicated skip code so the envelope reports success:false —
    // matching single-release behavior (issue #4316). A batch with at least one
    // real release exits 0; any failure exits 1.
    let exit_code = if batch_result.summary.failed > 0 {
        1
    } else if batch_result.summary.released == 0 && batch_result.summary.skipped > 0 {
        release::SKIPPED_RELEASE_EXIT_CODE
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
/// 2. `--project <id>` — all components in the project that need a release
/// 3. Positional component IDs
fn resolve_component_ids(
    args: &ReleaseArgs,
    components: &[String],
) -> homeboy::core::Result<Vec<String>> {
    if let Some(ref project_id) = args.project {
        let components =
            scope::resolve_scope_component_records(&Scope::Project(project_id.into()))?;

        if components.is_empty() {
            return Err(homeboy::core::Error::validation_invalid_argument(
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
                    matches!(status, ReleaseStateStatus::NeedsRelease)
                } else {
                    // Without --outdated: anything that's not clean
                    matches!(
                        status,
                        ReleaseStateStatus::NeedsRelease | ReleaseStateStatus::DocsOnly
                    )
                }
            })
            .map(|c| c.id.clone())
            .collect();

        if releasable.is_empty() {
            let filter_desc = if args.outdated {
                "with unreleased code commits"
            } else {
                "that need a release"
            };
            return Err(homeboy::core::Error::validation_invalid_argument(
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
            Err(_) => Err(homeboy::core::Error::validation_missing_argument(vec![
                "component ID(s), or --project <project-id>".to_string(),
            ])),
        }
    } else {
        Ok(components.to_vec())
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
            None,
            false,
            false,
            None,
        )
    }

    #[test]
    fn final_bump_keyword_stays_component() {
        let release_args = args(&["api", "patch"]);
        let components = resolve_component_ids(&release_args, &release_args.components).unwrap();

        assert_eq!(components, vec!["api", "patch"]);
    }

    #[test]
    fn single_component_named_like_bump_stays_component() {
        let release_args = args(&["patch"]);
        let components = resolve_component_ids(&release_args, &release_args.components).unwrap();

        assert_eq!(components, vec!["patch"]);
    }

    #[test]
    fn canonical_bump_flag_does_not_change_components() {
        let mut release_args = args(&["api"]);
        release_args.bump = Some("minor".to_string());

        let components = resolve_component_ids(&release_args, &release_args.components).unwrap();

        assert_eq!(components, vec!["api"]);
        assert_eq!(release_args.bump.as_deref(), Some("minor"));
    }

    fn skip_args(skip_checks: Option<Vec<&str>>) -> ReleaseArgs {
        ReleaseArgs {
            components: vec!["fixture".to_string()],
            project: None,
            outdated: false,
            path: None,
            dry_run_args: DryRunArgs { dry_run: true },
            deploy: false,
            recover: false,
            retag: false,
            head: false,
            from_artifacts: None,
            skip_checks: skip_checks
                .map(|values| values.iter().map(|value| value.to_string()).collect()),
            bump: None,
            force_lower_bump: false,
            skip_publish: false,
            no_github_release: false,
            git_identity: None,
        }
    }

    #[test]
    fn resolve_skip_checks_absent_runs_all_gates() {
        let args = skip_args(None);
        let (skip_all, granular) = args.resolve_skip_checks().expect("absent is valid");
        assert!(!skip_all);
        assert!(granular.is_empty());
    }

    #[test]
    fn resolve_skip_checks_bare_skips_all() {
        let args = skip_args(Some(Vec::new()));
        let (skip_all, granular) = args.resolve_skip_checks().expect("bare is valid");
        assert!(skip_all);
        assert!(granular.is_empty());
    }

    #[test]
    fn resolve_skip_checks_granular_lint_only() {
        let args = skip_args(Some(vec!["lint"]));
        let (skip_all, granular) = args.resolve_skip_checks().expect("lint is valid");
        assert!(!skip_all);
        assert_eq!(granular, vec!["lint"]);
    }

    #[test]
    fn resolve_skip_checks_unknown_name_is_rejected() {
        let args = skip_args(Some(vec!["bogus"]));
        let err = args
            .resolve_skip_checks()
            .expect_err("unknown check rejected");
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.to_string().contains("Unknown check 'bogus'"));
    }
}
