//! Review command — scoped audit + lint + test umbrella.
//!
//! `homeboy review --changed-since=<ref>` runs the same scoped checks a CI
//! reviewer would run on a PR diff, fanning out to the existing
//! `audit`, `lint`, and `test` commands and collapsing their structured
//! results into a single consolidated report.
//!
//! The umbrella is deliberately thin: scoping logic lives in the underlying
//! commands (and in `core/git/changes.rs::get_files_changed_since`). Review
//! orchestrates ordering, short-circuits on empty changesets, and assembles
//! the consolidated output envelope.
//!
//! See: https://github.com/Extra-Chill/homeboy/issues/1500

use clap::Args;
use homeboy::core::ci_profile::{self, CiRunSelection};
use homeboy::core::code_audit::AuditCommandOutput;
use homeboy::core::engine::execution_context::{self, ResolveOptions};
use homeboy::core::extension::lint::LintCommandOutput;
use homeboy::core::extension::test::TestCommandOutput;
use homeboy::core::git;
use homeboy::core::plan::PlanStep;
use homeboy::core::quality::{build_quality_plan, QualityPlanOptions};
use homeboy::core::review::{
    self, ReviewArtifactFindings, ReviewCommandOutput, ReviewOutputInput, ReviewService,
    ReviewStage, ReviewStages,
};
use serde::Serialize;
use serde_json::Value;

use super::parse_key_val;
use super::utils::args::{BaselineArgs, ExtensionOverrideArgs, PositionalComponentArgs};
use super::utils::output::{write_output_file_atomically, OutputWriteOptions};
use super::{audit, lint, test, CmdResult, GlobalArgs};

mod observation;
pub(super) mod raw_output;

#[derive(Args, Debug, Clone)]
pub struct ReviewArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

    #[command(flatten)]
    pub extension_override: ExtensionOverrideArgs,

    /// Run audit + lint + test only against files changed since this git ref
    /// (branch, tag, or SHA). CI-friendly — mirrors the per-stage flag.
    #[arg(long, value_name = "REF", conflicts_with = "changed_only")]
    pub changed_since: Option<String>,

    /// Run only against files modified in the working tree
    /// (staged, unstaged, untracked). Only the lint stage scopes natively;
    /// audit and test run on the full component with a hint noting the
    /// limitation. Use `--changed-since` for full umbrella scoping.
    #[arg(long, conflicts_with = "changed_since")]
    pub changed_only: bool,

    /// Show compact summary instead of full per-stage output
    #[arg(long)]
    pub summary: bool,

    /// Run an extension-declared CI profile as an additional review gate.
    #[arg(long, value_name = "ID")]
    pub ci_profile: Option<String>,

    /// Output format. Default JSON envelope; `--report=pr-comment` emits a
    /// markdown PR-comment section instead, suitable for piping to
    /// `homeboy git pr comment --body-file`.
    #[arg(long, value_name = "FORMAT", value_parser = ["pr-comment"])]
    pub report: Option<String>,

    /// Action-level banner rendered above the PR-comment scope line.
    /// Repeatable as `--banner key=value`.
    #[arg(long, value_name = "KEY=VALUE", value_parser = parse_key_val)]
    pub banner: Vec<(String, String)>,

    #[command(flatten)]
    pub baseline_args: BaselineArgs,
}

/// True when the caller asked for a markdown PR-comment section instead of
/// the structured JSON envelope. Used by the top-level dispatcher to route
/// the response through `RawOutputMode::Markdown`.
pub fn is_markdown_mode(args: &ReviewArgs) -> bool {
    args.report.as_deref() == Some("pr-comment")
}

struct ReviewStageDescriptor<Args, Output: Serialize + ReviewArtifactFindings> {
    name: &'static str,
    include_changed_only_scope: bool,
    build_args: fn(&ReviewArgs) -> Args,
    run: fn(Args, &GlobalArgs) -> CmdResult<Output>,
    finding_count: fn(&Output) -> usize,
}

enum ReviewStageRun {
    Audit(ReviewStage<AuditCommandOutput>),
    Lint(ReviewStage<LintCommandOutput>),
    Test(ReviewStage<TestCommandOutput>),
}

impl<Args, Output: Serialize + ReviewArtifactFindings> ReviewStageDescriptor<Args, Output> {
    fn execute(
        &self,
        review_args: &ReviewArgs,
        global: &GlobalArgs,
        component_label: &str,
    ) -> CmdResult<ReviewStage<Output>> {
        let (output, exit_code) = (self.run)((self.build_args)(review_args), global)?;
        let finding_count = (self.finding_count)(&output);

        Ok((
            ReviewStage {
                stage: self.name.to_string(),
                ran: true,
                passed: exit_code == 0,
                exit_code,
                finding_count,
                hint: format!(
                    "Deep dive: homeboy {} {}{}",
                    self.name,
                    component_label,
                    scope_flag_suffix(review_args, self.include_changed_only_scope),
                ),
                skipped_reason: None,
                output: Some(output),
            },
            exit_code,
        ))
    }
}

fn dispatch_review_plan_step(
    step: &PlanStep,
    args: &ReviewArgs,
    global: &GlobalArgs,
    component_label: &str,
) -> homeboy::core::Result<Option<ReviewStageRun>> {
    match step.kind.as_str() {
        "review.audit" => {
            let descriptor = ReviewStageDescriptor {
                name: "audit",
                include_changed_only_scope: false,
                build_args: build_audit_args,
                run: audit::run,
                finding_count: audit_finding_count,
            };
            let (stage, _) = descriptor.execute(args, global, component_label)?;
            Ok(Some(ReviewStageRun::Audit(stage)))
        }
        "review.lint" => {
            let descriptor = ReviewStageDescriptor {
                name: "lint",
                include_changed_only_scope: true,
                build_args: build_lint_args,
                run: lint::run,
                finding_count: lint_finding_count,
            };
            let (stage, _) = descriptor.execute(args, global, component_label)?;
            Ok(Some(ReviewStageRun::Lint(stage)))
        }
        "review.test" => {
            let descriptor = ReviewStageDescriptor {
                name: "test",
                include_changed_only_scope: false,
                build_args: build_test_args,
                run: test::run,
                finding_count: test_finding_count,
            };
            let (stage, _) = descriptor.execute(args, global, component_label)?;
            Ok(Some(ReviewStageRun::Test(stage)))
        }
        other => Err(homeboy::core::Error::internal_unexpected(format!(
            "review quality plan contains unsupported executable step '{other}'"
        ))),
    }
}

pub fn run(args: ReviewArgs, global: &GlobalArgs) -> CmdResult<ReviewCommandOutput> {
    // Resolve component ID (auto-discovers from CWD when omitted) and source
    // path so we can probe git for the changed-file set ourselves.
    let component = args.comp.load()?;
    let component_label = component.id.clone();
    let source_path = component.local_path.clone();

    let scope = if args.changed_since.is_some() {
        "changed-since"
    } else if args.changed_only {
        "changed-only"
    } else {
        "full"
    }
    .to_string();

    let quality_plan = build_quality_plan(QualityPlanOptions::review(&component_label));

    // Probe the changed set once at the umbrella level so we can short-circuit
    // before paying for any extension setup. Each stage will re-derive its
    // own scope internally (and that's fine — `get_files_changed_since` is
    // cheap, and lint/audit/test must remain independently invocable).
    let changed_file_count = match (&args.changed_since, args.changed_only) {
        (Some(git_ref), _) => Some(git::get_files_changed_since(&source_path, git_ref)?.len()),
        (_, true) => Some(git::get_dirty_files(&source_path)?.len()),
        _ => None,
    };

    if let Some(0) = changed_file_count {
        let scope_label = if let Some(ref r) = args.changed_since {
            format!("since {}", r)
        } else {
            "in working tree".to_string()
        };
        let message = format!("No files changed {} — skipping review", scope_label);
        println!("{}", message);

        let review_observation = observation::start(observation::ReviewObservationStart {
            component_id: &component.id,
            component_label: &component_label,
            source_path: Path::new(&source_path),
            args: &args,
            scope: &scope,
            changed_file_count: Some(0),
        });
        let observation_metadata = review_observation.as_ref().map(|o| o.output_metadata());

        let output = ReviewService::skipped_output(
            ReviewOutputInput {
                component: component_label.clone(),
                plan: build_quality_plan(QualityPlanOptions::skipped_review(
                    &component_label,
                    "no files changed",
                )),
                observation: observation_metadata,
                scope: scope.clone(),
                changed_since: args.changed_since.clone(),
                changed_file_count: Some(0),
                head_ref: review::git_ref(&source_path, "HEAD").unwrap_or_default(),
                hints: vec![message],
            },
            "no files changed",
            args.ci_profile.is_some(),
        );
        observation::finish_success(review_observation, &output, 0);
        return Ok((output, 0));
    }

    let review_observation = observation::start(observation::ReviewObservationStart {
        component_id: &component.id,
        component_label: &component_label,
        source_path: Path::new(&source_path),
        args: &args,
        scope: &scope,
        changed_file_count,
    });

    let mut top_hints: Vec<String> = Vec::new();

    let mut audit_stage = None;
    let mut lint_stage = None;
    let mut test_stage = None;

    let stage_run = match review::execute_review_plan_steps(&quality_plan.steps, |step| {
        dispatch_review_plan_step(step, &args, global, &component_label)
    }) {
        Ok(run) => run,
        Err(error) => {
            observation::finish_error(review_observation, &error);
            return Err(error);
        }
    };

    for stage_run in stage_run.results {
        match stage_run {
            ReviewStageRun::Audit(stage) => {
                audit_stage = Some(stage);
            }
            ReviewStageRun::Lint(stage) => {
                lint_stage = Some(stage);
            }
            ReviewStageRun::Test(stage) => {
                test_stage = Some(stage);
            }
        }
    }

    let audit_stage = audit_stage.expect("review quality plan must include audit stage");
    let lint_stage = lint_stage.expect("review quality plan must include lint stage");
    let test_stage = test_stage.expect("review quality plan must include test stage");
    let ci_profile_stage = match args.ci_profile.as_ref() {
        Some(profile) => {
            let ctx = execution_context::resolve(&ResolveOptions {
                component_id: args.comp.component.clone(),
                path_override: args.comp.path.clone(),
                capability: None,
                settings_overrides: Vec::new(),
                settings_json_overrides: Vec::new(),
                extension_overrides: args.extension_override.extensions.clone(),
            })?;
            let extension_ids = ctx
                .component
                .extensions
                .as_ref()
                .map(|extensions| {
                    let mut ids: Vec<String> = extensions.keys().cloned().collect();
                    ids.sort();
                    ids
                })
                .unwrap_or_default();
            let extension_id = ci_profile::select_extension_id(&extension_ids)?;
            let output = ci_profile::run_for_extension(
                &ctx.source_path,
                &extension_id,
                CiRunSelection::Profile(profile.clone()),
            )?;
            let exit_code = output.exit_code;
            let finding_count = output.jobs.iter().filter(|job| !job.success).count();
            let stage = ReviewStage {
                stage: "ci".to_string(),
                ran: true,
                passed: exit_code == 0,
                exit_code,
                finding_count,
                hint: format!(
                    "Deep dive: homeboy ci run {} --profile {}",
                    component_label, profile
                ),
                skipped_reason: None,
                output: Some(output),
            };
            Some(stage)
        }
        None => None,
    };

    if args.changed_only {
        top_hints.push(
            "--changed-only scopes lint only; audit and test ran on the full component".to_string(),
        );
    }

    let observation_metadata = review_observation.as_ref().map(|o| o.output_metadata());
    let (output, overall_exit) = ReviewService::output_from_stages(
        ReviewOutputInput {
            component: component_label.clone(),
            plan: quality_plan,
            observation: observation_metadata,
            scope,
            changed_since: args.changed_since.clone(),
            changed_file_count,
            head_ref: review::git_ref(&source_path, "HEAD").unwrap_or_default(),
            hints: top_hints,
        },
        ReviewStages {
            audit: audit_stage,
            lint: lint_stage,
            test: test_stage,
            ci_profile: ci_profile_stage,
        },
    );

    print_human_summary(&output);

    observation::finish_success(review_observation, &output, overall_exit);

    Ok((output, overall_exit))
}

/// Markdown output mode — runs the JSON path internally and renders the
/// envelope into a PR-comment section. The body is just the section content;
/// the consumer (`homeboy git pr comment --header`) owns the wrapping
/// section header.
pub fn run_markdown(args: ReviewArgs, global: &GlobalArgs) -> CmdResult<String> {
    let banners = args.banner.clone();
    let (output, exit_code) = run(args, global)?;
    let md = if banners.is_empty() {
        review::render::render_pr_comment(&output)
    } else {
        review::render::render_pr_comment_with_banners(&output, &banners)
    };
    Ok((md, exit_code))
}

/// Write the stable review artifact to `--output` for automated consumers.
/// Falls back to the generic JSON envelope if the review command failed before
/// producing an artifact.
pub fn write_artifact_to_file(
    result: &homeboy::core::Result<Value>,
    path: &str,
    _exit_code: i32,
) -> bool {
    let Ok(data) = result else {
        return false;
    };
    let Some(artifact) = data.get("artifact") else {
        return false;
    };

    let json = match serde_json::to_string_pretty(artifact) {
        Ok(j) => j,
        Err(e) => {
            eprintln!(
                "Warning: failed to serialize review artifact for --output: {}",
                e
            );
            return true;
        }
    };

    if let Err(e) = write_output_file_atomically(path, json, OutputWriteOptions::artifact()) {
        eprintln!("Warning: failed to write --output file '{}': {}", path, e);
    }
    true
}

fn scope_flag_suffix(args: &ReviewArgs, include_changed_only: bool) -> String {
    if let Some(ref r) = args.changed_since {
        format!(" --changed-since={}", r)
    } else if args.changed_only && include_changed_only {
        " --changed-only".to_string()
    } else {
        String::new()
    }
}

fn build_audit_args(args: &ReviewArgs) -> audit::AuditArgs {
    audit::AuditArgs {
        comp: args.comp.clone(),
        extension_override: args.extension_override.clone(),
        conventions: false,
        only: Vec::new(),
        exclude: Vec::new(),
        baseline_args: args.baseline_args.clone(),
        changed_since: args.changed_since.clone(),
        json_summary: args.summary,
        fixability: false,
    }
}

fn build_lint_args(args: &ReviewArgs) -> lint::LintArgs {
    lint::LintArgs {
        comp: args.comp.clone(),
        summary: args.summary,
        file: None,
        glob: None,
        changed_only: args.changed_only,
        changed_since: args.changed_since.clone(),
        ci_job: None,
        errors_only: false,
        sniffs: None,
        exclude_sniffs: None,
        category: None,
        fix: false,
        force: false,
        extension_override: args.extension_override.clone(),
        setting_args: Default::default(),
        baseline_args: args.baseline_args.clone(),
        json_summary: args.summary,
    }
}

fn build_test_args(args: &ReviewArgs) -> test::TestArgs {
    test::TestArgs {
        comp: args.comp.clone(),
        extension_override: args.extension_override.clone(),
        skip_lint: true,
        coverage: false,
        coverage_min: None,
        baseline_args: args.baseline_args.clone(),
        analyze: false,
        drift: false,
        write: false,
        since: "HEAD~10".to_string(),
        changed_since: args.changed_since.clone(),
        ci_job: None,
        setting_args: Default::default(),
        args: Vec::new(),
        json_summary: args.summary,
    }
}

fn audit_finding_count(output: &AuditCommandOutput) -> usize {
    match output {
        AuditCommandOutput::Full { result, .. } => result.findings.len(),
        AuditCommandOutput::Compared { result, .. } => result.findings.len(),
        AuditCommandOutput::Summary(summary) => summary.total_findings,
        AuditCommandOutput::BaselineSaved { findings_count, .. } => *findings_count,
        AuditCommandOutput::Conventions { .. } => 0,
    }
}

fn lint_finding_count(output: &LintCommandOutput) -> usize {
    output.findings.as_ref().map(|f| f.len()).unwrap_or(0)
}

fn test_finding_count(output: &TestCommandOutput) -> usize {
    output
        .test_counts
        .as_ref()
        .map(|c| c.failed as usize)
        .unwrap_or(0)
}

/// Print a compact human-readable summary to stderr so users running
/// `homeboy review` interactively see a skim-friendly report on top of the
/// JSON envelope. Mirrors the per-command stderr status hints.
fn print_human_summary(output: &ReviewCommandOutput) {
    use std::io::IsTerminal;
    if !std::io::stderr().is_terminal() {
        return;
    }

    eprintln!();
    eprintln!(
        "[review] {}: {} (component {}, scope {})",
        if output.summary.passed {
            "PASS"
        } else {
            "FAIL"
        },
        output.summary.status,
        output.summary.component,
        output.summary.scope,
    );
    print_stage_line(&output.audit);
    print_stage_line(&output.lint);
    print_stage_line(&output.test);
    if let Some(ref stage) = output.ci_profile {
        print_stage_line(stage);
    }
    for hint in &output.summary.hints {
        eprintln!("[review] hint: {}", hint);
    }
}

fn print_stage_line<T: Serialize>(stage: &ReviewStage<T>) {
    let marker = if !stage.ran {
        "skipped"
    } else if stage.passed {
        "passed"
    } else {
        "failed"
    };
    eprintln!(
        "[review]   {:<6} {:<7} findings={} exit={}",
        stage.stage, marker, stage.finding_count, stage.exit_code,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::utils::args::{BaselineArgs, PositionalComponentArgs};
    use clap::Parser;

    /// Minimal CLI wrapper to exercise clap parsing of `ReviewArgs`.
    #[derive(Parser, Debug)]
    struct TestCli {
        #[command(flatten)]
        review: ReviewArgs,
    }

    #[test]
    fn parses_changed_since() {
        let cli = TestCli::try_parse_from(["test", "my-comp", "--changed-since", "trunk"])
            .expect("should parse");
        assert_eq!(cli.review.changed_since.as_deref(), Some("trunk"));
        assert!(!cli.review.changed_only);
        assert_eq!(cli.review.comp.component.as_deref(), Some("my-comp"));
    }

    #[test]
    fn parses_one_shot_extension_override() {
        let cli = TestCli::try_parse_from([
            "test",
            "my-comp",
            "--extension",
            "fixture-review",
            "--changed-since",
            "origin/main",
        ])
        .expect("review should parse --extension override");

        assert_eq!(
            cli.review.extension_override.extensions,
            vec!["fixture-review"]
        );
        assert_eq!(cli.review.changed_since.as_deref(), Some("origin/main"));
    }

    #[test]
    fn parses_changed_only() {
        let cli = TestCli::try_parse_from(["test", "--changed-only"]).expect("should parse");
        assert!(cli.review.changed_only);
        assert!(cli.review.changed_since.is_none());
    }

    #[test]
    fn parses_report_pr_comment() {
        let cli = TestCli::try_parse_from(["test", "my-comp", "--report=pr-comment"])
            .expect("should parse");
        assert_eq!(cli.review.report.as_deref(), Some("pr-comment"));
        assert!(is_markdown_mode(&cli.review));
    }

    #[test]
    fn parses_repeatable_pr_comment_banners_in_order() {
        let cli = TestCli::try_parse_from([
            "test",
            "my-comp",
            "--report=pr-comment",
            "--banner",
            "autofix=applied 3 file(s)",
            "--banner=binary-source=fallback",
            "--banner",
            "custom=value=with=equals",
        ])
        .expect("should parse repeatable banners");

        assert_eq!(
            cli.review.banner,
            vec![
                ("autofix".to_string(), "applied 3 file(s)".to_string()),
                ("binary-source".to_string(), "fallback".to_string()),
                ("custom".to_string(), "value=with=equals".to_string()),
            ]
        );
    }

    #[test]
    fn rejects_unknown_report_format() {
        let result = TestCli::try_parse_from(["test", "my-comp", "--report=slack"]);
        assert!(
            result.is_err(),
            "clap whitelist must reject unknown report formats"
        );
    }

    #[test]
    fn is_markdown_mode_false_without_flag() {
        let cli = TestCli::try_parse_from(["test", "my-comp"]).expect("should parse");
        assert!(!is_markdown_mode(&cli.review));
    }

    #[test]
    fn parses_with_no_component() {
        let cli = TestCli::try_parse_from(["test", "--changed-since", "main"])
            .expect("should parse without positional component");
        assert!(cli.review.comp.component.is_none());
    }

    #[test]
    fn rejects_changed_since_with_changed_only() {
        let result =
            TestCli::try_parse_from(["test", "--changed-since", "trunk", "--changed-only"]);
        assert!(result.is_err(), "clap must reject conflicting scope flags");
    }

    #[test]
    fn rejects_changed_only_with_changed_since() {
        let result =
            TestCli::try_parse_from(["test", "--changed-only", "--changed-since", "trunk"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_summary_and_baseline_flags() {
        let cli = TestCli::try_parse_from([
            "test",
            "my-comp",
            "--changed-since=trunk",
            "--summary",
            "--ignore-baseline",
        ])
        .expect("should parse");
        assert!(cli.review.summary);
        assert!(cli.review.baseline_args.ignore_baseline);
    }

    #[test]
    fn parses_ci_profile() {
        let cli = TestCli::try_parse_from(["test", "my-comp", "--ci-profile", "pr"])
            .expect("should parse ci profile");

        assert_eq!(cli.review.ci_profile.as_deref(), Some("pr"));
    }

    #[test]
    fn unsupported_review_plan_step_returns_consistent_error() {
        let args = review_args_fixture();
        let global = GlobalArgs {};
        let step = PlanStep::ready("review.unknown", "review.unknown").build();

        let err = match dispatch_review_plan_step(&step, &args, &global, "fixture") {
            Ok(_) => panic!("unsupported executable review step should fail"),
            Err(err) => err,
        };

        assert!(err
            .to_string()
            .contains("unsupported executable step 'review.unknown'"));
    }

    #[test]
    fn write_artifact_to_file_writes_direct_artifact_and_creates_parent_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("homeboy-ci-results").join("review.json");
        let result = Ok(serde_json::json!({
            "command": "review",
            "artifact": {
                "schema": "homeboy/review/v1",
                "component": "homeboy",
                "status": "passed",
                "generated_at": "2026-04-28T00:00:00Z",
                "base_ref": "origin/main",
                "head_ref": "abc123",
                "commands": []
            }
        }));

        assert!(write_artifact_to_file(
            &result,
            path.to_str().expect("utf8 path"),
            0
        ));

        let written = std::fs::read_to_string(path).expect("artifact written");
        let json: serde_json::Value = serde_json::from_str(&written).expect("valid json");
        assert_eq!(json["schema"], "homeboy/review/v1");
        assert!(
            json.get("success").is_none(),
            "artifact is not CLI envelope"
        );
    }

    #[test]
    fn write_artifact_to_file_preserves_observation_pointer_when_available() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("review.json");
        let result = Ok(serde_json::json!({
            "command": "review",
            "observation": {
                "schema": "homeboy/observation-pointer/v1",
                "run_id": "run-123",
                "kind": "review",
                "details": {
                    "query": "homeboy runs show run-123",
                    "artifacts": "homeboy runs artifacts run-123",
                    "export_bundle": "homeboy runs export --run run-123 --output ~/.local/share/homeboy/exports/run-123"
                }
            },
            "artifact": {
                "schema": "homeboy/review/v1",
                "component": "homeboy",
                "status": "passed",
                "generated_at": "2026-04-28T00:00:00Z",
                "base_ref": "origin/main",
                "head_ref": "abc123",
                "observation": {
                    "schema": "homeboy/observation-pointer/v1",
                    "run_id": "run-123",
                    "kind": "review",
                    "details": {
                        "query": "homeboy runs show run-123",
                        "artifacts": "homeboy runs artifacts run-123",
                        "export_bundle": "homeboy runs export --run run-123 --output ~/.local/share/homeboy/exports/run-123"
                    }
                },
                "commands": []
            }
        }));

        assert!(write_artifact_to_file(
            &result,
            path.to_str().expect("utf8 path"),
            0
        ));

        let written = std::fs::read_to_string(path).expect("artifact written");
        let json: serde_json::Value = serde_json::from_str(&written).expect("valid json");
        assert_eq!(json["schema"], "homeboy/review/v1");
        assert_eq!(json["observation"]["run_id"], "run-123");
        assert_eq!(
            json["observation"]["details"]["query"],
            "homeboy runs show run-123"
        );
        assert!(
            json.get("success").is_none(),
            "artifact remains the direct review artifact, not the CLI envelope"
        );
    }

    #[test]
    fn scope_flag_suffix_renders_changed_since() {
        let args = ReviewArgs {
            comp: PositionalComponentArgs {
                component: None,
                path: None,
            },
            extension_override: ExtensionOverrideArgs::default(),
            changed_since: Some("trunk".to_string()),
            changed_only: false,
            summary: false,
            ci_profile: None,
            report: None,
            banner: Vec::new(),
            baseline_args: BaselineArgs::default(),
        };
        assert_eq!(scope_flag_suffix(&args, true), " --changed-since=trunk");
        assert_eq!(scope_flag_suffix(&args, false), " --changed-since=trunk");
    }

    #[test]
    fn scope_flag_suffix_renders_changed_only_only_when_allowed() {
        let args = ReviewArgs {
            comp: PositionalComponentArgs {
                component: None,
                path: None,
            },
            extension_override: ExtensionOverrideArgs::default(),
            changed_since: None,
            changed_only: true,
            summary: false,
            ci_profile: None,
            report: None,
            banner: Vec::new(),
            baseline_args: BaselineArgs::default(),
        };
        assert_eq!(scope_flag_suffix(&args, true), " --changed-only");
        // audit/test do not support --changed-only, so the suffix is empty
        // when the caller requests it not be included.
        assert_eq!(scope_flag_suffix(&args, false), "");
    }

    #[test]
    fn scope_flag_suffix_empty_for_full_run() {
        let args = ReviewArgs {
            comp: PositionalComponentArgs {
                component: None,
                path: None,
            },
            extension_override: ExtensionOverrideArgs::default(),
            changed_since: None,
            changed_only: false,
            summary: false,
            ci_profile: None,
            report: None,
            banner: Vec::new(),
            baseline_args: BaselineArgs::default(),
        };
        assert_eq!(scope_flag_suffix(&args, true), "");
        assert_eq!(scope_flag_suffix(&args, false), "");
    }

    fn review_args_fixture() -> ReviewArgs {
        ReviewArgs {
            comp: PositionalComponentArgs {
                component: Some("fixture".to_string()),
                path: None,
            },
            extension_override: ExtensionOverrideArgs::default(),
            changed_since: None,
            changed_only: false,
            summary: false,
            ci_profile: None,
            report: None,
            banner: Vec::new(),
            baseline_args: BaselineArgs::default(),
        }
    }
}
