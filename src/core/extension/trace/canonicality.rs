use std::path::Path;
use std::process::Command;

use crate::core::component::Component;
use crate::core::error::{ErrorCode, Result};
use crate::core::extension::manifest_config::TraceToolchainProvenanceConfig;
use crate::core::extension::ExtensionExecutionContext;

use super::parsing::{
    TraceAssertion, TraceAssertionStatus, TraceCanonicalCheck, TraceEvidenceMetadata, TraceResults,
    TraceStatus,
};
use super::run::{
    TraceCheckoutProvenance, TraceRunFailure, TraceRunWorkflowArgs, TraceRunWorkflowResult,
};

const HOMEBOY_TRACE_COMPARE_PROVENANCE_SOURCE: &str = "homeboy-trace-compare";

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
        metrics: Default::default(),
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

    let checkout_provenance = args
        .checkout_provenance
        .as_ref()
        .filter(|provenance| canonical_paths_equal(&provenance.path, component_path));

    if let Some(path_override) = args.path_override.as_deref() {
        if !canonical_paths_equal(path_override, &component.local_path)
            && checkout_provenance.is_none()
        {
            reasons.push(format!(
                "component path resolved through local override `{}` instead of declared binding `{}`",
                path_override, component.local_path
            ));
        }
    }

    checks.push(check_git_checkout(
        "component",
        Path::new(component_path),
        checkout_provenance,
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
        checks.push(check_extension_checkout(context, &mut reasons));
    }

    for requirement in trace_toolchain_provenance_requirements(execution_context)? {
        check_declared_toolchain_provenance(&requirement, args, &mut checks, &mut reasons);
    }

    Ok(TraceCanonicalityReport {
        canonical: reasons.is_empty(),
        reasons,
        checks,
    })
}

pub(crate) fn trace_toolchain_provenance_requirements(
    execution_context: Option<&ExtensionExecutionContext>,
) -> Result<Vec<TraceToolchainProvenanceConfig>> {
    let Some(context) = execution_context else {
        return Ok(Vec::new());
    };
    let manifest = match crate::core::extension::load_extension(&context.extension_id) {
        Ok(manifest) => manifest,
        Err(error) if error.code == ErrorCode::ExtensionNotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    Ok(manifest.trace_toolchain_provenance().to_vec())
}

fn check_declared_toolchain_provenance(
    requirement: &TraceToolchainProvenanceConfig,
    args: &TraceRunWorkflowArgs,
    checks: &mut Vec<TraceCanonicalCheck>,
    reasons: &mut Vec<String>,
) {
    for key in &requirement.env_keys {
        let explicit = args.runner_inputs.env.iter().any(|(name, _)| name == key);
        let inherited = std::env::var_os(key);
        if !explicit && inherited.is_none() {
            continue;
        }
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
                &format!("toolchain:{} ({key}, {source})", requirement.id),
                &crate::core::git::git_probe_path(Path::new(path)),
                None,
                reasons,
            )),
            _ => reasons.push(format!(
                "{} toolchain path `{}` is empty via {}",
                requirement.label, key, source
            )),
        }
    }
}

fn check_extension_checkout(
    context: &ExtensionExecutionContext,
    reasons: &mut Vec<String>,
) -> TraceCanonicalCheck {
    let target = format!("extension:{}", context.extension_id);
    if git_output(
        &context.extension_path,
        &["rev-parse", "--is-inside-work-tree"],
    )
    .as_deref()
        == Some("true")
    {
        return check_git_checkout(&target, &context.extension_path, None, reasons);
    }

    let manifest_path = context
        .extension_path
        .join(format!("{}.json", context.extension_id));
    if !context.extension_path.exists() {
        reasons.push(format!(
            "{} checkout path is missing: {}",
            target,
            context.extension_path.display()
        ));
        return empty_check(&target, &context.extension_path, "missing");
    }
    if !manifest_path.exists() {
        reasons.push(format!(
            "{} installed extension manifest is missing: {}",
            target,
            manifest_path.display()
        ));
        return empty_check(&target, &context.extension_path, "missing-manifest");
    }

    TraceCanonicalCheck {
        target,
        path: context.extension_path.to_string_lossy().to_string(),
        status: "installed-extension".to_string(),
        sha: None,
        branch: None,
        upstream: None,
        commits_ahead: None,
        commits_behind: None,
    }
}

fn canonical_paths_equal(left: &str, right: &str) -> bool {
    let left = Path::new(left).canonicalize();
    let right = Path::new(right).canonicalize();
    matches!((left, right), (Ok(left), Ok(right)) if left == right)
}

fn check_git_checkout(
    target: &str,
    path: &Path,
    provenance: Option<&TraceCheckoutProvenance>,
    reasons: &mut Vec<String>,
) -> TraceCanonicalCheck {
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
    let trusted_exact_sha = match provenance {
        Some(provenance) => {
            validate_exact_sha_provenance(target, path, sha.as_deref(), provenance, reasons)
        }
        None => false,
    };

    push_git_reasons(
        target,
        path,
        dirty,
        branch.as_deref(),
        upstream.as_deref(),
        ahead,
        behind,
        trusted_exact_sha,
        reasons,
    );

    TraceCanonicalCheck {
        target: target.to_string(),
        path: path.to_string_lossy().to_string(),
        status: checkout_status(
            dirty,
            branch.as_deref(),
            upstream.as_deref(),
            ahead,
            behind,
            trusted_exact_sha,
        ),
        sha,
        branch,
        upstream,
        commits_ahead: ahead,
        commits_behind: behind,
    }
}

fn validate_exact_sha_provenance(
    target: &str,
    path: &Path,
    sha: Option<&str>,
    provenance: &TraceCheckoutProvenance,
    reasons: &mut Vec<String>,
) -> bool {
    if provenance.source != HOMEBOY_TRACE_COMPARE_PROVENANCE_SOURCE {
        reasons.push(format!(
            "{} checkout exact-SHA provenance source is not trusted: {}",
            target, provenance.source
        ));
        return false;
    }
    if !canonical_paths_equal(&provenance.path, &path.to_string_lossy()) {
        reasons.push(format!(
            "{} checkout exact-SHA provenance path does not match checkout: {} != {}",
            target,
            provenance.path,
            path.display()
        ));
        return false;
    }
    if provenance.requested_ref.trim().is_empty() || provenance.resolved_sha.trim().is_empty() {
        reasons.push(format!(
            "{} checkout exact-SHA provenance is incomplete for {}",
            target,
            path.display()
        ));
        return false;
    }
    if sha != Some(provenance.resolved_sha.as_str()) {
        reasons.push(format!(
            "{} checkout HEAD does not match recorded exact-SHA provenance for `{}`: expected {}, got {}",
            target,
            provenance.requested_ref,
            provenance.resolved_sha,
            sha.unwrap_or("unknown")
        ));
        return false;
    }
    true
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
    trusted_exact_sha: bool,
    reasons: &mut Vec<String>,
) {
    if dirty {
        reasons.push(format!("{} checkout is dirty: {}", target, path.display()));
    }
    if branch == Some("HEAD") && !trusted_exact_sha {
        reasons.push(format!(
            "{} checkout is detached: {}",
            target,
            path.display()
        ));
    }
    if upstream.is_none() && !trusted_exact_sha {
        reasons.push(format!(
            "{} checkout has no upstream branch: {}",
            target,
            path.display()
        ));
    }
    let mut push_divergence = |count: Option<u32>, direction: &str, qualifier: &str| {
        let count = count.unwrap_or(0);
        if count > 0 {
            reasons.push(format!(
                "{} checkout is {} upstream by {} {}commit(s): {}",
                target,
                direction,
                count,
                qualifier,
                path.display()
            ));
        }
    };
    push_divergence(behind, "behind", "");
    push_divergence(ahead, "ahead of", "unmerged ");
}

fn checkout_status(
    dirty: bool,
    branch: Option<&str>,
    upstream: Option<&str>,
    ahead: Option<u32>,
    behind: Option<u32>,
    trusted_exact_sha: bool,
) -> String {
    if dirty
        || (branch == Some("HEAD") && !trusted_exact_sha)
        || (upstream.is_none() && !trusted_exact_sha)
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
    use crate::core::extension::ExtensionCapability;
    use std::path::Path;

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
    fn canonical_trace_refuses_missing_declared_toolchain_runner_env() {
        crate::test_support::with_isolated_home(|home| {
            let temp = tempfile::tempdir().unwrap();
            init_git_repo(temp.path());
            let component = test_component(temp.path());
            let context = write_trace_extension(home.path(), &component);
            let mut args = test_run_args(temp.path());
            args.canonical_policy = TraceCanonicalPolicy::Canonical;
            args.runner_inputs.env.push((
                "FIXTURE_TOOLCHAIN_BIN".to_string(),
                "/tmp/other-fixture-toolchain".to_string(),
            ));

            let report = evaluate_trace_canonicality(Some(&context), &component, &args).unwrap();

            assert!(!report.is_canonical());
            assert!(report.reasons.iter().any(|reason| {
                reason.contains("toolchain:fixture-toolchain (FIXTURE_TOOLCHAIN_BIN, trace runner env) checkout path is missing")
            }));
        });
    }

    #[test]
    fn canonical_trace_accepts_clean_declared_toolchain_runner_env() {
        crate::test_support::with_isolated_home(|home| {
            let component_dir = tempfile::tempdir().unwrap();
            let component_remote = tempfile::tempdir().unwrap();
            let toolchain_dir = tempfile::tempdir().unwrap();
            let toolchain_remote = tempfile::tempdir().unwrap();
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
            init_git_repo(toolchain_dir.path());
            init_bare_repo(toolchain_remote.path());
            git(
                toolchain_dir.path(),
                &[
                    "remote",
                    "add",
                    "origin",
                    toolchain_remote.path().to_str().unwrap(),
                ],
            );
            git(toolchain_dir.path(), &["push", "-u", "origin", "main"]);
            let bin = toolchain_dir.path().join("packages/cli/dist/index.js");
            std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
            std::fs::write(&bin, "#!/usr/bin/env node\n").unwrap();
            git(toolchain_dir.path(), &["add", "."]);
            git(toolchain_dir.path(), &["commit", "-m", "add cli bin"]);
            git(toolchain_dir.path(), &["push", "origin", "main"]);

            let component = test_component(component_dir.path());
            let context = write_trace_extension(home.path(), &component);
            let mut args = test_run_args(component_dir.path());
            args.canonical_policy = TraceCanonicalPolicy::Canonical;
            args.runner_inputs.env.push((
                "FIXTURE_TOOLCHAIN_BIN".to_string(),
                bin.to_string_lossy().to_string(),
            ));

            let report = evaluate_trace_canonicality(Some(&context), &component, &args).unwrap();

            assert!(report.is_canonical());
            assert!(report.reasons.is_empty());
        });
    }

    #[test]
    fn canonical_trace_accepts_homeboy_compare_exact_sha_worktree() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        init_git_repo(source.path());
        let sha = git_stdout(source.path(), &["rev-parse", "HEAD"]).unwrap();
        git(
            source.path(),
            &[
                "worktree",
                "add",
                "--detach",
                target.path().to_str().unwrap(),
                &sha,
            ],
        );
        let component = test_component(source.path());
        let mut args = test_run_args(target.path());
        args.checkout_provenance = Some(TraceCheckoutProvenance {
            source: HOMEBOY_TRACE_COMPARE_PROVENANCE_SOURCE.to_string(),
            path: target.path().to_string_lossy().to_string(),
            requested_ref: "main".to_string(),
            resolved_sha: sha,
        });

        let report = evaluate_trace_canonicality(None, &component, &args).unwrap();

        assert!(report.is_canonical());
        assert!(report.reasons.is_empty());
    }

    #[test]
    fn canonical_trace_refuses_arbitrary_detached_checkout() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        init_git_repo(source.path());
        let sha = git_stdout(source.path(), &["rev-parse", "HEAD"]).unwrap();
        git(
            source.path(),
            &[
                "worktree",
                "add",
                "--detach",
                target.path().to_str().unwrap(),
                &sha,
            ],
        );
        let component = test_component(source.path());
        let args = test_run_args(target.path());

        let report = evaluate_trace_canonicality(None, &component, &args).unwrap();

        assert!(!report.is_canonical());
        assert!(report
            .reasons
            .iter()
            .any(|reason| reason.contains("local override")));
        assert!(report
            .reasons
            .iter()
            .any(|reason| reason.contains("checkout is detached")));
        assert!(report
            .reasons
            .iter()
            .any(|reason| reason.contains("no upstream branch")));
    }

    #[test]
    fn canonical_trace_refuses_dirty_homeboy_compare_exact_sha_worktree() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        init_git_repo(source.path());
        let sha = git_stdout(source.path(), &["rev-parse", "HEAD"]).unwrap();
        git(
            source.path(),
            &[
                "worktree",
                "add",
                "--detach",
                target.path().to_str().unwrap(),
                &sha,
            ],
        );
        std::fs::write(target.path().join("dirty.txt"), "dirty\n").unwrap();
        let component = test_component(source.path());
        let mut args = test_run_args(target.path());
        args.checkout_provenance = Some(TraceCheckoutProvenance {
            source: HOMEBOY_TRACE_COMPARE_PROVENANCE_SOURCE.to_string(),
            path: target.path().to_string_lossy().to_string(),
            requested_ref: "main".to_string(),
            resolved_sha: sha,
        });

        let report = evaluate_trace_canonicality(None, &component, &args).unwrap();

        assert!(!report.is_canonical());
        assert_eq!(
            report
                .reasons
                .iter()
                .filter(|reason| reason.contains("checkout is dirty"))
                .count(),
            1
        );
        assert!(!report
            .reasons
            .iter()
            .any(|reason| reason.contains("checkout is detached")));
        assert!(!report
            .reasons
            .iter()
            .any(|reason| reason.contains("no upstream branch")));
    }

    #[test]
    fn canonical_trace_accepts_installed_extension_manifest_directory() {
        let component_dir = tempfile::tempdir().unwrap();
        let component_remote = tempfile::tempdir().unwrap();
        let extension_dir = tempfile::tempdir().unwrap();
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
        std::fs::write(extension_dir.path().join("fixture-extension.json"), "{}\n").unwrap();
        let component = test_component(component_dir.path());
        let args = test_run_args(component_dir.path());
        let context = ExtensionExecutionContext {
            component: component.clone(),
            capability: ExtensionCapability::Trace,
            extension_id: "fixture-extension".to_string(),
            extension_path: extension_dir.path().to_path_buf(),
            script_path: "trace.js".to_string(),
            settings: Vec::new(),
            accepted_setting_keys: Vec::new(),
        };

        let report = evaluate_trace_canonicality(Some(&context), &component, &args).unwrap();

        assert!(report.is_canonical());
        assert!(report.reasons.is_empty());
        assert!(report.checks.iter().any(|check| {
            check.target == "extension:fixture-extension" && check.status == "installed-extension"
        }));
    }

    #[test]
    fn canonical_trace_refuses_installed_extension_without_manifest() {
        let component_dir = tempfile::tempdir().unwrap();
        let extension_dir = tempfile::tempdir().unwrap();
        init_git_repo(component_dir.path());
        let component = test_component(component_dir.path());
        let args = test_run_args(component_dir.path());
        let context = ExtensionExecutionContext {
            component: component.clone(),
            capability: ExtensionCapability::Trace,
            extension_id: "fixture-extension".to_string(),
            extension_path: extension_dir.path().to_path_buf(),
            script_path: "trace.js".to_string(),
            settings: Vec::new(),
            accepted_setting_keys: Vec::new(),
        };

        let report = evaluate_trace_canonicality(Some(&context), &component, &args).unwrap();

        assert!(!report.is_canonical());
        assert!(report
            .reasons
            .iter()
            .any(|reason| reason.contains("installed extension manifest is missing")));
    }

    #[test]
    fn allow_local_toolchain_marks_result_non_canonical_without_refusing() {
        let temp = tempfile::tempdir().unwrap();
        init_git_repo(temp.path());
        std::fs::write(temp.path().join("dirty.txt"), "dirty\n").unwrap();
        let component = test_component(temp.path());
        let run_dir = RunDir::create().unwrap();
        let mut args = test_run_args(temp.path());
        args.canonical_policy = TraceCanonicalPolicy::AllowLocalToolchain;

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

    fn write_trace_extension(home: &Path, component: &Component) -> ExtensionExecutionContext {
        let extension_id = "fixture-extension";
        let extension_dir = home.join(".config/homeboy/extensions").join(extension_id);
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        std::fs::write(
            extension_dir.join(format!("{extension_id}.json")),
            serde_json::json!({
                "name": "Fixture Extension",
                "version": "0.0.0",
                "trace": {
                    "extension_script": "trace.js",
                    "toolchain_provenance": [
                        {
                            "id": "fixture-toolchain",
                            "label": "Fixture Toolchain",
                            "env_keys": ["FIXTURE_TOOLCHAIN_BIN"]
                        }
                    ]
                }
            })
            .to_string(),
        )
        .expect("extension manifest");
        std::fs::write(extension_dir.join("trace.js"), "#!/usr/bin/env node\n").unwrap();

        ExtensionExecutionContext {
            component: component.clone(),
            capability: ExtensionCapability::Trace,
            extension_id: extension_id.to_string(),
            extension_path: extension_dir,
            script_path: "trace.js".to_string(),
            settings: Vec::new(),
            accepted_setting_keys: Vec::new(),
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

    fn git_stdout(path: &std::path::Path, args: &[&str]) -> Option<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
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
            checkout_provenance: None,
        }
    }
}
