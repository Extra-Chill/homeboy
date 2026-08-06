//! Source guard: each running-staleness threshold is defined exactly once.
//!
//! Four independent definitions of "when is a `Running` record stale" existed
//! in this workspace, all holding 30 minutes and none pinned equal by any test:
//!
//! * `activity.rs::RUNNING_HEARTBEAT_STALE_MINUTES` (#9743)
//! * `http_api.rs::OWNERLESS_RUNNING_STALE_THRESHOLD_MINUTES`
//! * `commands/runs/reconcile.rs::OWNERLESS_RUNNING_STALE_THRESHOLD_MINUTES`
//! * `agent_task_service/discovery.rs::STALE_UPDATE_THRESHOLD_MINUTES` (#5682)
//!
//! A fifth copy was worse: `AgentTaskRunRecord::has_fresh_update` compared
//! against a bare `30` with no name at all, so it could never have been found
//! by searching for the constant. That is the shape this guard exists to catch —
//! the divergence that is invisible to a name-based search.
//!
//! They resolved to **two** concepts, not one, and the split is by which
//! timestamp is measured. See [`super::RUNNING_HEARTBEAT_STALE_MINUTES`] and
//! [`super::OWNERLESS_RUNNING_STALE_THRESHOLD_MINUTES`] for the reasoning. This
//! guard pins the resulting structure so a third home cannot quietly appear.
//!
//! Following the `owned_args_guard_test` and `core-agnostic-source` precedent,
//! this reads source text, so it is deliberately shape-based rather than
//! semantic. Comment lines are skipped so prose describing the shape — this
//! module header included — is not a finding.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Every constant in this workspace that names a running-staleness threshold in
/// minutes, mapped to the reason it is allowed its own definition.
///
/// Two entries are the shared vocabulary; the third is a genuinely different
/// concept that merely lives in the same family. Anything else is a
/// re-divergence and fails the guard.
const SANCTIONED_STALENESS_THRESHOLDS: &[(&str, &str)] = &[
    (
        "crates/homeboy-core/src/observation/records/run_status.rs::RUNNING_HEARTBEAT_STALE_MINUTES",
        "canonical: heartbeat liveness, measured from `updated_at` (#9743, #5682)",
    ),
    (
        "crates/homeboy-core/src/observation/records/run_status.rs::OWNERLESS_RUNNING_STALE_THRESHOLD_MINUTES",
        "canonical: ownerless grace period, measured from `started_at`",
    ),
    (
        "crates/homeboy-cli/src/commands/runs/reconcile.rs::RUNNER_BACKED_RUNNING_STALE_THRESHOLD_MINUTES",
        "different concept: the 24h ceiling on a runner-backed record's reconciliation exemption, \
         where a live remote job is authoritative and the bound exists only so the exemption ends (#11107)",
    ),
];

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<workspace>/crates/homeboy-core`.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// The source subtrees this guard walks: every workspace crate, plus the thin
/// root `homeboy` crate. Workspace-root `tests/` is excluded as test source.
const SCANNED_SUBTREES: &[&str] = &["crates", "src"];

fn is_test_source(relative_path: &str) -> bool {
    relative_path.contains("/tests/")
        || relative_path.ends_with("_test.rs")
        || relative_path.ends_with("_tests.rs")
        || relative_path.ends_with("/tests.rs")
}

fn collect_rust_sources(directory: &Path, root: &Path, collected: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };

    for entry in entries {
        let entry = entry.expect("readable directory entry");
        let path = entry.path();
        if path.is_dir() {
            // `target/` under a crate is build output, not source.
            if path.file_name().and_then(|name| name.to_str()) == Some("target") {
                continue;
            }
            collect_rust_sources(&path, root, collected);
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let relative_path = path
            .strip_prefix(root)
            .expect("collected path is under the workspace root")
            .to_string_lossy()
            .replace('\\', "/");
        if is_test_source(&relative_path) {
            continue;
        }
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        collected.push((relative_path, content));
    }
}

fn is_comment_line(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// The name of the staleness-threshold constant this line declares, if it
/// declares one.
///
/// The shape is a `const` whose name mentions both staleness and a minute unit,
/// which is what every one of the original four spelled.
fn declared_staleness_threshold(line: &str) -> Option<&str> {
    let mut rest = line.trim_start();
    for visibility in ["pub(crate) ", "pub(super) ", "pub(self) ", "pub "] {
        if let Some(stripped) = rest.strip_prefix(visibility) {
            rest = stripped.trim_start();
            break;
        }
    }
    let rest = rest.strip_prefix("const ")?;
    let name_end = rest.find(':')?;
    let name = rest[..name_end].trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    (name.contains("STALE") && name.contains("MINUTES")).then_some(name)
}

/// Whether a `num_minutes()` result on this line is compared against a bare
/// integer literal rather than a named threshold.
///
/// The comparison may wrap onto the next line, which is exactly how the
/// `has_fresh_update` copy hid: `.num_minutes()` ended one line and `< 30`
/// began the next.
fn compares_minutes_to_a_bare_literal(lines: &[&str], index: usize) -> bool {
    const CALL: &str = "num_minutes()";
    let line = lines[index];
    let Some(offset) = line.find(CALL) else {
        return false;
    };

    let mut tail = line[offset + CALL.len()..].trim().to_string();
    if tail.is_empty() {
        let Some(next) = lines.get(index + 1) else {
            return false;
        };
        if is_comment_line(next) {
            return false;
        }
        tail = next.trim().to_string();
    }

    let stripped = tail.trim_start_matches(['<', '>', '=']);
    if stripped.len() == tail.len() {
        // No comparison operator followed the call.
        return false;
    }
    stripped
        .trim_start()
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit())
}

fn workspace_sources() -> Vec<(String, String)> {
    let root = workspace_root();
    let mut sources = Vec::new();
    for subtree in SCANNED_SUBTREES {
        collect_rust_sources(&root.join(subtree), &root, &mut sources);
    }
    assert!(
        !sources.is_empty(),
        "no rust sources found under {}",
        root.display()
    );
    sources
}

/// Every staleness-threshold constant declared in the workspace, by site.
fn declared_staleness_thresholds() -> BTreeSet<String> {
    let mut declarations = BTreeSet::new();
    for (relative_path, content) in &workspace_sources() {
        for line in content.lines() {
            if is_comment_line(line) {
                continue;
            }
            if let Some(name) = declared_staleness_threshold(line) {
                declarations.insert(format!("{relative_path}::{name}"));
            }
        }
    }
    declarations
}

#[test]
fn a_new_running_staleness_threshold_is_a_failure() {
    let declarations = declared_staleness_thresholds();
    let unsanctioned: Vec<&String> = declarations
        .iter()
        .filter(|site| {
            !SANCTIONED_STALENESS_THRESHOLDS
                .iter()
                .any(|(sanctioned, _)| *sanctioned == site.as_str())
        })
        .collect();

    assert!(
        unsanctioned.is_empty(),
        "these sites define their own running-staleness threshold: {unsanctioned:?}. \
         Four such definitions already drifted apart unpinned once. Import \
         `RUNNING_HEARTBEAT_STALE_MINUTES` (heartbeat age, from `updated_at`) or \
         `OWNERLESS_RUNNING_STALE_THRESHOLD_MINUTES` (ownerless grace, from `started_at`) \
         from `homeboy_core::observation`, or add the site to \
         SANCTIONED_STALENESS_THRESHOLDS with the reason it is a distinct concept."
    );
}

#[test]
fn a_sanctioned_threshold_that_no_longer_exists_is_a_failure() {
    // A stale entry is how an inventory stops describing the tree it guards.
    let declarations = declared_staleness_thresholds();
    let missing: Vec<&str> = SANCTIONED_STALENESS_THRESHOLDS
        .iter()
        .filter(|(site, _)| !declarations.contains(*site))
        .map(|(site, _)| *site)
        .collect();

    assert!(
        missing.is_empty(),
        "SANCTIONED_STALENESS_THRESHOLDS names thresholds that no longer exist: {missing:?}. \
         Remove the stale rows."
    );
}

#[test]
fn every_sanctioned_threshold_states_a_reason() {
    for (site, reason) in SANCTIONED_STALENESS_THRESHOLDS {
        assert!(
            reason.len() > 20,
            "{site} is sanctioned without a usable reason"
        );
    }
}

#[test]
fn no_minute_age_is_compared_against_a_bare_literal() {
    let mut findings = Vec::new();
    for (relative_path, content) in &workspace_sources() {
        let lines: Vec<&str> = content.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if is_comment_line(line) {
                continue;
            }
            if compares_minutes_to_a_bare_literal(&lines, index) {
                findings.push(format!("{relative_path}:{}", index + 1));
            }
        }
    }

    assert!(
        findings.is_empty(),
        "these sites compare a minute age against a bare integer literal: {findings:?}. \
         `AgentTaskRunRecord::has_fresh_update` was a fifth copy of the 30-minute \
         staleness rule written exactly this way, invisible to any search for the \
         constant's name. Name the threshold instead."
    );
}

#[test]
fn the_guard_recognizes_the_declaration_shape_it_is_written_to_catch() {
    // The four original definitions, as they were written.
    assert_eq!(
        declared_staleness_threshold("const RUNNING_HEARTBEAT_STALE_MINUTES: i64 = 30;"),
        Some("RUNNING_HEARTBEAT_STALE_MINUTES")
    );
    assert_eq!(
        declared_staleness_threshold("const OWNERLESS_RUNNING_STALE_THRESHOLD_MINUTES: i64 = 30;"),
        Some("OWNERLESS_RUNNING_STALE_THRESHOLD_MINUTES")
    );
    assert_eq!(
        declared_staleness_threshold("const STALE_UPDATE_THRESHOLD_MINUTES: i64 = 30;"),
        Some("STALE_UPDATE_THRESHOLD_MINUTES")
    );
    assert_eq!(
        declared_staleness_threshold("pub const RUNNING_HEARTBEAT_STALE_MINUTES: i64 = 30;"),
        Some("RUNNING_HEARTBEAT_STALE_MINUTES")
    );

    // A staleness threshold in another unit is a different question.
    assert_eq!(
        declared_staleness_threshold("pub const STALE_RUN_RECLAIM_SECS: i64 = 6 * 60 * 60;"),
        None
    );
    assert_eq!(
        declared_staleness_threshold("const STALE_PENDING_ACTION_SECONDS: i64 = 24 * 60 * 60;"),
        None
    );
    // Not a constant, and not a threshold.
    assert_eq!(
        declared_staleness_threshold("    let stale_minutes = 30;"),
        None
    );
    assert_eq!(
        declared_staleness_threshold("const STALE_DAEMON_RECOVERY: &str = \"stale\";"),
        None
    );
}

#[test]
fn the_guard_recognizes_the_bare_literal_shape_it_is_written_to_catch() {
    // The `has_fresh_update` copy, as it was written: the operator wrapped.
    let wrapped = [
        "                    .signed_duration_since(updated_at.with_timezone(&chrono::Utc))",
        "                    .num_minutes()",
        "                    < 30",
    ];
    assert!(compares_minutes_to_a_bare_literal(&wrapped, 1));

    // The same shape inline.
    let inline = ["let stale = age.num_minutes() >= 30;"];
    assert!(compares_minutes_to_a_bare_literal(&inline, 0));

    // A named threshold is the whole point and must not be a finding.
    let named = [
        "                    .num_minutes()",
        "                    < RUNNING_HEARTBEAT_STALE_MINUTES",
    ];
    assert!(!compares_minutes_to_a_bare_literal(&named, 0));

    let named_inline = ["age.num_minutes() >= OWNERLESS_RUNNING_STALE_THRESHOLD_MINUTES"];
    assert!(!compares_minutes_to_a_bare_literal(&named_inline, 0));

    // Binding the age without comparing it is not a finding.
    let bound = ["let age_minutes = (now - heartbeat).num_minutes();"];
    assert!(!compares_minutes_to_a_bare_literal(&bound, 0));
}

#[test]
fn test_sources_are_out_of_scope() {
    // Paths are workspace-relative, as `collect_rust_sources` produces them.
    assert!(is_test_source(
        "crates/homeboy-cli/src/commands/runner/tests/status.rs"
    ));
    assert!(is_test_source("crates/homeboy-core/src/paths_tests.rs"));
    assert!(is_test_source(
        "crates/homeboy-core/src/observation/records/run_staleness_guard_test.rs"
    ));
    assert!(!is_test_source(
        "crates/homeboy-core/src/observation/records/run_status.rs"
    ));
    assert!(!is_test_source(
        "crates/homeboy-cli/src/commands/runs/reconcile.rs"
    ));
    assert!(!is_test_source("src/lib.rs"));
}
