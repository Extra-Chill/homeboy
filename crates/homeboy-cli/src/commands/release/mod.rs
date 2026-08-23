use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::{fs, path::Path};

use homeboy::core::component;
use homeboy::core::scope::{self, Scope};
use homeboy_deploy::{self as deploy, ReleaseStateStatus};
use homeboy_release::release::{
    self, ArtifactSourceAuthorityManifest, BatchReleaseResult, ReleaseCommandInput,
    ReleaseCommandResult, ReleaseExecutionPlan, ReleasePackageResult, ReleasePhase,
    ReleasePipelineOptions, ReleasePreflightPlacement, ReleasePreflightPlacementPolicy,
    ReleasePreflightSourceIdentity, ReleaseReadinessEnvelope, ReleaseReadinessGateResult,
    ReleaseReadinessLocalOnly, ReleaseReadinessProvenance,
};

use super::utils::args::DryRunArgs;
use super::utils::response::{CommandActionableMetadata, CommandNextAction, CommandNextActionKind};
use super::CmdResult;

pub mod changelog;
pub mod changes;
pub mod contains;
pub mod version;

#[derive(Args)]
pub struct ReleaseArgs {
    #[command(subcommand)]
    command: Option<ReleaseSubcommand>,

    #[command(flatten)]
    execute: ReleaseExecuteArgs,
}

impl ReleaseArgs {
    pub(crate) fn is_changelog_markdown(&self) -> bool {
        matches!(
            &self.command,
            Some(ReleaseSubcommand::Changelog(args)) if changelog::is_show_markdown(args)
        )
    }

    pub(crate) fn markdown_changelog_args(self) -> Option<changelog::ChangelogArgs> {
        match self.command {
            Some(ReleaseSubcommand::Changelog(args)) if changelog::is_show_markdown(&args) => {
                Some(args)
            }
            _ => None,
        }
    }
}

#[derive(Subcommand)]
enum ReleaseSubcommand {
    /// Show changes since the last version tag
    Changes(changes::ChangesArgs),
    /// Show generated changelog content
    Changelog(changelog::ChangelogArgs),
    /// Version inspection helpers
    Version(ReleaseVersionArgs),
    /// Write a source-authority manifest for assembled release artifacts
    ArtifactSourceAuthority(ArtifactSourceAuthorityArgs),
    /// Report which release first contained a commit, and whether the installed build has it
    Contains(contains::ContainsArgs),
    /// Report how far the installed build is behind the newest release
    Gap(contains::GapArgs),
    /// Inspect retained portable release-readiness evidence
    Readiness(ReleaseReadinessArgs),
}

#[derive(Args)]
struct ReleaseReadinessArgs {
    #[command(subcommand)]
    command: ReleaseReadinessCommand,
}

#[derive(Subcommand)]
enum ReleaseReadinessCommand {
    /// Show one retained readiness operation by operation:// reference or ID
    Show { reference: String },
    /// List retained readiness operations for a component
    List { component_id: String },
}

#[derive(Args)]
struct ArtifactSourceAuthorityArgs {
    /// Component prepared for finalization
    component_id: String,
    /// Directory containing the assembled publication files
    #[arg(long, value_name = "DIR")]
    dir: String,
    /// Prepared release tag
    #[arg(long)]
    tag: String,
    /// Prepared release version
    #[arg(long)]
    version: String,
    /// Exact commit the prepared tag resolves to
    #[arg(long)]
    commit: String,
    /// Exact persisted GitHub Release body to bind as a non-publication control artifact
    #[arg(long, value_name = "PATH")]
    release_notes: Option<String>,
}

#[derive(Args)]
struct ReleaseVersionArgs {
    #[command(subcommand)]
    command: version::VersionCommand,
}

#[derive(Args)]
pub struct ReleaseExecuteArgs {
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

    /// Run portable lint and test release gates through the existing Lab review
    /// commands before controller-owned release mutation.
    #[arg(long, value_name = "RUNNER_ID", conflicts_with = "preflight_placement")]
    preflight_runner: Option<String>,

    /// Placement policy for portable release preflight gates.
    #[arg(long, value_enum, default_value_t = ReleasePreflightPlacementArg::Local)]
    preflight_placement: ReleasePreflightPlacementArg,

    #[command(flatten)]
    dry_run_args: DryRunArgs,

    /// Emit the complete release command-result envelope on stdout.
    ///
    /// The default is a bounded operator summary; `--output <path>` always
    /// writes the complete structured result.
    #[arg(long)]
    pub full: bool,

    /// Confirm risky release execution modes.
    #[arg(long)]
    apply: bool,

    /// Deploy to all projects using this component after release
    #[arg(long)]
    deploy: bool,

    /// Recover from an interrupted release (tag + push current version)
    #[arg(long)]
    recover: bool,

    /// Provider workspace owner reference to reconcile during --recover.
    #[arg(long, value_name = "OWNER_RUN_REF")]
    owner_run_ref: Option<String>,

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
    /// Combine with --head --tag <tag> --apply to write a durable artifact
    /// inventory for later --head --from-artifacts finalization.
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
    ///
    /// SHARP / MANUAL OVERRIDE. On a manual/local release of a GitHub component,
    /// suppressing the reviewer-facing GitHub Release is almost never what you
    /// want — humans expect the release page to exist. This flag is therefore
    /// gated: pass --i-know-ci-creates-the-github-release to confirm you really
    /// intend a tag-only release because CI (or another pipeline) owns GitHub
    /// Release creation. Without that confirmation the release is refused.
    #[arg(long)]
    no_github_release: bool,

    /// Confirm that --no-github-release is intentional on a manual/local release
    /// because CI (or another pipeline) creates the GitHub Release. Required
    /// whenever --no-github-release is used on a component that would otherwise
    /// get a reviewer-facing GitHub Release.
    #[arg(long)]
    i_know_ci_creates_the_github_release: bool,

    /// Confirm an intentional manual tag-only release. Use only when no CI-owned
    /// GitHub Release automation should create the reviewer-facing release page.
    #[arg(long)]
    i_know_this_is_a_manual_tag_only_release: bool,

    /// Git identity for release commits and tags.
    /// Use "bot" for the default CI bot identity, or "Name <email>" for custom.
    /// When set, configures git user.name and user.email before committing.
    #[arg(long)]
    git_identity: Option<String>,

    /// After releasing the component, release every dependent that declares a
    /// dependency on it: update the dependent's declared dependency pin and
    /// release it with an automatic patch bump, transitively. Single-component
    /// releases only.
    #[arg(long)]
    cascade: bool,
}

impl ReleaseArgs {
    pub(crate) fn requests_full_output(&self) -> bool {
        self.execute.full
    }
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum ReleasePreflightPlacementArg {
    #[default]
    Local,
    Lab,
}

#[derive(Serialize)]
#[serde(tag = "command", rename = "release")]
pub struct ReleaseOutput {
    pub variant: &'static str,
    pub result: ReleaseCommandResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<release::ReleaseWorkspaceOutput>,
    /// Dependency-aware cascade result, present when `--cascade` released
    /// dependents after this component.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cascade: Option<release::CascadeResult>,
    #[serde(
        rename = "_homeboy_actionable",
        skip_serializing_if = "Option::is_none"
    )]
    pub actionable: Option<CommandActionableMetadata>,
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
#[serde(tag = "command", rename = "release.artifact-source-authority")]
pub struct ArtifactSourceAuthorityOutput {
    pub variant: &'static str,
    pub manifest: ArtifactSourceAuthorityManifest,
}

#[derive(Serialize)]
pub struct ReleaseReadinessShowOutput {
    pub variant: &'static str,
    pub record: release::operation_record::OperationRecord,
}

#[derive(Serialize)]
pub struct ReleaseReadinessListOutput {
    pub variant: &'static str,
    pub records: Vec<release::operation_record::OperationRecord>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum ReleaseCommandOutput {
    Single(Box<ReleaseOutput>),
    Batch(BatchReleaseOutput),
    Package(Box<ReleasePackageOutput>),
    ArtifactSourceAuthority(ArtifactSourceAuthorityOutput),
    Changes(changes::ChangesCommandOutput),
    Changelog(changelog::ChangelogOutput),
    Version(version::VersionOutput),
    Contains(Box<release::ReleaseContainsReport>),
    Gap(Box<release::ReleaseGapReport>),
    ReadinessShow(ReleaseReadinessShowOutput),
    ReadinessList(ReleaseReadinessListOutput),
}

fn map_nested<T>(
    result: CmdResult<T>,
    wrap: impl FnOnce(T) -> ReleaseCommandOutput,
) -> CmdResult<ReleaseCommandOutput> {
    result.map(|(output, code)| (wrap(output), code))
}

fn artifact_source_authority_release_notes(
    args: &ArtifactSourceAuthorityArgs,
) -> Option<std::path::PathBuf> {
    let current_dir = std::env::current_dir().ok();
    let component_path = component::load(&args.component_id)
        .ok()
        .map(|component| std::path::PathBuf::from(component.local_path));
    select_artifact_source_authority_release_notes(
        args.release_notes.as_deref(),
        current_dir.as_deref(),
        component_path.as_deref(),
        &args.tag,
    )
}

fn select_artifact_source_authority_release_notes(
    explicit: Option<&str>,
    current_dir: Option<&std::path::Path>,
    component_path: Option<&std::path::Path>,
    tag: &str,
) -> Option<std::path::PathBuf> {
    if let Some(path) = explicit {
        return Some(std::path::PathBuf::from(path));
    }
    [current_dir, component_path]
        .into_iter()
        .flatten()
        .find_map(|path| {
            let path = path.join(release::release_notes_path(tag));
            path.is_file().then_some(path)
        })
}

impl ReleaseExecuteArgs {
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

pub fn run(args: ReleaseArgs) -> CmdResult<ReleaseCommandOutput> {
    match args.command {
        Some(ReleaseSubcommand::Changes(args)) => {
            return map_nested(changes::run(args), ReleaseCommandOutput::Changes);
        }
        Some(ReleaseSubcommand::Changelog(args)) => {
            return map_nested(changelog::run(args), ReleaseCommandOutput::Changelog);
        }
        Some(ReleaseSubcommand::Version(args)) => {
            return map_nested(
                version::run_command(args.command),
                ReleaseCommandOutput::Version,
            );
        }
        Some(ReleaseSubcommand::Contains(args)) => {
            return map_nested(contains::run_contains(args), |report| {
                ReleaseCommandOutput::Contains(Box::new(report))
            });
        }
        Some(ReleaseSubcommand::Gap(args)) => {
            return map_nested(contains::run_gap(args), |report| {
                ReleaseCommandOutput::Gap(Box::new(report))
            });
        }
        Some(ReleaseSubcommand::ArtifactSourceAuthority(args)) => {
            let release_notes = artifact_source_authority_release_notes(&args);
            let manifest = release::write_artifact_source_authority_manifest(
                Path::new(&args.dir),
                &args.component_id,
                &args.tag,
                &args.version,
                &args.commit,
                release_notes.as_deref(),
            )?;
            return Ok((
                ReleaseCommandOutput::ArtifactSourceAuthority(ArtifactSourceAuthorityOutput {
                    variant: "artifact-source-authority",
                    manifest,
                }),
                0,
            ));
        }
        Some(ReleaseSubcommand::Readiness(args)) => match args.command {
            ReleaseReadinessCommand::Show { reference } => {
                // The readiness subcommand is its own unit of work, so it
                // resolves roots once and binds the record store to them
                // rather than letting each store call rediscover a home
                // (#7505).
                let store = release::operation_record::OperationRecordStore::in_roots(
                    &homeboy::core::paths::PathRoots::from_environment()?,
                );
                let owner_run_ref = reference.strip_prefix("operation://").unwrap_or(&reference);
                let record = store.load(owner_run_ref)?.ok_or_else(|| {
                    homeboy::core::Error::validation_invalid_argument(
                        "reference",
                        "release readiness operation does not exist",
                        Some(reference.clone()),
                        None,
                    )
                })?;
                if record.operation != "release_readiness" {
                    return Err(homeboy::core::Error::validation_invalid_argument(
                        "reference",
                        "operation reference is not a release readiness record",
                        Some(reference),
                        None,
                    ));
                }
                return Ok((
                    ReleaseCommandOutput::ReadinessShow(ReleaseReadinessShowOutput {
                        variant: "readiness-show",
                        record,
                    }),
                    0,
                ));
            }
            ReleaseReadinessCommand::List { component_id } => {
                let store = release::operation_record::OperationRecordStore::in_roots(
                    &homeboy::core::paths::PathRoots::from_environment()?,
                );
                let records = store.for_subject("release_readiness", &component_id, false)?;
                return Ok((
                    ReleaseCommandOutput::ReadinessList(ReleaseReadinessListOutput {
                        variant: "readiness-list",
                        records,
                    }),
                    0,
                ));
            }
        },
        None => {}
    }

    run_execute(args.execute)
}

fn run_portable_preflight(
    args: &ReleaseExecuteArgs,
    component_id: &str,
    skip_all: bool,
    skipped: &[String],
) -> homeboy::core::Result<Option<ReleaseReadinessEnvelope>> {
    run_portable_preflight_with(
        &CliPortableStageDispatcher,
        args,
        component_id,
        skip_all,
        skipped,
    )
}

struct PortableStageRequest<'a> {
    gate: &'a str,
    component_id: &'a str,
    path: &'a str,
    source_commit: &'a str,
    requested_runner_id: Option<&'a str>,
    placement: ReleasePreflightPlacementArg,
}

#[derive(Debug, Clone)]
struct PortableStageChildResult {
    passed: bool,
    runner_id: Option<String>,
    evidence_refs: Vec<String>,
    provenance: ReleaseReadinessProvenance,
}

trait PortableStageDispatcher {
    fn dispatch(
        &self,
        request: PortableStageRequest<'_>,
    ) -> homeboy::core::Result<PortableStageChildResult>;
}

struct CliPortableStageDispatcher;

impl PortableStageDispatcher for CliPortableStageDispatcher {
    fn dispatch(
        &self,
        request: PortableStageRequest<'_>,
    ) -> homeboy::core::Result<PortableStageChildResult> {
        let executable = std::env::current_exe().map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some("release preflight executable".to_string()),
            )
        })?;
        let mut command = Command::new(executable);
        if let Some(runner_id) = request.requested_runner_id {
            command.args(["--runner", runner_id]);
        } else if matches!(request.placement, ReleasePreflightPlacementArg::Lab) {
            command.args(["--placement", "lab"]);
        }
        command.args([
            "review",
            request.gate,
            request.component_id,
            "--path",
            request.path,
            "--release-readiness-source",
            request.source_commit,
        ]);
        let output = command.output().map_err(|error| {
            homeboy::core::Error::internal_io(
                format!("start portable release {} preflight: {error}", request.gate),
                Some(request.path.to_string()),
            )
        })?;
        let envelope: PortableReviewChildEnvelope = serde_json::from_slice(&output.stdout)
            .map_err(|error| {
                homeboy::core::Error::validation_invalid_argument(
                    "release.preflight",
                    format!(
                        "portable {} preflight returned no stable command-result JSON: {error}",
                        request.gate
                    ),
                    Some(String::from_utf8_lossy(&output.stderr).to_string()),
                    None,
                )
            })?;
        envelope.project(output.status.success(), request.source_commit)
    }
}

/// Stable subset of a child command-result envelope consumed by release.
/// Release deliberately retains only durable references and resolved placement,
/// never an opaque copy of child JSON.
#[derive(Deserialize)]
struct PortableReviewChildEnvelope {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    run: Option<PortableChildRunRef>,
    #[serde(default)]
    refs: PortableChildRefs,
    #[serde(default)]
    artifacts: Vec<PortableChildArtifactRef>,
    #[serde(default)]
    evidence: Vec<PortableChildArtifactRef>,
    #[serde(default)]
    data: Option<PortableReviewChildData>,
}

#[derive(Default, Deserialize)]
struct PortableChildRefs {
    #[serde(default)]
    runs: Vec<PortableChildRunRef>,
}

#[derive(Deserialize)]
struct PortableChildRunRef {
    id: String,
}
#[derive(Deserialize)]
struct PortableChildArtifactRef {
    uri: String,
}
#[derive(Deserialize)]
struct PortableReviewChildData {
    release_readiness: PortableChildReadinessEvidence,
}

#[derive(Deserialize)]
struct PortableChildReadinessEvidence {
    requested_source_commit: String,
    source_commit: String,
    runner_id: Option<String>,
    provenance: ReleaseReadinessProvenance,
}
impl PortableReviewChildEnvelope {
    fn project(
        self,
        process_passed: bool,
        requested_source_commit: &str,
    ) -> homeboy::core::Result<PortableStageChildResult> {
        let mut evidence_refs = self
            .evidence
            .into_iter()
            .map(|reference| reference.uri)
            .collect::<Vec<_>>();
        evidence_refs.extend(self.artifacts.into_iter().map(|reference| reference.uri));
        if let Some(run) = self.run.as_ref() {
            evidence_refs.push(format!("run://{}", run.id));
        }
        evidence_refs.extend(
            self.refs
                .runs
                .into_iter()
                .map(|run| format!("run://{}", run.id)),
        );
        evidence_refs.sort();
        evidence_refs.dedup();
        let child = self.data.ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "release.preflight",
                "portable child omitted typed readiness evidence",
                None,
                None,
            )
        })?;
        let readiness = child.release_readiness;
        if process_passed && self.success {
            validate_portable_child_success(&readiness, requested_source_commit, &evidence_refs)?;
        }
        Ok(PortableStageChildResult {
            passed: process_passed && self.success,
            runner_id: readiness.runner_id,
            evidence_refs,
            provenance: readiness.provenance,
        })
    }
}

fn validate_portable_child_success(
    child: &PortableChildReadinessEvidence,
    requested_source_commit: &str,
    evidence_refs: &[String],
) -> homeboy::core::Result<()> {
    if child.source_commit != requested_source_commit {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "release.preflight",
            "portable child source commit does not match frozen release source",
            Some(child.source_commit.clone()),
            None,
        ));
    }
    if child.requested_source_commit != requested_source_commit {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "release.preflight",
            "portable child did not retain the frozen release source request",
            Some(child.requested_source_commit.clone()),
            None,
        ));
    }
    if child.runner_id.is_none() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "release.preflight",
            "portable child omitted its resolved runner ID",
            None,
            None,
        ));
    }
    if evidence_refs.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "release.preflight",
            "portable child omitted durable run, artifact, or evidence references",
            None,
            None,
        ));
    }
    if ReleaseReadinessProvenance::is_empty(&child.provenance) {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "release.preflight",
            "portable child omitted immutable dependency and extension provenance",
            None,
            None,
        ));
    }
    Ok(())
}

fn run_portable_preflight_with(
    dispatcher: &dyn PortableStageDispatcher,
    args: &ReleaseExecuteArgs,
    component_id: &str,
    skip_all: bool,
    skipped: &[String],
) -> homeboy::core::Result<Option<ReleaseReadinessEnvelope>> {
    let runner_id = args.preflight_runner.as_deref();
    if runner_id.is_none() && !matches!(args.preflight_placement, ReleasePreflightPlacementArg::Lab)
    {
        return Ok(None);
    }
    let component = component::resolve_effective(Some(component_id), args.path.as_deref(), None)?;
    let commit = homeboy::core::git::get_head_commit(&component.local_path)?;
    let mut gate_results = Vec::new();
    let mut evidence_refs = Vec::new();
    let mut resolved_runner_id = None;
    for gate in ["audit", "lint", "test"] {
        if skip_all || skipped.iter().any(|skip| skip == gate) {
            gate_results.push(ReleaseReadinessGateResult {
                gate: gate.to_string(),
                status: "skipped".to_string(),
                reason: Some("--skip-checks".to_string()),
                source_sha: Some(commit.clone()),
                runner_id: None,
                evidence_refs: Vec::new(),
                provenance: None,
                local_only: None,
            });
            continue;
        }
        let child = dispatcher.dispatch(PortableStageRequest {
            gate,
            component_id,
            path: &component.local_path,
            source_commit: &commit,
            requested_runner_id: runner_id,
            placement: args.preflight_placement,
        });
        match child {
            Ok(child) => {
                resolved_runner_id = child.runner_id.clone().or(resolved_runner_id);
                evidence_refs.extend(child.evidence_refs.iter().cloned());
                gate_results.push(ReleaseReadinessGateResult {
                    gate: gate.to_string(),
                    status: if child.passed { "passed" } else { "failed" }.to_string(),
                    reason: None,
                    source_sha: Some(commit.clone()),
                    runner_id: child.runner_id,
                    evidence_refs: child.evidence_refs,
                    provenance: Some(child.provenance),
                    local_only: None,
                });
            }
            Err(error) => {
                // Keep dispatch failures alongside other gate outcomes. The
                // retained readiness operation makes this error durable.
                gate_results.push(ReleaseReadinessGateResult {
                    gate: gate.to_string(),
                    status: "failed".to_string(),
                    reason: Some(format!("portable dispatch failed: {error}")),
                    source_sha: Some(commit.clone()),
                    runner_id: runner_id.map(str::to_string),
                    evidence_refs: Vec::new(),
                    provenance: None,
                    local_only: None,
                });
            }
        }
    }
    // Package preflight owns release-specific lockfile and local-dependency
    // guards. No portable review command exposes that contract yet, so retain
    // the explicit controller placement while other gates still offload.
    gate_results.push(ReleaseReadinessGateResult {
        gate: "package_preflight".to_string(),
        status: "local_only".to_string(),
        reason: Some("release package guards have no portable command contract".to_string()),
        source_sha: Some(commit.clone()),
        runner_id: None,
        evidence_refs: Vec::new(),
        provenance: None,
        local_only: Some(ReleaseReadinessLocalOnly {
            reason: "release package guards have no portable command contract".to_string(),
            continuation: "preflight.package".to_string(),
        }),
    });
    let placement = ReleasePreflightPlacement {
        policy: if runner_id.is_some()
            || matches!(args.preflight_placement, ReleasePreflightPlacementArg::Lab)
        {
            ReleasePreflightPlacementPolicy::Lab
        } else {
            ReleasePreflightPlacementPolicy::Local
        },
        runner_id: resolved_runner_id
            .clone()
            .or_else(|| runner_id.map(str::to_string)),
    };
    Ok(Some(ReleaseReadinessEnvelope {
        source: ReleasePreflightSourceIdentity { commit },
        placement,
        runner_id: resolved_runner_id.or_else(|| runner_id.map(str::to_string)),
        evidence_refs,
        provenance: gate_results
            .iter()
            .filter_map(|gate| gate.provenance.as_ref())
            .fold(
                ReleaseReadinessProvenance::default(),
                |mut all, provenance| {
                    all.dependencies.extend(provenance.dependencies.clone());
                    all.extensions.extend(provenance.extensions.clone());
                    all
                },
            ),
        gate_results,
    }))
}

fn run_execute(args: ReleaseExecuteArgs) -> CmdResult<ReleaseCommandOutput> {
    let (skip_checks, mut skip_checks_granular) = args.resolve_skip_checks()?;
    let execution = args.execution_plan(skip_checks);
    validate_apply_boundary(&execution)?;
    let component_ids = resolve_component_ids(&args, &args.components)?;
    if args.package_only {
        validate_package_only_intent(&args, &component_ids)?;
    }
    if component_ids.len() > 1
        && (args.preflight_runner.is_some()
            || matches!(args.preflight_placement, ReleasePreflightPlacementArg::Lab))
    {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "preflight-runner",
            "portable release preflight currently requires one component",
            None,
            None,
        ));
    }
    let readiness = if component_ids.len() == 1 {
        run_portable_preflight(&args, &component_ids[0], skip_checks, &skip_checks_granular)?
    } else {
        None
    };
    if readiness.is_some() {
        for gate in ["audit", "lint", "test"] {
            if !skip_checks_granular.iter().any(|existing| existing == gate) {
                skip_checks_granular.push(gate.to_string());
            }
        }
    }
    let bump_override = args.bump.clone();

    guard_no_github_release(&args, &component_ids)?;

    if args.package_only {
        if readiness
            .as_ref()
            .is_some_and(|value| !release::readiness_is_valid(value))
        {
            let input = ReleaseCommandInput {
                component_id: component_ids[0].clone(),
                path_override: args.path.clone(),
                readiness,
                ..Default::default()
            };
            return match release::run_command_with_workspace(input, args.owner_run_ref.as_deref()) {
                Ok(_) => Err(homeboy::core::Error::internal_unexpected(
                    "invalid readiness unexpectedly passed",
                )),
                Err(error) => Err(error),
            };
        }
        return run_package_only(args, &component_ids, readiness.as_ref());
    }

    // Single component: use the original single-release flow
    if component_ids.len() == 1 {
        let component_id = &component_ids[0];
        let input = ReleaseCommandInput {
            component_id: component_id.clone(),
            path_override: args.path.clone(),
            dry_run: args.dry_run_args.dry_run,
            recover: args.recover,
            retag: args.retag,
            skip_checks,
            skip_checks_granular: skip_checks_granular.clone(),
            skip_build_validation: args.skip_build_validation,
            skip_deps_hydration: crate::commands::skip_deps_hydration(),
            bump_override: bump_override.clone(),
            force_lower_bump: args.force_lower_bump,
            pipeline: args.pipeline_options(),
            skip_github_release: args.no_github_release,
            git_identity: args.git_identity.clone(),
            execution: Some(execution.clone()),
            readiness,
        };
        let (workspace_result, exit_code) =
            release::run_command_with_workspace(input.clone(), args.owner_run_ref.as_deref())?;
        let result = workspace_result.result;

        let cascade = run_cascade_if_requested(&args, component_id, &result, &input)?;

        return Ok((
            ReleaseCommandOutput::Single(Box::new(ReleaseOutput {
                variant: "single",
                actionable: release_actionable_metadata(&result),
                result,
                workspace: workspace_result.workspace,
                cascade,
            })),
            exit_code,
        ));
    }

    if args.cascade {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "cascade",
            "--cascade releases dependents of a single component; run one component at a time",
            None,
            None,
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
        skip_deps_hydration: crate::commands::skip_deps_hydration(),
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
        readiness: None,
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

fn release_actionable_metadata(result: &ReleaseCommandResult) -> Option<CommandActionableMetadata> {
    let mut metadata = CommandActionableMetadata::default();
    if let Some(command) = result.continuation_command.as_ref() {
        metadata = metadata.with_next_action(
            CommandNextAction::new("finish release publication", command)
                .with_kind(CommandNextActionKind::Repair),
        );
    }
    if let Some(reference) = result.readiness.as_ref().and_then(|readiness| {
        readiness
            .evidence_refs
            .iter()
            .find_map(|reference| reference.strip_prefix("operation://"))
    }) {
        metadata = metadata.with_next_action(
            CommandNextAction::new(
                "inspect release readiness",
                format!("homeboy release readiness show {reference}"),
            )
            .with_kind(CommandNextActionKind::Show),
        );
    }
    (!metadata.is_empty()).then_some(metadata)
}

/// Run the dependency-aware cascade after a single-component release, when
/// `--cascade` was requested and the component actually released.
fn run_cascade_if_requested(
    args: &ReleaseExecuteArgs,
    component_id: &str,
    result: &ReleaseCommandResult,
    input: &ReleaseCommandInput,
) -> Result<Option<release::CascadeResult>, homeboy::core::Error> {
    if !args.cascade {
        return Ok(None);
    }

    if args.dry_run_args.dry_run {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "cascade",
            "--cascade mutates and releases dependents; it cannot be combined with --dry-run",
            None,
            None,
        ));
    }

    // Only cascade when the upstream actually produced a release; a skipped or
    // failed upstream has no new coordinates to propagate.
    if result.status != "released" {
        return Ok(None);
    }

    let component = component::resolve_effective(Some(component_id), args.path.as_deref(), None)?;
    let sha = homeboy::core::git::get_head_commit(&component.local_path).unwrap_or_default();
    let root = release::ReleasedCoordinates {
        component_id: component_id.to_string(),
        version: result.new_version.clone().unwrap_or_default(),
        tag: result.tag.clone().unwrap_or_default(),
        sha,
    };

    Ok(Some(release::run_cascade(&root, input)?))
}

fn run_package_only(
    args: ReleaseExecuteArgs,
    component_ids: &[String],
    readiness: Option<&ReleaseReadinessEnvelope>,
) -> CmdResult<ReleaseCommandOutput> {
    validate_package_only_intent(&args, component_ids)?;
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
    validate_package_only_args(&args)?;
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
                "Use a temporary artifact root to inspect output: homeboy --artifact-root <dir> release <component-id> --head --package-only --tag <tag> --apply".to_string(),
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
        readiness.map(|value| value.source.commit.as_str()),
    )?;

    Ok((
        ReleaseCommandOutput::Package(Box::new(ReleasePackageOutput {
            variant: "package",
            result,
        })),
        0,
    ))
}

fn validate_package_only_intent(
    args: &ReleaseExecuteArgs,
    component_ids: &[String],
) -> homeboy::core::Result<()> {
    if component_ids.len() != 1 {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "components",
            "--package-only supports exactly one component",
            None,
            None,
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
    validate_package_only_args(args)?;
    if args.from_artifacts.is_some() {
        return Err(homeboy::core::Error::validation_invalid_argument("from-artifacts", "--from-artifacts is for --head publish recovery; --package-only regenerates artifacts instead", args.from_artifacts.clone(), None));
    }
    if args.dry_run_args.dry_run {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "dry-run",
            "--package-only writes release artifacts and does not support --dry-run",
            None,
            None,
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
    if args.tag.is_none() {
        return Err(homeboy::core::Error::validation_missing_argument(vec![
            "--tag <existing-release-tag>".to_string(),
        ]));
    }
    Ok(())
}

fn validate_package_only_args(args: &ReleaseExecuteArgs) -> homeboy::core::Result<()> {
    if args.recover || args.retag || args.deploy || args.skip_publish {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "package-only",
            "--package-only cannot be combined with --recover, --retag, --deploy, or --skip-publish",
            None,
            None,
        ));
    }
    Ok(())
}

/// Guard `--no-github-release` as a sharp, manual override (issues #6137, #7049).
///
/// On a manual/local release of a GitHub component, suppressing the
/// reviewer-facing GitHub Release is almost never intended — the flag is too
/// easy to carry over from a tag-only / CI-publish mental model. So whenever
/// `--no-github-release` is used on a component that would otherwise get a
/// GitHub Release, require the explicit `--i-know-ci-creates-the-github-release`
/// confirmation. The error spells out how to either confirm intent or run a
/// normal release, and how to create the GitHub Release later if a tag-only
/// release was already produced.
///
/// `--dry-run` is exempt: previewing a plan with `--no-github-release` should
/// never be blocked, and the dry-run hints already explain the tag-only outcome.
fn guard_no_github_release(
    args: &ReleaseExecuteArgs,
    component_ids: &[String],
) -> homeboy::core::Result<()> {
    if !args.no_github_release {
        return Ok(());
    }
    if args.dry_run_args.dry_run {
        return Ok(());
    }

    // Only gate components that would actually get a reviewer-facing GitHub
    // Release; non-GitHub remotes never create one, so the flag is a no-op there.
    let mut gated: Vec<(&str, component::Component)> = Vec::new();
    for id in component_ids {
        match component::load(id) {
            Ok(component) if release::github_release_expected(&component) => {
                gated.push((id.as_str(), component));
            }
            // Unresolved or non-GitHub components are not gated here — the
            // downstream release flow reports load failures with full context.
            _ => {}
        }
    }

    no_github_release_guard_for_components(args, gated)
}

fn no_github_release_guard_for_components(
    args: &ReleaseExecuteArgs,
    gated: Vec<(&str, component::Component)>,
) -> homeboy::core::Result<()> {
    if !args.no_github_release || args.dry_run_args.dry_run {
        return Ok(());
    }

    if gated.is_empty() {
        return Ok(());
    }

    if args.i_know_this_is_a_manual_tag_only_release {
        return Ok(());
    }

    if args.i_know_ci_creates_the_github_release {
        let missing_evidence = gated
            .iter()
            .filter(|(_, component)| !ci_owned_github_release_evidence(component))
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();

        if missing_evidence.is_empty() {
            return Ok(());
        }

        return Err(no_github_release_error(
            "no-github-release",
            &missing_evidence.join(", "),
            missing_evidence.first().copied().unwrap_or("<component>"),
            Some(
                "--i-know-ci-creates-the-github-release requires machine-checkable evidence that CI creates the GitHub Release.",
            ),
        ));
    }

    let components = gated
        .iter()
        .map(|(id, _)| *id)
        .collect::<Vec<_>>()
        .join(", ");
    let example = gated.first().map(|(id, _)| *id).unwrap_or("<component>");

    Err(no_github_release_error(
        "no-github-release",
        &components,
        example,
        None,
    ))
}

fn no_github_release_error(
    argument: &'static str,
    components: &str,
    example: &str,
    prefix: Option<&str>,
) -> homeboy::core::Error {
    let message = match prefix {
        Some(prefix) => format!(
            "{prefix} --no-github-release would suppress the reviewer-facing GitHub Release for: {components}. Drop --no-github-release for a normal manual release."
        ),
        None => format!(
            "--no-github-release is a SHARP, manual override and would suppress the reviewer-facing GitHub Release for: {components}. On a manual/local release humans expect the GitHub Release page to exist, so this is gated. Confirm only if CI (or another pipeline) creates the GitHub Release, or if you explicitly want a manual tag-only release."
        ),
    };

    homeboy::core::Error::validation_invalid_argument(
        argument,
        message,
        None,
        Some(vec![
            "If CI creates the GitHub Release, add release.github_release.owner = \"ci\" to the component config or ensure a GitHub Actions workflow contains both release-skip-github-release and a release-head finish step, then re-run with --i-know-ci-creates-the-github-release."
                .to_string(),
            "If you actually want a normal release, drop --no-github-release so the GitHub Release \
             is created."
                .to_string(),
            "If you truly want a manual tag-only release, re-run with --i-know-this-is-a-manual-tag-only-release instead of the CI-owned confirmation."
                .to_string(),
            format!(
                "If a tag-only release was already produced, create the GitHub Release later from \
                 the existing tag (same notes Homeboy generated): homeboy release {example} --head"
            ),
        ]),
    )
}

fn ci_owned_github_release_evidence(component: &component::Component) -> bool {
    if component
        .release
        .github_release
        .as_ref()
        .and_then(|config| config.owner)
        == Some(component::GithubReleaseOwner::Ci)
    {
        return true;
    }

    detected_ci_owned_github_release_workflow(Path::new(&component.local_path))
}

fn detected_ci_owned_github_release_workflow(local_path: &Path) -> bool {
    let workflows_dir = local_path.join(".github").join("workflows");
    let Ok(entries) = fs::read_dir(workflows_dir) else {
        return false;
    };

    entries.flatten().any(|entry| {
        let path = entry.path();
        let is_workflow = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| matches!(extension, "yml" | "yaml"))
            .unwrap_or(false);
        if !is_workflow {
            return false;
        }

        let Ok(content) = fs::read_to_string(path) else {
            return false;
        };

        content.contains("release-skip-github-release")
            && (content.contains("release-head") || content.contains("--head"))
            && (content.contains("release-from-artifacts") || content.contains("from-artifacts"))
    })
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
    args: &ReleaseExecuteArgs,
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
        if let Some(path) = args.path.as_deref() {
            return match component::resolve_effective(None, Some(path), None) {
                Ok(comp) => Ok(vec![comp.id]),
                Err(_) => Err(homeboy::core::Error::validation_missing_argument(vec![
                    "component ID(s), or --project <project-id>".to_string(),
                ])),
            };
        }

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
    use std::cell::RefCell;

    /// The record store for the isolated home each test below installs.
    ///
    /// `with_isolated_home` establishes the home; this binds a store to it once,
    /// the same way the release boundary binds one for a whole command (#7505).
    fn test_store() -> release::operation_record::OperationRecordStore {
        release::operation_record::OperationRecordStore::in_roots(
            &homeboy::core::paths::PathRoots::from_environment().expect("path roots"),
        )
    }

    fn args(components: &[&str]) -> ReleaseExecuteArgs {
        ReleaseExecuteArgs {
            components: components.iter().map(|value| value.to_string()).collect(),
            project: None,
            outdated: false,
            path: None,
            preflight_runner: None,
            preflight_placement: ReleasePreflightPlacementArg::Local,
            dry_run_args: DryRunArgs { dry_run: true },
            full: false,
            apply: false,
            deploy: false,
            recover: false,
            owner_run_ref: None,
            retag: false,
            head: false,
            from_artifacts: None,
            package_only: false,
            tag: None,
            skip_checks: None,
            skip_build_validation: false,
            bump: None,
            force_lower_bump: false,
            skip_publish: false,
            no_github_release: false,
            i_know_ci_creates_the_github_release: false,
            i_know_this_is_a_manual_tag_only_release: false,
            git_identity: None,
            cascade: false,
        }
    }

    #[test]
    fn artifact_source_authority_uses_configured_notes_when_cwd_has_none() {
        let current = tempfile::tempdir().expect("current directory");
        let component = tempfile::tempdir().expect("component directory");
        let canonical = component.path().join(release::release_notes_path("v1.2.3"));
        std::fs::create_dir_all(canonical.parent().unwrap()).expect("build dir");
        std::fs::write(&canonical, "exact body").expect("canonical notes");

        assert_eq!(
            select_artifact_source_authority_release_notes(
                None,
                Some(current.path()),
                Some(component.path()),
                "v1.2.3",
            ),
            Some(canonical)
        );
    }

    #[test]
    fn artifact_source_authority_prefers_cwd_notes_over_configured_component() {
        let current = tempfile::tempdir().expect("current directory");
        let component = tempfile::tempdir().expect("component directory");
        let cwd_notes = current.path().join(release::release_notes_path("v1.2.3"));
        let component_notes = component.path().join(release::release_notes_path("v1.2.3"));
        for notes in [&cwd_notes, &component_notes] {
            std::fs::create_dir_all(notes.parent().unwrap()).expect("build dir");
            std::fs::write(notes, "exact body").expect("canonical notes");
        }

        assert_eq!(
            select_artifact_source_authority_release_notes(
                None,
                Some(current.path()),
                Some(component.path()),
                "v1.2.3",
            ),
            Some(cwd_notes)
        );
    }

    #[test]
    fn artifact_source_authority_explicit_notes_override_canonical_notes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let explicit = temp.path().join("provided.md");
        let canonical = temp.path().join(release::release_notes_path("v1.2.3"));
        std::fs::create_dir_all(canonical.parent().unwrap()).expect("build dir");
        std::fs::write(&canonical, "canonical body").expect("canonical notes");

        assert_eq!(
            select_artifact_source_authority_release_notes(
                Some(explicit.to_str().unwrap()),
                Some(temp.path()),
                Some(temp.path()),
                "v1.2.3",
            ),
            Some(explicit)
        );
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

    fn skip_args(skip_checks: Option<Vec<&str>>) -> ReleaseExecuteArgs {
        ReleaseExecuteArgs {
            components: vec!["fixture".to_string()],
            project: None,
            outdated: false,
            path: None,
            preflight_runner: None,
            preflight_placement: ReleasePreflightPlacementArg::Local,
            dry_run_args: DryRunArgs { dry_run: true },
            full: false,
            apply: false,
            deploy: false,
            recover: false,
            owner_run_ref: None,
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
            i_know_ci_creates_the_github_release: false,
            i_know_this_is_a_manual_tag_only_release: false,
            git_identity: None,
            cascade: false,
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

        let err = match run_package_only(args, &["fixture".to_string()], None) {
            Ok(_) => panic!("package-only requires an explicit tag"),
            Err(err) => err,
        };

        assert_eq!(err.code.as_str(), "validation.missing_argument");
        assert_eq!(err.details["args"][0], "--tag <existing-release-tag>");
    }

    #[test]
    fn package_only_accepts_head_but_rejects_publish_modes() {
        let mut args = args(&["fixture"]);
        args.dry_run_args.dry_run = false;
        args.package_only = true;
        args.apply = true;
        args.head = true;
        args.tag = Some("v1.2.3".to_string());

        validate_package_only_args(&args)
            .expect("--head confirms package-only recovery against the tagged HEAD");

        args.deploy = true;
        let err = validate_package_only_args(&args).expect_err("deploy remains mutually exclusive");

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
    fn no_github_release_guard_skips_when_flag_absent() {
        let mut args = args(&["fixture"]);
        args.dry_run_args.dry_run = false;
        // no_github_release stays false; guard is a no-op even for unknown ids.
        guard_no_github_release(&args, &["fixture".to_string()])
            .expect("guard is inert without --no-github-release");
    }

    #[test]
    fn normal_manual_release_creates_github_release_by_default() {
        let args = args(&["fixture"]);

        assert!(!args.no_github_release);
    }

    fn github_component(local_path: &std::path::Path) -> component::Component {
        let mut component = component::Component::new(
            "fixture".to_string(),
            local_path.to_string_lossy().into_owned(),
            String::new(),
            None,
        );
        component.remote_url = Some("https://github.com/Extra-Chill/fixture.git".to_string());
        component
    }

    #[test]
    fn no_github_release_ci_confirmation_requires_evidence() {
        let mut args = args(&["fixture"]);
        args.dry_run_args.dry_run = false;
        args.no_github_release = true;
        args.i_know_ci_creates_the_github_release = true;
        let temp = tempfile::tempdir().expect("tempdir");
        let component = github_component(temp.path());

        let err = no_github_release_guard_for_components(&args, vec![("fixture", component)])
            .expect_err("CI-owned confirmation requires evidence");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err
            .message
            .contains("requires machine-checkable evidence that CI creates the GitHub Release"));
        assert!(err.message.contains("Drop --no-github-release"));
    }

    #[test]
    fn no_github_release_guard_passes_with_configured_ci_ownership() {
        let mut args = args(&["fixture"]);
        args.dry_run_args.dry_run = false;
        args.no_github_release = true;
        args.i_know_ci_creates_the_github_release = true;
        let temp = tempfile::tempdir().expect("tempdir");
        let mut component = github_component(temp.path());
        component.release.github_release = Some(component::ComponentGithubReleaseConfig {
            owner: Some(component::GithubReleaseOwner::Ci),
        });

        no_github_release_guard_for_components(&args, vec![("fixture", component)])
            .expect("configured CI ownership satisfies the guard");
    }

    #[test]
    fn no_github_release_guard_passes_with_detected_ci_ownership() {
        let mut args = args(&["fixture"]);
        args.dry_run_args.dry_run = false;
        args.no_github_release = true;
        args.i_know_ci_creates_the_github_release = true;
        let temp = tempfile::tempdir().expect("tempdir");
        let workflows = temp.path().join(".github").join("workflows");
        std::fs::create_dir_all(&workflows).expect("workflow dir");
        std::fs::write(
            workflows.join("release.yml"),
            r#"
jobs:
  prepare:
    steps:
      - uses: Extra-Chill/homeboy-action@v2
        with:
          release-skip-github-release: 'true'
  host:
    steps:
      - uses: Extra-Chill/homeboy-action@v2
        with:
          release-head: 'true'
          release-from-artifacts: artifacts
"#,
        )
        .expect("workflow fixture");
        let component = github_component(temp.path());

        no_github_release_guard_for_components(&args, vec![("fixture", component)])
            .expect("detected CI ownership satisfies the guard");
    }

    #[test]
    fn no_github_release_guard_passes_with_manual_tag_only_confirmation() {
        let mut args = args(&["fixture"]);
        args.dry_run_args.dry_run = false;
        args.no_github_release = true;
        args.i_know_this_is_a_manual_tag_only_release = true;
        let temp = tempfile::tempdir().expect("tempdir");
        let component = github_component(temp.path());

        no_github_release_guard_for_components(&args, vec![("fixture", component)]).expect(
            "manual tag-only confirmation satisfies the guard without implying CI ownership",
        );
    }

    #[test]
    fn no_github_release_guard_exempts_dry_run() {
        let mut args = args(&["fixture"]);
        // dry_run defaults to true via the helper.
        args.no_github_release = true;
        guard_no_github_release(&args, &["fixture".to_string()])
            .expect("dry-run preview is never blocked by the guard");
    }

    #[test]
    fn no_github_release_guard_ignores_unresolvable_components() {
        let mut args = args(&["definitely-not-a-real-component-xyz"]);
        args.dry_run_args.dry_run = false;
        args.no_github_release = true;
        // A component that cannot be loaded (or is non-GitHub) is not gated;
        // the downstream release flow surfaces load failures with full context.
        guard_no_github_release(&args, &["definitely-not-a-real-component-xyz".to_string()])
            .expect("unresolvable components are not gated by the GitHub-release guard");
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

    #[test]
    fn recovered_git_state_surfaces_publication_continuation_in_json_envelope() {
        let continuation = "homeboy release fixture --head --skip-checks --apply".to_string();
        let result = ReleaseCommandResult {
            component_id: "fixture".to_string(),
            status: "git_recovered".to_string(),
            phase: ReleasePhase::Recover,
            bump_type: "recover".to_string(),
            dry_run: false,
            releasable_commits: 0,
            new_version: None,
            tag: Some("v1.2.3".to_string()),
            skipped_reason: None,
            plan: None,
            run: None,
            deployment: None,
            continuation_command: Some(continuation.clone()),
            release_summary: vec!["Git state recovered; publication is incomplete".to_string()],
            readiness: None,
        };
        let output = ReleaseOutput {
            variant: "single",
            actionable: release_actionable_metadata(&result),
            result,
            workspace: None,
            cascade: None,
        };
        let data = serde_json::to_value(output).expect("serialize release result");
        let response = crate::commands::utils::response::cli_response_for_json_result_for_command(
            &Ok(data),
            4,
            "release",
            None,
        );
        let value = serde_json::to_value(response).expect("serialize command envelope");

        assert!(!value["success"].as_bool().expect("success boolean"));
        assert_eq!(value["data"]["result"]["status"], "git_recovered");
        assert_eq!(value["next_actions"][0]["command"], continuation);
    }

    #[test]
    fn portable_child_projection_retains_remote_runner_and_durable_evidence() {
        let child: PortableReviewChildEnvelope = serde_json::from_value(serde_json::json!({
            "success": true,
            "run": { "id": "review-run-7", "location": "lab-runner-7" },
            "refs": { "runs": [{ "id": "review-run-7" }] },
            "evidence": [{ "uri": "runner-artifact://lab-runner-7/review.json" }],
            "data": {
                "release_readiness": {
                    "requested_source_commit": "frozen-source",
                    "source_commit": "frozen-source",
                    "runner_id": "lab-runner-7",
                    "provenance": {
                        "dependencies": { "fixture-dependency": "child-locked-sha" },
                        "extensions": { "fixture-extension": "sha256:child-manifest" }
                    }
                }
            }
        }))
        .expect("stable child result");

        let projected = child.project(true, "frozen-source").expect("valid child");

        assert!(projected.passed);
        assert_eq!(projected.runner_id.as_deref(), Some("lab-runner-7"));
        assert_eq!(
            projected.provenance.dependencies["fixture-dependency"],
            "child-locked-sha"
        );
        assert_eq!(
            projected.provenance.extensions["fixture-extension"],
            "sha256:child-manifest"
        );
        assert_eq!(
            projected.evidence_refs,
            vec![
                "run://review-run-7".to_string(),
                "runner-artifact://lab-runner-7/review.json".to_string(),
            ]
        );
    }

    fn valid_child_evidence() -> PortableChildReadinessEvidence {
        PortableChildReadinessEvidence {
            requested_source_commit: "frozen-source".to_string(),
            source_commit: "frozen-source".to_string(),
            runner_id: Some("lab-runner".to_string()),
            provenance: ReleaseReadinessProvenance {
                dependencies: std::collections::BTreeMap::from([(
                    "dependency".to_string(),
                    "locked-sha".to_string(),
                )]),
                extensions: Default::default(),
            },
        }
    }

    #[test]
    fn portable_child_success_rejects_missing_source_commit() {
        let mut child = valid_child_evidence();
        child.source_commit.clear();
        assert!(
            validate_portable_child_success(&child, "frozen-source", &["run://1".to_string()])
                .is_err()
        );
    }

    #[test]
    fn portable_child_success_rejects_mismatched_source_commit() {
        let mut child = valid_child_evidence();
        child.source_commit = "other-source".to_string();
        assert!(
            validate_portable_child_success(&child, "frozen-source", &["run://1".to_string()])
                .is_err()
        );
    }

    #[test]
    fn portable_child_success_rejects_missing_runner_or_durable_evidence() {
        let mut child = valid_child_evidence();
        child.runner_id = None;
        assert!(
            validate_portable_child_success(&child, "frozen-source", &["run://1".to_string()])
                .is_err()
        );
        let child = valid_child_evidence();
        assert!(validate_portable_child_success(&child, "frozen-source", &[]).is_err());
    }

    #[test]
    fn portable_child_success_rejects_empty_provenance() {
        let mut child = valid_child_evidence();
        child.provenance = Default::default();
        assert!(
            validate_portable_child_success(&child, "frozen-source", &["run://1".to_string()])
                .is_err()
        );
    }

    #[test]
    fn readiness_show_resolves_operation_reference_for_operators() {
        homeboy::core::test_support::with_isolated_home(|_| {
            let record = release::operation_record::OperationRecord {
                owner_run_ref: "release-readiness-operator-test".to_string(),
                operation: "release_readiness".to_string(),
                subject: "fixture".to_string(),
                provider: "lab".to_string(),
                handle: "commit".to_string(),
                path: None,
                source_sha: "commit".to_string(),
                cleanup_policy: "retain".to_string(),
                lifecycle_state: "finalized".to_string(),
                terminal_disposition: Some("succeeded".to_string()),
                finalization_status: "completed".to_string(),
                finalization_lease: None,
                finalization_lease_started_ms: None,
                attempt_count: 1,
                continuation_evidence: Vec::new(),
                attributes: Default::default(),
            };
            test_store().create(&record).expect("record");
            let (output, exit_code) = run(ReleaseArgs {
                command: Some(ReleaseSubcommand::Readiness(ReleaseReadinessArgs {
                    command: ReleaseReadinessCommand::Show {
                        reference: format!("operation://{}", record.owner_run_ref),
                    },
                })),
                execute: args(&[]),
            })
            .expect("show readiness");
            assert_eq!(exit_code, 0);
            let ReleaseCommandOutput::ReadinessShow(output) = output else {
                panic!("expected readiness show output");
            };
            assert_eq!(output.record.owner_run_ref, record.owner_run_ref);
        });
    }

    #[test]
    fn readiness_show_rejects_non_readiness_operation_records() {
        homeboy::core::test_support::with_isolated_home(|_| {
            let record = release::operation_record::OperationRecord {
                owner_run_ref: "not-readiness".to_string(),
                operation: "provider_workspace".to_string(),
                subject: "fixture".to_string(),
                provider: "lab".to_string(),
                handle: "commit".to_string(),
                path: None,
                source_sha: "commit".to_string(),
                cleanup_policy: "retain".to_string(),
                lifecycle_state: "finalized".to_string(),
                terminal_disposition: Some("succeeded".to_string()),
                finalization_status: "completed".to_string(),
                finalization_lease: None,
                finalization_lease_started_ms: None,
                attempt_count: 1,
                continuation_evidence: Vec::new(),
                attributes: Default::default(),
            };
            test_store().create(&record).expect("record");
            let error = match run(ReleaseArgs {
                command: Some(ReleaseSubcommand::Readiness(ReleaseReadinessArgs {
                    command: ReleaseReadinessCommand::Show {
                        reference: record.owner_run_ref,
                    },
                })),
                execute: args(&[]),
            }) {
                Ok(_) => panic!("non-readiness records are rejected"),
                Err(error) => error,
            };
            assert_eq!(error.code.as_str(), "validation.invalid_argument");
        });
    }

    struct RecordingPortableDispatcher {
        calls: RefCell<Vec<String>>,
        failing_gate: Option<&'static str>,
    }

    impl PortableStageDispatcher for RecordingPortableDispatcher {
        fn dispatch(
            &self,
            request: PortableStageRequest<'_>,
        ) -> homeboy::core::Result<PortableStageChildResult> {
            self.calls.borrow_mut().push(request.gate.to_string());
            if self.failing_gate == Some(request.gate) {
                return Err(homeboy::core::Error::internal_unexpected(format!(
                    "{} dispatcher unavailable",
                    request.gate
                )));
            }
            Ok(PortableStageChildResult {
                passed: true,
                runner_id: Some("test-runner".to_string()),
                evidence_refs: vec![format!("evidence://{}", request.gate)],
                provenance: ReleaseReadinessProvenance {
                    dependencies: std::collections::BTreeMap::from([(
                        "fixture".to_string(),
                        "locked".to_string(),
                    )]),
                    extensions: Default::default(),
                },
            })
        }
    }

    fn portable_preflight_fixture() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        let run = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(temp.path())
                .output()
                .expect("run git");
            assert!(output.status.success(), "git {:?} failed", args);
        };
        run(&["init", "-q", "--initial-branch", "main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(
            temp.path().join("homeboy.json"),
            r#"{
                "id": "fixture",
                "components": { "fixture": { "type": "fixture", "path": "." } }
            }"#,
        )
        .expect("write config");
        std::fs::write(temp.path().join("README.md"), "fixture\n").expect("write fixture");
        run(&["add", "."]);
        run(&["commit", "-qm", "initial"]);
        temp
    }

    fn portable_preflight_args(path: &std::path::Path) -> ReleaseExecuteArgs {
        let mut args = args(&["fixture"]);
        args.path = Some(path.to_string_lossy().to_string());
        args.preflight_placement = ReleasePreflightPlacementArg::Lab;
        args
    }

    #[test]
    fn portable_preflight_does_not_dispatch_skipped_stages() {
        let temp = portable_preflight_fixture();
        let dispatcher = RecordingPortableDispatcher {
            calls: RefCell::new(Vec::new()),
            failing_gate: None,
        };

        let readiness = run_portable_preflight_with(
            &dispatcher,
            &portable_preflight_args(temp.path()),
            "fixture",
            false,
            &["lint".to_string()],
        )
        .expect("preflight should complete")
        .expect("lab preflight is enabled");

        assert_eq!(*dispatcher.calls.borrow(), vec!["audit", "test"]);
        assert_eq!(readiness.gate_results[1].gate, "lint");
        assert_eq!(readiness.gate_results[1].status, "skipped");
        assert!(release::readiness_is_valid(&readiness));
    }

    #[test]
    fn portable_dispatch_error_is_a_failed_gate_and_later_gates_run() {
        let temp = portable_preflight_fixture();
        let dispatcher = RecordingPortableDispatcher {
            calls: RefCell::new(Vec::new()),
            failing_gate: Some("lint"),
        };

        let readiness = run_portable_preflight_with(
            &dispatcher,
            &portable_preflight_args(temp.path()),
            "fixture",
            false,
            &[],
        )
        .expect("dispatch failure is retained as a gate result")
        .expect("lab preflight is enabled");

        assert_eq!(*dispatcher.calls.borrow(), vec!["audit", "lint", "test"]);
        let lint = readiness
            .gate_results
            .iter()
            .find(|gate| gate.gate == "lint")
            .expect("lint result");
        assert_eq!(lint.status, "failed");
        assert!(lint
            .reason
            .as_deref()
            .expect("failure reason")
            .contains("dispatcher unavailable"));
        assert!(readiness
            .gate_results
            .iter()
            .any(|gate| gate.gate == "test" && gate.status == "passed"));
    }

    #[test]
    fn portable_preflight_bare_skip_checks_dispatches_no_remote_gates() {
        let temp = portable_preflight_fixture();
        let dispatcher = RecordingPortableDispatcher {
            calls: RefCell::new(Vec::new()),
            failing_gate: None,
        };

        let readiness = run_portable_preflight_with(
            &dispatcher,
            &portable_preflight_args(temp.path()),
            "fixture",
            true,
            &[],
        )
        .expect("preflight should complete")
        .expect("lab preflight is enabled");

        assert!(dispatcher.calls.borrow().is_empty());
        assert!(readiness
            .gate_results
            .iter()
            .filter(|gate| ["audit", "lint", "test"].contains(&gate.gate.as_str()))
            .all(|gate| gate.status == "skipped"));
    }

    #[test]
    fn portable_preflight_retains_child_provenance_not_controller_state() {
        let temp = portable_preflight_fixture();
        let child_provenance = ReleaseReadinessProvenance {
            dependencies: std::collections::BTreeMap::from([(
                "fixture-dependency".to_string(),
                "child-locked-sha".to_string(),
            )]),
            extensions: std::collections::BTreeMap::from([(
                "fixture-extension".to_string(),
                "sha256:child-manifest".to_string(),
            )]),
        };
        struct ProvenanceDispatcher(ReleaseReadinessProvenance);
        impl PortableStageDispatcher for ProvenanceDispatcher {
            fn dispatch(
                &self,
                _request: PortableStageRequest<'_>,
            ) -> homeboy::core::Result<PortableStageChildResult> {
                Ok(PortableStageChildResult {
                    passed: true,
                    runner_id: Some("lab".to_string()),
                    evidence_refs: Vec::new(),
                    provenance: self.0.clone(),
                })
            }
        }

        let readiness = run_portable_preflight_with(
            &ProvenanceDispatcher(child_provenance.clone()),
            &portable_preflight_args(temp.path()),
            "fixture",
            false,
            &[],
        )
        .expect("preflight should complete")
        .expect("lab preflight is enabled");

        let controller_component = component::resolve_effective(
            Some("fixture"),
            Some(temp.path().to_string_lossy().as_ref()),
            None,
        )
        .expect("controller component");
        let controller_provenance =
            release::readiness_provenance(&controller_component).expect("controller provenance");
        assert_ne!(controller_provenance, child_provenance);
        assert_eq!(readiness.provenance, child_provenance);
        assert!(readiness
            .gate_results
            .iter()
            .filter(|gate| ["audit", "lint", "test"].contains(&gate.gate.as_str()))
            .all(|gate| gate.provenance.as_ref() == Some(&child_provenance)));
    }
}
