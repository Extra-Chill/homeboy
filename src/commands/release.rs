use clap::Args;
use serde::Serialize;

use homeboy::core::component;
use homeboy::core::deploy::{self, ReleaseStateStatus};
use homeboy::core::release::{
    self, BatchReleaseResult, ReleaseCommandInput, ReleaseCommandResult, ReleaseExecutionPlan,
    ReleasePackageResult, ReleasePhase, ReleasePipelineOptions,
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

    /// Confirm risky release execution modes.
    #[arg(long)]
    apply: bool,

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

    /// Regenerate only the release package for an existing tag at HEAD.
    /// Writes copied artifacts and manifest under Homeboy's artifact root.
    #[arg(long)]
    package_only: bool,

    /// Existing release tag to package with --package-only.
    #[arg(long, value_name = "TAG")]
    tag: Option<String>,

    /// Skip pre-release quality checks.
    ///
    /// Bare `--skip-checks` skips ALL quality gates (audit, lint, test).
    /// `--skip-checks=lint` (or `audit`/`test`, comma- or space-separated)
    /// skips only the named checks while leaving working_tree, remote_sync,
    /// and the remaining quality checks active.
    #[arg(long, num_args = 0.., value_name = "CHECK", value_delimiter = ',')]
    skip_checks: Option<Vec<String>>,

    /// Bypass the package/build-structure validation while still running the build.
    ///
    /// `--skip-checks` only covers audit/lint/test; it does NOT cover the
    /// build-structure validation that the packaging extension runs inside the
    /// `preflight.package`/`package` steps. Use this flag when an operator knows
    /// a build-structure assertion is a false positive and wants to ship anyway.
    /// A build that fails to produce an artifact still blocks the release —
    /// only structure assertions are bypassed (issue #5425).
    #[arg(long)]
    skip_build_validation: bool,

    /// Force a specific version bump: major, minor, patch, or an explicit version (e.g. 2.0.0).
    /// Overrides auto-detection from commit history.
    #[arg(long)]
    bump: Option<String>,

    /// Allow an explicit bump lower than Homeboy's commit-derived recommendation.
    #[arg(long)]
    force_lower_bump: bool,

    /// Skip registry/package publishing only (version bump + tag + push).
    /// This does NOT skip GitHub Release creation — a GitHub Release is still
    /// created unless you ALSO pass --no-github-release. Use when CI handles
    /// registry/package publishing after the tag is pushed.
    #[arg(long)]
    skip_publish: bool,

    /// Skip the GitHub Release creation step (the reviewer-facing release page
    /// with notes + assets on github.com). The tag is still created and pushed.
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
    pub variant: &'static str,
    pub result: ReleaseCommandResult,
}

#[derive(Serialize)]
#[serde(tag = "command", rename = "release.batch")]
pub struct BatchReleaseOutput {
    pub variant: &'static str,
    pub result: BatchReleaseResult,
}

#[derive(Serialize)]
#[serde(tag = "command", rename = "release.package")]
pub struct ReleasePackageOutput {
    pub variant: &'static str,
    pub result: ReleasePackageResult,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum ReleaseCommandOutput {
    Single(ReleaseOutput),
    Batch(BatchReleaseOutput),
    Package(ReleasePackageOutput),
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

    fn execution_plan(&self, skip_checks: bool) -> ReleaseExecutionPlan {
        let phase = if self.recover {
            ReleasePhase::Recover
        } else if self.dry_run_args.dry_run {
            ReleasePhase::Plan
        } else if self.deploy {
            ReleasePhase::Deploy
        } else if self.skip_publish {
            ReleasePhase::Prepare
        } else {
            ReleasePhase::Publish
        };

        let apply_risks = [
            (self.deploy, "--deploy"),
            (self.recover, "--recover"),
            (self.retag, "--retag"),
            (self.head, "--head"),
            (self.package_only, "--package-only"),
            (skip_checks, "bare --skip-checks"),
        ]
        .into_iter()
        .filter_map(|(enabled, flag)| enabled.then_some(flag))
        .collect::<Vec<_>>();

        let requires_apply = !self.apply && !self.dry_run_args.dry_run && !apply_risks.is_empty();

        ReleaseExecutionPlan::new(phase, requires_apply, apply_risks)
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
            apply: false,
            deploy,
            recover,
            retag: false,
            head,
            from_artifacts,
            package_only: false,
            tag: None,
            skip_checks: if skip_checks { Some(Vec::new()) } else { None },
            skip_build_validation: false,
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
    let (skip_checks, skip_checks_granular) = args.resolve_skip_checks()?;
    let execution = args.execution_plan(skip_checks);
    validate_apply_boundary(&execution)?;
    let component_ids = resolve_component_ids(&args, &args.components)?;
    let bump_override = args.bump.clone();

    if args.package_only {
        return run_package_only(args, &component_ids);
    }

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
            skip_build_validation: args.skip_build_validation,
            bump_override: bump_override.clone(),
            force_lower_bump: args.force_lower_bump,
            pipeline: args.pipeline_options(),
            skip_github_release: args.no_github_release,
            git_identity: args.git_identity.clone(),
            execution: Some(execution.clone()),
        })?;

        return Ok((
            ReleaseCommandOutput::Single(ReleaseOutput {
                variant: "single",
                result,
            }),
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
        skip_build_validation: args.skip_build_validation,
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
        execution: Some(execution),
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
            variant: "batch",
            result: batch_result,
        }),
        exit_code,
    ))
}

fn run_package_only(
    args: ReleaseArgs,
    component_ids: &[String],
) -> CmdResult<ReleaseCommandOutput> {
    if component_ids.len() != 1 {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "components",
            "--package-only supports exactly one component",
            None,
            Some(vec![
                "Run package recovery once per component: homeboy release <component-id> --package-only --tag <tag> --apply".to_string(),
            ]),
        ));
    }
    if args.project.is_some() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "project",
            "--package-only does not support --project",
            args.project.clone(),
            None,
        ));
    }
    if args.recover || args.retag || args.head || args.deploy || args.skip_publish {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "package-only",
            "--package-only cannot be combined with release execution modes such as --recover, --retag, --head, --deploy, or --skip-publish",
            None,
            None,
        ));
    }
    if args.from_artifacts.is_some() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "from-artifacts",
            "--from-artifacts is for --head publish recovery; --package-only regenerates artifacts instead",
            args.from_artifacts.clone(),
            None,
        ));
    }
    if args.dry_run_args.dry_run {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "dry-run",
            "--package-only writes release artifacts and does not support --dry-run",
            None,
            Some(vec![
                "Use a temporary artifact root to inspect output: homeboy --artifact-root <dir> release <component-id> --package-only --tag <tag> --apply".to_string(),
            ]),
        ));
    }
    if args.bump.is_some() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "bump",
            "--package-only packages an existing tag and cannot be combined with --bump",
            args.bump.clone(),
            None,
        ));
    }
    let tag = args.tag.clone().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec![
            "--tag <existing-release-tag>".to_string()
        ])
    })?;

    let result = release::package_existing_tag(
        &component_ids[0],
        args.path.clone(),
        &tag,
        args.skip_build_validation,
    )?;

    Ok((
        ReleaseCommandOutput::Package(ReleasePackageOutput {
            variant: "package",
            result,
        }),
        0,
    ))
}

fn validate_apply_boundary(execution: &ReleaseExecutionPlan) -> homeboy::core::Result<()> {
    if !execution.requires_apply {
        return Ok(());
    }

    let risky_flags = execution.apply_risks.join(" and ");

    Err(homeboy::core::Error::validation_invalid_argument(
        "apply",
        format!(
            "Real releases with {risky_flags} require explicit --apply. Use --dry-run to preview or re-run with --apply to release."
        ),
        None,
        None,
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
            apply: false,
            deploy: false,
            recover: false,
            retag: false,
            head: false,
            from_artifacts: None,
            package_only: false,
            tag: None,
            skip_checks: skip_checks
                .map(|values| values.iter().map(|value| value.to_string()).collect()),
            skip_build_validation: false,
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

    #[test]
    fn risky_real_release_requires_apply() {
        let mut args = args(&["fixture"]);
        args.dry_run_args.dry_run = false;
        args.head = true;

        let execution = args.execution_plan(false);
        let err = validate_apply_boundary(&execution).expect_err("--head requires --apply");

        assert!(err
            .message
            .contains("Real releases with --head require explicit --apply"));
    }

    #[test]
    fn package_only_real_release_requires_apply() {
        let mut args = args(&["fixture"]);
        args.dry_run_args.dry_run = false;
        args.package_only = true;
        args.tag = Some("v1.2.3".to_string());

        let execution = args.execution_plan(false);
        let err = validate_apply_boundary(&execution).expect_err("--package-only requires --apply");

        assert!(err
            .message
            .contains("Real releases with --package-only require explicit --apply"));
    }

    #[test]
    fn package_only_requires_tag() {
        let mut args = args(&["fixture"]);
        args.dry_run_args.dry_run = false;
        args.package_only = true;
        args.apply = true;

        let err = match run_package_only(args, &["fixture".to_string()]) {
            Ok(_) => panic!("package-only requires an explicit tag"),
            Err(err) => err,
        };

        assert_eq!(err.code.as_str(), "validation.missing_argument");
        assert_eq!(err.details["args"][0], "--tag <existing-release-tag>");
    }

    #[test]
    fn package_only_rejects_publish_modes() {
        let mut args = args(&["fixture"]);
        args.dry_run_args.dry_run = false;
        args.package_only = true;
        args.apply = true;
        args.head = true;
        args.tag = Some("v1.2.3".to_string());

        let err = match run_package_only(args, &["fixture".to_string()]) {
            Ok(_) => panic!("package-only is mutually exclusive with --head"),
            Err(err) => err,
        };

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("--package-only cannot be combined"));
    }

    #[test]
    fn risky_dry_run_release_does_not_require_apply() {
        let mut args = args(&["fixture"]);
        args.head = true;

        let execution = args.execution_plan(false);
        validate_apply_boundary(&execution).expect("dry-run may preview risky mode");
    }

    #[test]
    fn bare_skip_checks_real_release_requires_apply() {
        let mut args = args(&["fixture"]);
        args.dry_run_args.dry_run = false;

        let execution = args.execution_plan(true);
        let err =
            validate_apply_boundary(&execution).expect_err("bare --skip-checks requires --apply");

        assert!(err
            .message
            .contains("Real releases with bare --skip-checks require explicit --apply"));
    }

    #[test]
    fn granular_skip_checks_real_release_does_not_require_apply() {
        let mut args = args(&["fixture"]);
        args.dry_run_args.dry_run = false;

        let execution = args.execution_plan(false);
        validate_apply_boundary(&execution).expect("granular skip-checks is not guarded");
    }

    #[test]
    fn apply_confirms_risky_real_release() {
        let mut args = args(&["fixture"]);
        args.dry_run_args.dry_run = false;
        args.recover = true;
        args.retag = true;
        args.apply = true;

        let execution = args.execution_plan(false);
        validate_apply_boundary(&execution).expect("--apply confirms risky release mode");
    }

    #[test]
    fn execution_plan_resolves_phase_from_args() {
        let mut args = args(&["fixture"]);
        args.dry_run_args.dry_run = false;
        args.skip_publish = true;

        let execution = args.execution_plan(false);

        assert_eq!(execution.phase, ReleasePhase::Prepare);
        assert!(!execution.requires_apply);
    }
}
