use std::path::{Path, PathBuf};

use homeboy::core::component;
use homeboy::core::extension::trace as extension_trace;
use homeboy::core::extension::trace::TraceCommandOutput;
use homeboy::core::git;

use super::matrix::{aggregate_to_compare_input, write_json_artifact};
use super::output::compare_trace_aggregates_with_focus;
use super::{apply_command_target_component, run_repeat, TraceArgs};
use crate::commands::CmdResult;

pub(super) fn run_compare_targets(args: TraceArgs) -> CmdResult<TraceCommandOutput> {
    if args.keep_overlay {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "--keep-overlay",
            "trace compare runs baseline and candidate in separate target checkouts; overlays must be reverted after each run",
            None,
            None,
        ));
    }

    let baseline = required_target(args.baseline_target.as_deref(), "--baseline-target")?;
    let candidate = required_target(args.candidate.as_deref(), "--candidate")?;
    let component_id = args
        .component_arg
        .as_deref()
        .or(args.scenario.as_deref())
        .ok_or_else(|| {
            homeboy::core::Error::validation_missing_argument(vec!["component".to_string()])
        })?
        .to_string();
    let scenario_id = args
        .scenario_arg
        .clone()
        .or_else(|| {
            args.compare_after
                .as_ref()
                .map(|path| path.to_string_lossy().to_string())
        })
        .ok_or_else(|| {
            homeboy::core::Error::validation_missing_argument(vec!["trace scenario".to_string()])
        })?;
    let base_component =
        component::resolve_effective(Some(&component_id), args.comp.path.as_deref(), None)?;

    let output_dir = args.output_dir.clone().unwrap_or_else(|| {
        PathBuf::from(".homeboy")
            .join("trace-compare")
            .join(format!(
                "{}-{}",
                scenario_id,
                chrono::Utc::now().format("%Y%m%d%H%M%S")
            ))
    });
    std::fs::create_dir_all(&output_dir).map_err(|err| {
        homeboy::core::Error::internal_io(
            format!(
                "Failed to create trace compare output dir {}: {}",
                output_dir.display(),
                err
            ),
            Some("trace.compare.output_dir".to_string()),
        )
    })?;

    let baseline_target = resolve_target("baseline", baseline, &base_component.local_path)?;
    let candidate_target = resolve_target("candidate", candidate, &base_component.local_path)?;
    let baseline_aggregate =
        run_target_aggregate(&args, &component_id, &scenario_id, &baseline_target.path)?;
    let candidate_aggregate =
        run_target_aggregate(&args, &component_id, &scenario_id, &candidate_target.path)?;

    let baseline_path = output_dir.join("baseline.aggregate.json");
    let candidate_path = output_dir.join("candidate.aggregate.json");
    let compare_path = output_dir.join("compare.json");
    let summary_path = output_dir.join("summary.md");
    write_json_artifact(&baseline_path, &baseline_aggregate)?;
    write_json_artifact(&candidate_path, &candidate_aggregate)?;

    let mut compare = compare_trace_aggregates_with_focus(
        &baseline_path,
        aggregate_to_compare_input(&baseline_aggregate),
        &candidate_path,
        aggregate_to_compare_input(&candidate_aggregate),
        &args.focus_spans,
        args.regression_threshold,
        args.regression_min_delta_ms,
    );
    compare.before_target = Some(baseline_target.input);
    compare.after_target = Some(candidate_target.input);
    compare.before_git_sha = baseline_target.git_sha;
    compare.after_git_sha = candidate_target.git_sha;
    compare.before_status = Some(baseline_aggregate.status.clone());
    compare.after_status = Some(candidate_aggregate.status.clone());
    compare.before_exit_code = Some(baseline_aggregate.exit_code);
    compare.after_exit_code = Some(candidate_aggregate.exit_code);
    compare.output_dir = Some(output_dir.to_string_lossy().to_string());
    compare.summary_path = Some(summary_path.to_string_lossy().to_string());
    write_json_artifact(&compare_path, &compare)?;
    std::fs::write(
        &summary_path,
        super::output::render_compare_markdown(&compare),
    )
    .map_err(|err| {
        homeboy::core::Error::internal_io(
            format!(
                "Failed to write trace compare summary {}: {}",
                summary_path.display(),
                err
            ),
            Some("trace.compare.summary".to_string()),
        )
    })?;

    let failed = !baseline_aggregate.passed
        || !candidate_aggregate.passed
        || compare.focus_status.as_deref() == Some("fail")
        || compare.guardrail_status.as_deref() == Some("fail");
    Ok((
        TraceCommandOutput::Compare(compare),
        if failed { 1 } else { 0 },
    ))
}

fn required_target<'a>(
    value: Option<&'a str>,
    name: &'static str,
) -> homeboy::core::Result<&'a str> {
    value.ok_or_else(|| homeboy::core::Error::validation_missing_argument(vec![name.to_string()]))
}

fn run_target_aggregate(
    args: &TraceArgs,
    component_id: &str,
    scenario_id: &str,
    path: &Path,
) -> homeboy::core::Result<extension_trace::TraceAggregateOutput> {
    let mut run_args = args.clone();
    apply_command_target_component(&mut run_args);
    run_args.comp.component = Some(component_id.to_string());
    run_args.comp.path = Some(path.to_string_lossy().to_string());
    run_args.scenario = Some(scenario_id.to_string());
    run_args.compare_after = None;
    run_args.baseline_target = None;
    run_args.candidate = None;
    run_args.repeat = args.repeat.max(1);
    run_args.aggregate = Some("spans".to_string());
    run_args.output_dir = None;

    match run_repeat(run_args)?.0 {
        TraceCommandOutput::Aggregate(output) => Ok(output),
        _ => unreachable!("run_repeat returns aggregate output"),
    }
}

struct ResolvedCompareTarget {
    input: String,
    path: PathBuf,
    git_sha: Option<String>,
    _worktree: Option<TemporaryGitWorktree>,
}

fn resolve_target(
    role: &'static str,
    input: &str,
    source_path: &str,
) -> homeboy::core::Result<ResolvedCompareTarget> {
    let input_path = PathBuf::from(input);
    if input_path.exists() {
        let path = input_path.canonicalize().map_err(|err| {
            homeboy::core::Error::internal_io(
                format!("Failed to resolve {} path {}: {}", role, input, err),
                Some("trace.compare.path".to_string()),
            )
        })?;
        let git_sha = git::short_head_revision_at(&path);
        return Ok(ResolvedCompareTarget {
            input: input.to_string(),
            path,
            git_sha,
            _worktree: None,
        });
    }

    let source_root = git::get_git_root(source_path)?;
    let source_root = PathBuf::from(source_root);
    let component_prefix = git::get_component_path_prefix(source_path);
    let worktree = TemporaryGitWorktree::add(role, &source_root, input)?;
    let path = component_prefix
        .as_deref()
        .map(|prefix| worktree.path.join(prefix))
        .unwrap_or_else(|| worktree.path.clone());
    let git_sha = git::short_head_revision_at(&path);
    Ok(ResolvedCompareTarget {
        input: input.to_string(),
        path,
        git_sha,
        _worktree: Some(worktree),
    })
}

struct TemporaryGitWorktree {
    source_root: PathBuf,
    path: PathBuf,
}

impl TemporaryGitWorktree {
    fn add(role: &str, source_root: &Path, git_ref: &str) -> homeboy::core::Result<Self> {
        let parent = std::env::temp_dir().join("homeboy-trace-compare");
        std::fs::create_dir_all(&parent).map_err(|err| {
            homeboy::core::Error::internal_io(
                format!(
                    "Failed to create trace compare temp dir {}: {}",
                    parent.display(),
                    err
                ),
                Some("trace.compare.temp".to_string()),
            )
        })?;
        let path = parent.join(format!("{}-{}", role, uuid::Uuid::new_v4()));
        let path_arg = path.to_string_lossy().to_string();
        git::run_git(
            source_root,
            &["worktree", "add", "--detach", &path_arg, git_ref],
            "git worktree add trace compare target",
        )?;
        Ok(Self {
            source_root: source_root.to_path_buf(),
            path,
        })
    }
}

impl Drop for TemporaryGitWorktree {
    fn drop(&mut self) {
        let path = self.path.to_string_lossy().to_string();
        let _ = git::run_git(
            &self.source_root,
            &["worktree", "remove", "--force", &path],
            "git worktree remove trace compare target",
        );
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
