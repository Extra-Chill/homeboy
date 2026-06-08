use std::path::Path;
use std::process::Command;

use crate::core::component::Component;
use crate::core::error::Result;
use crate::core::extension::ExtensionExecutionContext;

use super::parsing::{
    TraceAssertion, TraceAssertionStatus, TraceCanonicalCheck, TraceEvidenceMetadata, TraceResults,
    TraceStatus,
};
use super::run::{TraceRunFailure, TraceRunWorkflowArgs, TraceRunWorkflowResult};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TraceCanonicalPolicy {
    #[default]
    Development,
    Canonical,
    AllowLocalToolchain,
}

impl TraceCanonicalPolicy {
    pub fn from_flags(_canonical: bool, allow_local_toolchain: bool) -> Self {
        if allow_local_toolchain {
            Self::AllowLocalToolchain
        } else {
            Self::Canonical
        }
    }

    fn mode(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Canonical => "canonical",
            Self::AllowLocalToolchain => "allow-local-toolchain",
        }
    }

    pub(crate) fn refuses_non_canonical(self) -> bool {
        self == Self::Canonical
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TraceCanonicalityReport {
    canonical: bool,
    reasons: Vec<String>,
    checks: Vec<TraceCanonicalCheck>,
}

impl TraceCanonicalityReport {
    pub(crate) fn is_canonical(&self) -> bool {
        self.canonical
    }

    pub(crate) fn metadata(&self, policy: TraceCanonicalPolicy) -> TraceEvidenceMetadata {
        TraceEvidenceMetadata {
            canonical: self.canonical,
            mode: policy.mode().to_string(),
            reasons: self.reasons.clone(),
            checks: self.checks.clone(),
        }
    }
}

pub(crate) fn refused_trace_result(
    args: TraceRunWorkflowArgs,
    evidence: TraceEvidenceMetadata,
) -> TraceRunWorkflowResult {
    let failure = format!(
        "canonical trace refused non-canonical toolchain: {}",
        evidence.reasons.join("; ")
    );
    let results = TraceResults {
        component_id: args.component_id.clone(),
        scenario_id: args.scenario_id.clone(),
        status: TraceStatus::Error,
        summary: Some("Canonical trace preflight failed before workload execution.".to_string()),
        failure: Some(failure.clone()),
        rig: None,
        evidence: Some(evidence.clone()),
        timeline: Vec::new(),
        span_definitions: args.span_definitions,
        span_results: Vec::new(),
        assertions: evidence
            .reasons
            .iter()
            .enumerate()
            .map(|(index, reason)| TraceAssertion {
                id: format!("trace_canonicality:{}", index + 1),
                status: TraceAssertionStatus::Error,
                message: Some(reason.clone()),
                details: None,
            })
            .collect(),
        temporal_assertions: Vec::new(),
        artifacts: Vec::new(),
        toolchain: None,
        components: None,
        dependencies: Vec::new(),
        preview: None,
    };

    TraceRunWorkflowResult {
        status: "error".to_string(),
        component: args.component_label,
        exit_code: 1,
        evidence,
        results: Some(results),
        failure: Some(TraceRunFailure {
            component_id: args.component_id,
            path_override: args.path_override,
            scenario_id: args.scenario_id,
            exit_code: 1,
            stderr_excerpt: failure,
            current_phase: None,
            child_pid: None,
            child_command: None,
            recipe_path: None,
            artifact_root: None,
            last_observed_homeboy_event: None,
            cleanup_succeeded: None,
        }),
        overlays: Vec::new(),
        baseline_comparison: None,
        hints: Some(vec![
            "Use --allow-local-evidence for development-only trace runs; evidence will be marked non-canonical.".to_string(),
        ]),
        toolchain: None,
        components: None,
    }
}

const WP_CODEBOX_BIN_ENV_KEYS: &[&str] =
    &["HOMEBOY_WP_CODEBOX_BIN", "HOMEBOY_SETTINGS_WP_CODEBOX_BIN"];

pub(crate) fn evaluate_trace_canonicality(
    execution_context: Option<&ExtensionExecutionContext>,
    component: &Component,
    args: &TraceRunWorkflowArgs,
) -> Result<TraceCanonicalityReport> {
    let mut reasons = Vec::new();
    let mut checks = Vec::new();
    let component_path = args
        .path_override
        .as_deref()
        .unwrap_or(component.local_path.as_str());

    if let Some(path_override) = args.path_override.as_deref() {
        if !canonical_paths_equal(path_override, &component.local_path) {
            reasons.push(format!(
                "component path resolved through local override `{}` instead of declared binding `{}`",
                path_override, component.local_path
            ));
        }
    }

    checks.push(check_git_checkout(
        "component",
        Path::new(component_path),
        &mut reasons,
    ));
    if let Some(artifact) = component.build_artifact.as_deref() {
        let artifact_path = Path::new(component_path).join(artifact);
        if !artifact_path.exists() {
            reasons.push(format!(
                "component build artifact is missing: {}",
                artifact_path.display()
            ));
            checks.push(empty_check(
                "component.build_artifact",
                &artifact_path,
                "missing",
            ));
        }
    }

    if let Some(context) = execution_context {
        checks.push(check_git_checkout(
            &format!("extension:{}", context.extension_id),
            &context.extension_path,
            &mut reasons,
        ));
    }

    for key in WP_CODEBOX_BIN_ENV_KEYS {
        let explicit = args.runner_inputs.env.iter().any(|(name, _)| name == key);
        let inherited = std::env::var_os(key);
        if explicit || inherited.is_some() {
            let source = if explicit {
                "trace runner env"
            } else {
                "process env"
            };
            let value = args
                .runner_inputs
                .env
                .iter()
                .find_map(|(name, value)| (name == key).then_some(value.as_str()))
                .or_else(|| inherited.as_deref().and_then(|value| value.to_str()));
            match value {
                Some(path) if !path.trim().is_empty() => checks.push(check_git_checkout(
                    &format!("toolchain:wp-codebox ({key}, {source})"),
                    &git_probe_path(Path::new(path)),
                    &mut reasons,
                )),
                _ => reasons.push(format!(
                    "WP Codebox toolchain path `{}` is empty via {}",
                    key, source
                )),
            }
        }
    }

    Ok(TraceCanonicalityReport {
        canonical: reasons.is_empty(),
        reasons,
        checks,
    })
}

fn git_probe_path(path: &Path) -> std::path::PathBuf {
    if path.is_file() {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

fn canonical_paths_equal(left: &str, right: &str) -> bool {
    let left = Path::new(left).canonicalize();
    let right = Path::new(right).canonicalize();
    matches!((left, right), (Ok(left), Ok(right)) if left == right)
}

fn check_git_checkout(target: &str, path: &Path, reasons: &mut Vec<String>) -> TraceCanonicalCheck {
    if !path.exists() {
        reasons.push(format!(
            "{} checkout path is missing: {}",
            target,
            path.display()
        ));
        return empty_check(target, path, "missing");
    }
    if git_output(path, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        reasons.push(format!(
            "{} path is not a git checkout: {}",
            target,
            path.display()
        ));
        return empty_check(target, path, "not-git");
    }

    let sha = git_output(path, &["rev-parse", "HEAD"]);
    let branch = git_output(path, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let upstream = git_output(path, &["rev-parse", "--abbrev-ref", "@{u}"]);
    let dirty = git_output(path, &["status", "--porcelain=v1"])
        .map(|status| !status.is_empty())
        .unwrap_or(true);
    let (ahead, behind) = upstream
        .as_ref()
        .and_then(|_| {
            git_output(
                path,
                &["rev-list", "--left-right", "--count", "HEAD...@{u}"],
            )
        })
        .and_then(|counts| parse_ahead_behind(&counts))
        .unwrap_or((None, None));

    push_git_reasons(
        target,
        path,
        dirty,
        branch.as_deref(),
        upstream.as_deref(),
        ahead,
        behind,
        reasons,
    );

    TraceCanonicalCheck {
        target: target.to_string(),
        path: path.to_string_lossy().to_string(),
        status: checkout_status(dirty, branch.as_deref(), upstream.as_deref(), ahead, behind),
        sha,
        branch,
        upstream,
        commits_ahead: ahead,
        commits_behind: behind,
    }
}

fn empty_check(target: &str, path: &Path, status: &str) -> TraceCanonicalCheck {
    TraceCanonicalCheck {
        target: target.to_string(),
        path: path.to_string_lossy().to_string(),
        status: status.to_string(),
        sha: None,
        branch: None,
        upstream: None,
        commits_ahead: None,
        commits_behind: None,
    }
}

fn push_git_reasons(
    target: &str,
    path: &Path,
    dirty: bool,
    branch: Option<&str>,
    upstream: Option<&str>,
    ahead: Option<u32>,
    behind: Option<u32>,
    reasons: &mut Vec<String>,
) {
    if dirty {
        reasons.push(format!("{} checkout is dirty: {}", target, path.display()));
    }
    if branch == Some("HEAD") {
        reasons.push(format!(
            "{} checkout is detached: {}",
            target,
            path.display()
        ));
    }
    if upstream.is_none() {
        reasons.push(format!(
            "{} checkout has no upstream branch: {}",
            target,
            path.display()
        ));
    }
    if behind.unwrap_or(0) > 0 {
        reasons.push(format!(
            "{} checkout is behind upstream by {} commit(s): {}",
            target,
            behind.unwrap_or(0),
            path.display()
        ));
    }
    if ahead.unwrap_or(0) > 0 {
        reasons.push(format!(
            "{} checkout is ahead of upstream by {} unmerged commit(s): {}",
            target,
            ahead.unwrap_or(0),
            path.display()
        ));
    }
}

fn checkout_status(
    dirty: bool,
    branch: Option<&str>,
    upstream: Option<&str>,
    ahead: Option<u32>,
    behind: Option<u32>,
) -> String {
    if dirty
        || branch == Some("HEAD")
        || upstream.is_none()
        || ahead.unwrap_or(0) > 0
        || behind.unwrap_or(0) > 0
    {
        "non-canonical"
    } else {
        "ok"
    }
    .to_string()
}

fn git_output(path: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn parse_ahead_behind(value: &str) -> Option<(Option<u32>, Option<u32>)> {
    let mut parts = value.split_whitespace();
    let ahead = parts.next()?.parse::<u32>().ok()?;
    let behind = parts.next()?.parse::<u32>().ok()?;
    Some((Some(ahead), Some(behind)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::baseline::BaselineFlags;
    use crate::core::engine::run_dir::RunDir;
    use crate::core::extension::trace::run::{run_trace_workflow, TraceRunnerInputs};

    #[test]
    fn trace_canonical_policy_defaults_to_canonical() {
        assert_eq!(
            TraceCanonicalPolicy::from_flags(false, false),
            TraceCanonicalPolicy::Canonical
        );
        assert_eq!(
            TraceCanonicalPolicy::from_flags(true, false),
            TraceCanonicalPolicy::Canonical
        );
        assert_eq!(
            TraceCanonicalPolicy::from_flags(false, true),
            TraceCanonicalPolicy::AllowLocalToolchain
        );
    }

    #[test]
    fn canonical_trace_refuses_dirty_checkout_before_workload_execution() {
        let temp = tempfile::tempdir().unwrap();
        init_git_repo(temp.path());
        std::fs::write(temp.path().join("dirty.txt"), "dirty\n").unwrap();
        let component = test_component(temp.path());
        let run_dir = RunDir::create().unwrap();
        let mut args = test_run_args(temp.path());
        args.canonical_policy = TraceCanonicalPolicy::Canonical;

        let result = run_trace_workflow(&component, args, &run_dir, None).unwrap();

        assert_eq!(result.exit_code, 1);
        assert!(!result.evidence.canonical);
        assert!(result
            .evidence
            .reasons
            .iter()
            .any(|reason| reason.contains("checkout is dirty")));
        assert_eq!(
            result.results.as_ref().unwrap().summary.as_deref(),
            Some("Canonical trace preflight failed before workload execution.")
        );
        run_dir.cleanup();
    }

    #[test]
    fn canonical_trace_refuses_checkout_behind_upstream() {
        let local = tempfile::tempdir().unwrap();
        let remote = tempfile::tempdir().unwrap();
        let writer = tempfile::tempdir().unwrap();
        init_git_repo(local.path());
        init_bare_repo(remote.path());
        git(
            local.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(local.path(), &["push", "-u", "origin", "main"]);
        git(
            writer.path(),
            &["clone", remote.path().to_str().unwrap(), "."],
        );
        git(writer.path(), &["config", "user.email", "test@example.com"]);
        git(writer.path(), &["config", "user.name", "Homeboy Test"]);
        std::fs::write(writer.path().join("remote.txt"), "remote\n").unwrap();
        git(writer.path(), &["add", "."]);
        git(writer.path(), &["commit", "-m", "remote update"]);
        git(writer.path(), &["push", "origin", "main"]);
        git(local.path(), &["fetch", "origin"]);

        let component = test_component(local.path());
        let run_dir = RunDir::create().unwrap();
        let mut args = test_run_args(local.path());
        args.canonical_policy = TraceCanonicalPolicy::Canonical;

        let result = run_trace_workflow(&component, args, &run_dir, None).unwrap();

        assert_eq!(result.exit_code, 1);
        assert!(result
            .evidence
            .reasons
            .iter()
            .any(|reason| reason.contains("behind upstream by 1 commit")));
        run_dir.cleanup();
    }

    #[test]
    fn canonical_trace_refuses_checkout_ahead_of_upstream() {
        let local = tempfile::tempdir().unwrap();
        let remote = tempfile::tempdir().unwrap();
        init_git_repo(local.path());
        init_bare_repo(remote.path());
        git(
            local.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(local.path(), &["push", "-u", "origin", "main"]);
        std::fs::write(local.path().join("local.txt"), "local\n").unwrap();
        git(local.path(), &["add", "."]);
        git(local.path(), &["commit", "-m", "local update"]);

        let component = test_component(local.path());
        let run_dir = RunDir::create().unwrap();
        let mut args = test_run_args(local.path());
        args.canonical_policy = TraceCanonicalPolicy::Canonical;

        let result = run_trace_workflow(&component, args, &run_dir, None).unwrap();

        assert_eq!(result.exit_code, 1);
        assert!(result
            .evidence
            .reasons
            .iter()
            .any(|reason| reason.contains("ahead of upstream by 1 unmerged commit")));
        run_dir.cleanup();
    }

    #[test]
    fn canonical_trace_refuses_missing_wp_codebox_runner_env() {
        let temp = tempfile::tempdir().unwrap();
        init_git_repo(temp.path());
        let component = test_component(temp.path());
        let run_dir = RunDir::create().unwrap();
        let mut args = test_run_args(temp.path());
        args.canonical_policy = TraceCanonicalPolicy::Canonical;
        args.runner_inputs.env.push((
            "HOMEBOY_WP_CODEBOX_BIN".to_string(),
            "/tmp/other-wp-codebox".to_string(),
        ));

        let result = run_trace_workflow(&component, args, &run_dir, None).unwrap();

        assert_eq!(result.exit_code, 1);
        assert!(result.evidence.reasons.iter().any(|reason| {
            reason.contains("toolchain:wp-codebox (HOMEBOY_WP_CODEBOX_BIN, trace runner env) checkout path is missing")
        }));
        run_dir.cleanup();
    }

    #[test]
    fn canonical_trace_accepts_clean_wp_codebox_runner_env() {
        let component_dir = tempfile::tempdir().unwrap();
        let component_remote = tempfile::tempdir().unwrap();
        let codebox_dir = tempfile::tempdir().unwrap();
        let codebox_remote = tempfile::tempdir().unwrap();
        init_git_repo(component_dir.path());
        init_bare_repo(component_remote.path());
        git(
            component_dir.path(),
            &[
                "remote",
                "add",
                "origin",
                component_remote.path().to_str().unwrap(),
            ],
        );
        git(component_dir.path(), &["push", "-u", "origin", "main"]);
        init_git_repo(codebox_dir.path());
        init_bare_repo(codebox_remote.path());
        git(
            codebox_dir.path(),
            &[
                "remote",
                "add",
                "origin",
                codebox_remote.path().to_str().unwrap(),
            ],
        );
        git(codebox_dir.path(), &["push", "-u", "origin", "main"]);
        let bin = codebox_dir.path().join("packages/cli/dist/index.js");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "#!/usr/bin/env node\n").unwrap();
        git(codebox_dir.path(), &["add", "."]);
        git(codebox_dir.path(), &["commit", "-m", "add cli bin"]);
        git(codebox_dir.path(), &["push", "origin", "main"]);

        let component = test_component(component_dir.path());
        let run_dir = RunDir::create().unwrap();
        let mut args = test_run_args(component_dir.path());
        args.canonical_policy = TraceCanonicalPolicy::Canonical;
        args.runner_inputs.env.push((
            "HOMEBOY_WP_CODEBOX_BIN".to_string(),
            bin.to_string_lossy().to_string(),
        ));

        let result = run_trace_workflow(&component, args, &run_dir, None).unwrap();

        assert_eq!(result.exit_code, 3);
        assert!(result.evidence.canonical);
        assert!(result.evidence.reasons.is_empty());
        run_dir.cleanup();
    }

    #[test]
    fn allow_local_toolchain_marks_result_non_canonical_without_refusing() {
        let temp = tempfile::tempdir().unwrap();
        init_git_repo(temp.path());
        let component = test_component(temp.path());
        let run_dir = RunDir::create().unwrap();
        let mut args = test_run_args(temp.path());
        args.canonical_policy = TraceCanonicalPolicy::AllowLocalToolchain;
        args.runner_inputs.env.push((
            "HOMEBOY_WP_CODEBOX_BIN".to_string(),
            "/tmp/other-wp-codebox".to_string(),
        ));

        let result = run_trace_workflow(&component, args, &run_dir, None).unwrap();

        assert_eq!(result.exit_code, 3);
        assert!(!result.evidence.canonical);
        assert_eq!(result.evidence.mode, "allow-local-toolchain");
        assert!(result.hints.as_ref().is_some_and(|hints| hints
            .iter()
            .any(|hint| hint.contains("Non-canonical local evidence mode"))));
        assert!(result.results.is_none());
        run_dir.cleanup();
    }

    fn test_component(path: &std::path::Path) -> Component {
        Component {
            id: "example".to_string(),
            local_path: path.to_string_lossy().to_string(),
            ..Default::default()
        }
    }

    fn init_git_repo(path: &std::path::Path) {
        git(path, &["init", "-b", "main"]);
        git(path, &["config", "user.email", "test@example.com"]);
        git(path, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(path.join("README.md"), "test\n").unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "-m", "initial"]);
    }

    fn init_bare_repo(path: &std::path::Path) {
        git(path, &["init", "--bare", "-b", "main"]);
    }

    fn git(path: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .unwrap_or_else(|err| panic!("git {:?} failed to start: {}", args, err));
        assert!(
            output.status.success(),
            "git {:?} failed\nstdout: {}\nstderr: {}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn test_run_args(path: &std::path::Path) -> TraceRunWorkflowArgs {
        TraceRunWorkflowArgs {
            component_label: "example".to_string(),
            component_id: "example".to_string(),
            path_override: Some(path.to_string_lossy().to_string()),
            settings: Vec::new(),
            runner_inputs: TraceRunnerInputs::default(),
            scenario_id: "missing".to_string(),
            json_summary: false,
            rig_id: None,
            overlays: Vec::new(),
            keep_overlay: false,
            span_definitions: Vec::new(),
            baseline_flags: BaselineFlags {
                baseline: false,
                ignore_baseline: true,
                ratchet: false,
            },
            regression_threshold_percent:
                crate::core::extension::trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
            regression_min_delta_ms:
                crate::core::extension::trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
            canonical_policy: TraceCanonicalPolicy::Development,
        }
    }
}
