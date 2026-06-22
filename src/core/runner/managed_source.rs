//! Generic, runtime-agnostic management of an extension-declared runner-side
//! source checkout.
//!
//! Background (issue #3818): a Lab runner can execute cooks using a tool whose
//! source lives in a git checkout on the runner (for example a path under the
//! runner's homeboy cache directory). Iterating on fixes to that tool used to
//! require manually `ssh`-ing into the runner and running
//! `git reset --hard / git fetch / git checkout` by hand. The checkout drifted
//! from its intended ref and could even track the wrong remote.
//!
//! This module lets homeboy keep such a checkout synced automatically. Core
//! treats the source generically: it is a *named* checkout with an optional
//! canonical remote URL and an optional intended ref. Core has **no knowledge**
//! of what the source actually is (a runtime, a CLI, a toolchain, ...). The
//! declaring extension supplies the path/remote/ref via the
//! [`AgentTaskProviderRunnerSource`] manifest contract; this keeps homeboy core
//! runtime-agnostic.
//!
//! The sync is expressed as a single idempotent POSIX shell script so it can be
//! executed on the runner over the existing exec channel. The script:
//!
//! 1. Clones the checkout from the canonical remote when it is missing (only
//!    possible when a remote URL is declared).
//! 2. Re-points `origin` at the canonical remote when the checkout tracks a
//!    different URL (fixes the "tracks wrong remote" drift).
//! 3. Fetches `origin` and hard-resets to the intended ref when one is
//!    declared, otherwise fast-forwards the current branch to its upstream.

use serde::{Deserialize, Serialize};

use crate::core::agent_task_provider::AgentTaskProviderRunnerSource;

/// A resolved, validated plan for syncing a single managed runner source. The
/// `script` is a POSIX shell program intended to run on the runner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedRunnerSourceSyncPlan {
    pub id: String,
    pub label: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    pub script: String,
}

/// Build sync plans for every declared managed runner source. Invalid
/// declarations (empty id/path) are skipped so a single malformed contract
/// cannot break the whole sweep.
pub fn plan_managed_runner_source_syncs(
    sources: &[AgentTaskProviderRunnerSource],
) -> Vec<ManagedRunnerSourceSyncPlan> {
    sources
        .iter()
        .filter_map(plan_managed_runner_source_sync)
        .collect()
}

/// Build a single sync plan, returning `None` when the declaration is unusable.
pub fn plan_managed_runner_source_sync(
    source: &AgentTaskProviderRunnerSource,
) -> Option<ManagedRunnerSourceSyncPlan> {
    let id = source.id.trim();
    let path = source.path.trim();
    if id.is_empty() || path.is_empty() {
        return None;
    }

    let remote_url = source
        .remote_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let git_ref = source
        .git_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let script = render_sync_script(path, remote_url.as_deref(), git_ref.as_deref());

    Some(ManagedRunnerSourceSyncPlan {
        id: id.to_string(),
        label: if source.label.trim().is_empty() {
            id.to_string()
        } else {
            source.label.trim().to_string()
        },
        path: path.to_string(),
        remote_url,
        git_ref,
        script,
    })
}

/// Render the idempotent sync script for one managed source.
///
/// The script is intentionally conservative: it fails loudly (`set -e`) so the
/// caller can surface a clear error rather than silently leaving a drifted
/// checkout in place.
fn render_sync_script(path: &str, remote_url: Option<&str>, git_ref: Option<&str>) -> String {
    let quoted_path = sq(path);
    let mut script = String::new();
    script.push_str("set -e\n");
    script.push_str(&format!("dir={quoted_path}\n"));

    match remote_url {
        Some(remote_url) => {
            let quoted_remote = sq(remote_url);
            script.push_str(&format!("remote={quoted_remote}\n"));
            // Clone when the checkout (or its .git) is missing.
            script.push_str("if [ ! -d \"$dir/.git\" ]; then\n");
            script.push_str("  mkdir -p \"$(dirname \"$dir\")\"\n");
            script.push_str("  git clone \"$remote\" \"$dir\"\n");
            script.push_str("fi\n");
            // Re-point origin if it drifted from the canonical remote.
            script.push_str(
                "current_remote=$(git -C \"$dir\" config --get remote.origin.url 2>/dev/null || true)\n",
            );
            script.push_str("if [ \"$current_remote\" != \"$remote\" ]; then\n");
            script.push_str(
                "  git -C \"$dir\" remote set-url origin \"$remote\" 2>/dev/null || git -C \"$dir\" remote add origin \"$remote\"\n",
            );
            script.push_str("fi\n");
        }
        None => {
            // No remote declared: the checkout must already exist.
            script.push_str("if [ ! -d \"$dir/.git\" ]; then\n");
            script.push_str(
                "  echo \"managed runner source checkout missing and no remote_url declared: $dir\" >&2\n",
            );
            script.push_str("  exit 1\n");
            script.push_str("fi\n");
        }
    }

    script.push_str("git -C \"$dir\" fetch --prune origin\n");

    match git_ref {
        Some(git_ref) => {
            let quoted_ref = sq(git_ref);
            script.push_str(&format!("ref={quoted_ref}\n"));
            // Resolve the ref preferring the remote-tracking form so a declared
            // branch syncs to origin's tip rather than a stale local branch.
            script.push_str(
                "target=$(git -C \"$dir\" rev-parse --verify --quiet \"origin/$ref\" || git -C \"$dir\" rev-parse --verify --quiet \"$ref\")\n",
            );
            script.push_str("if [ -z \"$target\" ]; then\n");
            script.push_str("  echo \"managed runner source ref not found: $ref\" >&2\n");
            script.push_str("  exit 1\n");
            script.push_str("fi\n");
            script.push_str("git -C \"$dir\" checkout --quiet --force -B \"$ref\" \"$target\" 2>/dev/null || git -C \"$dir\" checkout --quiet --force --detach \"$target\"\n");
            script.push_str("git -C \"$dir\" reset --hard \"$target\"\n");
        }
        None => {
            // No ref declared: fast-forward the current branch to its upstream
            // when one exists. Detached or untracked managed checkouts have no
            // upstream, so reset them to the remote default branch instead.
            script.push_str(
                "upstream=$(git -C \"$dir\" rev-parse --abbrev-ref --symbolic-full-name '@{u}' 2>/dev/null || true)\n",
            );
            script.push_str("if [ -n \"$upstream\" ]; then\n");
            script.push_str("  git -C \"$dir\" merge --ff-only \"@{u}\"\n");
            script.push_str("else\n");
            script.push_str("  target=$(git -C \"$dir\" rev-parse --verify --quiet origin/HEAD || git -C \"$dir\" rev-parse --verify --quiet origin/main)\n");
            script.push_str("  if [ -z \"$target\" ]; then\n");
            script
                .push_str("    echo \"managed runner source remote default ref not found\" >&2\n");
            script.push_str("    exit 1\n");
            script.push_str("  fi\n");
            script.push_str("  git -C \"$dir\" checkout --quiet --force --detach \"$target\"\n");
            script.push_str("  git -C \"$dir\" reset --hard \"$target\"\n");
            script.push_str("fi\n");
        }
    }

    script
}

/// POSIX single-quote escaping: wrap in single quotes and replace embedded
/// single quotes with the `'\''` sequence.
fn sq(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(id: &str, path: &str) -> AgentTaskProviderRunnerSource {
        AgentTaskProviderRunnerSource {
            id: id.to_string(),
            label: String::new(),
            path: path.to_string(),
            remote_url: None,
            git_ref: None,
            remediation: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn plan_skips_declarations_without_id_or_path() {
        assert!(plan_managed_runner_source_sync(&source("", "/x")).is_none());
        assert!(plan_managed_runner_source_sync(&source("x", "   ")).is_none());
    }

    #[test]
    fn plan_defaults_label_to_id() {
        let plan = plan_managed_runner_source_sync(&source("src.cache", "/home/r/.cache/src"))
            .expect("plan");
        assert_eq!(plan.label, "src.cache");
        assert_eq!(plan.path, "/home/r/.cache/src");
    }

    #[test]
    fn plan_managed_runner_source_syncs_skips_invalid_in_batch() {
        let plans = plan_managed_runner_source_syncs(&[
            source("", "/x"),
            source("ok", "/home/r/.cache/src"),
        ]);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].id, "ok");
    }

    #[test]
    fn script_clones_and_repoints_when_remote_declared() {
        let mut decl = source("src", "/home/r/.cache/src");
        decl.remote_url = Some("https://github.com/Extra-Chill/example.git".to_string());
        let plan = plan_managed_runner_source_sync(&decl).expect("plan");

        assert!(plan.script.contains("git clone \"$remote\" \"$dir\""));
        assert!(plan
            .script
            .contains("git -C \"$dir\" remote set-url origin \"$remote\""));
        assert!(plan.script.contains("git -C \"$dir\" fetch --prune origin"));
        // Remote URL is single-quoted into the script, not interpolated raw.
        assert!(plan
            .script
            .contains("remote='https://github.com/Extra-Chill/example.git'"));
    }

    #[test]
    fn script_requires_existing_checkout_when_no_remote_declared() {
        let plan =
            plan_managed_runner_source_sync(&source("src", "/home/r/.cache/src")).expect("plan");
        assert!(plan
            .script
            .contains("managed runner source checkout missing and no remote_url declared"));
        assert!(!plan.script.contains("git clone"));
    }

    #[test]
    fn script_hard_resets_to_declared_ref() {
        let mut decl = source("src", "/home/r/.cache/src");
        decl.remote_url = Some("https://example.test/repo.git".to_string());
        decl.git_ref = Some("main".to_string());
        let plan = plan_managed_runner_source_sync(&decl).expect("plan");

        assert!(plan.script.contains("ref='main'"));
        assert!(plan
            .script
            .contains("rev-parse --verify --quiet \"origin/$ref\""));
        assert!(plan
            .script
            .contains("git -C \"$dir\" reset --hard \"$target\""));
    }

    #[test]
    fn script_restores_declared_branch_from_detached_dirty_checkout() {
        let mut decl = source("src", "/home/r/.cache/src");
        decl.remote_url = Some("https://example.test/repo.git".to_string());
        decl.git_ref = Some("main".to_string());
        let plan = plan_managed_runner_source_sync(&decl).expect("plan");

        assert!(plan
            .script
            .contains("git -C \"$dir\" checkout --quiet --force -B \"$ref\" \"$target\""));
        assert!(plan
            .script
            .contains("git -C \"$dir\" reset --hard \"$target\""));
    }

    #[test]
    fn script_fast_forwards_when_no_ref_declared() {
        let mut decl = source("src", "/home/r/.cache/src");
        decl.remote_url = Some("https://example.test/repo.git".to_string());
        let plan = plan_managed_runner_source_sync(&decl).expect("plan");

        assert!(plan.script.contains("merge --ff-only \"@{u}\""));
        assert!(plan
            .script
            .contains("rev-parse --verify --quiet origin/HEAD"));
        assert!(plan.script.contains("checkout --quiet --force --detach"));
        assert!(plan.script.contains("reset --hard \"$target\""));
    }

    #[test]
    fn script_repairs_detached_dirty_checkout_when_no_ref_declared() {
        let mut decl = source("src", "/home/r/.cache/src");
        decl.remote_url = Some("https://example.test/repo.git".to_string());
        let plan = plan_managed_runner_source_sync(&decl).expect("plan");

        assert!(plan.script.contains("if [ -n \"$upstream\" ]; then"));
        assert!(plan.script.contains("else"));
        assert!(plan.script.contains("origin/HEAD"));
        assert!(plan.script.contains("origin/main"));
        assert!(plan
            .script
            .contains("git -C \"$dir\" reset --hard \"$target\""));
    }

    #[test]
    fn sq_escapes_embedded_single_quotes() {
        assert_eq!(sq("a'b"), "'a'\\''b'");
        assert_eq!(sq("/plain/path"), "'/plain/path'");
    }

    #[test]
    fn script_is_runtime_agnostic_no_product_literals() {
        let mut decl = source("src", "/home/r/.cache/homeboy/source");
        decl.remote_url = Some("https://example.test/repo.git".to_string());
        let plan = plan_managed_runner_source_sync(&decl).expect("plan");
        let lower = plan.script.to_ascii_lowercase();
        assert!(!lower.contains("sample-runtime"));
        assert!(!lower.contains("wordpress"));
        assert!(!lower.contains("wp-"));
    }
}
