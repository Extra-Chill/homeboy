//! Resolve and materialize the declared runtime dependency graph for agent-task
//! Lab proofs.
//!
//! WPSG (and other runtime-package loops) are thin wrappers around Homeboy's
//! agent-task dispatch architecture. A runtime proof — e.g. driving a Managed Sandbox
//! loop through `agents-api`, `sample-component`, `workspace-registry`, and the WPSG
//! component — declares those runtime substrate components as
//! [`AgentTaskComponentContract`]s on the dispatch request (`component_contracts`
//! / `runtime_component_contracts` in provider-config or client-context).
//!
//! Historically those contracts only carried a *declared* path and ref. Nothing
//! resolved the declared graph before dispatch, so proofs spent their time
//! fighting stale or missing checkouts and manually exporting substrate path env
//! vars (`SAMPLE_RUNTIME_AGENTS_API_PATH`, …) instead of testing loop behavior
//! (#6121).
//!
//! This module gives dispatch a first-class, preflight-time materialization path:
//!
//! - Resolve each declared component contract to a concrete on-disk path.
//! - Materialize each component at a **known ref** — the resolved git HEAD (or an
//!   explicitly pinned ref) of the checkout — so run evidence records exactly the
//!   `agents-api` / `sample-component` / `workspace-registry` / `sample-runtime` / WPSG
//!   ref under test.
//! - Surface an **actionable preflight failure** when a required runtime
//!   dependency cannot be resolved (missing path) or is stale (dirty working
//!   tree, or behind a pinned ref it cannot reach), *before* dispatch.
//! - Return the resolved refs/paths so the caller can record them in run
//!   evidence (the dispatch plan/task metadata).
//!
//! The resolver reuses the canonical git-provenance conventions established by
//! the Lab git-dependency materialization path (#4314) and the rig-declared
//! component resolution (#4012): a declared component is only accepted at a
//! reproducible ref, never at an ambiguous dirty/unknown checkout.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::core::agent_task::AgentTaskComponentContract;
use crate::core::git;
use crate::core::{Error, Result};

/// Schema tag for the resolved runtime dependency graph recorded in run
/// evidence.
pub const RUNTIME_DEPENDENCY_GRAPH_SCHEMA: &str = "homeboy/agent-task-runtime-dependency-graph/v1";

/// A single component dependency resolved and materialized at a known ref.
///
/// Serialized into dispatch plan/task metadata so `homeboy runs evidence` and
/// `agent-task status` surface exactly which `agents-api`, `sample-component`,
/// `workspace-registry`, `sample-runtime`, and WPSG refs/paths a proof ran against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedRuntimeDependency {
    /// Component slug (`agents-api`, `sample-component`, `sample-runtime`, `wpsg`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// Canonical absolute path of the materialized component checkout.
    pub path: String,
    /// The known ref the component was materialized at: the resolved git commit
    /// (or the explicitly declared/pinned ref when the checkout is non-git).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_ref: Option<String>,
    /// The branch the checkout is on, when resolvable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The ref explicitly declared on the contract, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_ref: Option<String>,
    /// How the component is loaded into the runtime (plugin, mu-plugin, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_as: Option<String>,
    /// Whether the component should be activated in the runtime.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activate: Option<bool>,
    /// True when the materialized checkout is a real git work tree carrying
    /// canonical provenance; false for a non-git declared path.
    pub git_provenance: bool,
}

/// The fully resolved runtime dependency graph for a dispatch, recorded in run
/// evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedRuntimeDependencyGraph {
    pub schema: &'static str,
    pub dependencies: Vec<ResolvedRuntimeDependency>,
}

impl ResolvedRuntimeDependencyGraph {
    fn new(dependencies: Vec<ResolvedRuntimeDependency>) -> Self {
        Self {
            schema: RUNTIME_DEPENDENCY_GRAPH_SCHEMA,
            dependencies,
        }
    }

    /// True when the graph carried no declared component dependencies. Callers
    /// can skip recording an empty graph into evidence.
    pub fn is_empty(&self) -> bool {
        self.dependencies.is_empty()
    }

    /// Serialize the resolved graph for run evidence.
    pub fn to_evidence_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// Resolve and materialize the declared runtime dependency graph from the
/// component contracts on a dispatch.
///
/// Each contract that declares a concrete `path` is treated as a runtime
/// dependency: its checkout is resolved to a known ref and validated for
/// freshness. Contracts that only carry an opaque provider hint (no `path`) are
/// passed through untouched — they are not on-disk runtime substrate.
///
/// Returns the resolved graph on success. On the first unresolved or stale
/// dependency it returns an actionable preflight error so dispatch fails before
/// the loop is dispatched.
pub fn resolve_runtime_dependency_graph(
    contracts: &[AgentTaskComponentContract],
) -> Result<ResolvedRuntimeDependencyGraph> {
    let mut resolved = Vec::new();
    for contract in contracts {
        let Some(path) = contract
            .path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            // No declared on-disk path: this is an opaque provider-hint contract,
            // not runtime substrate. Nothing to materialize.
            continue;
        };
        resolved.push(resolve_one(contract, path)?);
    }
    Ok(ResolvedRuntimeDependencyGraph::new(resolved))
}

fn resolve_one(
    contract: &AgentTaskComponentContract,
    declared_path: &str,
) -> Result<ResolvedRuntimeDependency> {
    let label = contract_label(contract, declared_path);
    let declared_ref = declared_contract_ref(contract);

    let path = PathBuf::from(declared_path);
    if !path.exists() {
        return Err(unresolved_dependency_error(
            &label,
            declared_path,
            &declared_ref,
        ));
    }
    let canonical = std::fs::canonicalize(&path).unwrap_or(path.clone());

    if !canonical.is_dir() {
        return Err(unresolved_dependency_error(
            &label,
            &canonical.display().to_string(),
            &declared_ref,
        ));
    }

    let provenance = resolve_checkout_provenance(&canonical, &label, declared_ref.as_deref())?;

    Ok(ResolvedRuntimeDependency {
        slug: contract.slug.clone(),
        path: canonical.display().to_string(),
        resolved_ref: provenance.resolved_ref.or_else(|| declared_ref.clone()),
        branch: provenance.branch,
        declared_ref,
        load_as: contract.load_as.clone(),
        activate: contract.activate,
        git_provenance: provenance.git_provenance,
    })
}

struct CheckoutProvenance {
    resolved_ref: Option<String>,
    branch: Option<String>,
    git_provenance: bool,
}

/// Resolve a declared component checkout to a known ref and reject stale state.
///
/// A non-git declared path materializes at its declared ref (or none) with no
/// provenance to verify. A git work tree must resolve to a concrete commit and
/// be clean; when an explicit ref is declared the checkout must be able to reach
/// it. A dirty or unreachable-pinned checkout is rejected as stale before
/// dispatch so failures are never ambiguous between a code bug and a stale
/// checkout.
fn resolve_checkout_provenance(
    path: &Path,
    label: &str,
    declared_ref: Option<&str>,
) -> Result<CheckoutProvenance> {
    if !path.join(".git").exists() {
        return Ok(CheckoutProvenance {
            resolved_ref: declared_ref.map(str::to_string),
            branch: None,
            git_provenance: false,
        });
    }

    let head = git::run_git(
        path,
        &["rev-parse", "HEAD"],
        "resolve runtime dependency HEAD",
    )
    .map(|value| value.trim().to_string())
    .ok()
    .filter(|value| !value.is_empty());
    let Some(head) = head else {
        return Err(unresolved_dependency_error(
            label,
            &path.display().to_string(),
            &declared_ref.map(str::to_string),
        ));
    };

    let branch = git::run_git(
        path,
        &["rev-parse", "--abbrev-ref", "HEAD"],
        "resolve runtime dependency branch",
    )
    .map(|value| value.trim().to_string())
    .ok()
    .filter(|value| !value.is_empty() && value != "HEAD");

    let status = git::run_git(
        path,
        &["status", "--porcelain=v1"],
        "resolve runtime dependency status",
    )
    .map(|value| value.trim().to_string())
    .unwrap_or_default();
    if !status.is_empty() {
        return Err(stale_dependency_error(
            label,
            &path.display().to_string(),
            "the runtime dependency checkout has a dirty working tree",
            vec![
                "Commit, stash, or clean the dependency checkout before rerunning the Lab proof so the materialized ref is reproducible.".to_string(),
                "A dirty runtime substrate makes proof failures ambiguous between a code bug and a stale checkout.".to_string(),
            ],
        ));
    }

    // When the contract pins an explicit ref, the checkout must be able to
    // resolve it; otherwise the proof would silently run against the wrong code.
    if let Some(declared) = declared_ref.filter(|value| !value.trim().is_empty()) {
        let resolved_declared = git::run_git(
            path,
            &["rev-parse", "--verify", &format!("{declared}^{{commit}}")],
            "verify pinned runtime dependency ref",
        )
        .map(|value| value.trim().to_string())
        .ok()
        .filter(|value| !value.is_empty());
        let Some(resolved_declared) = resolved_declared else {
            return Err(stale_dependency_error(
                label,
                &path.display().to_string(),
                &format!("the checkout cannot resolve the declared ref `{declared}`"),
                vec![
                    format!(
                        "Fetch or check out `{declared}` in the dependency checkout before rerunning the Lab proof: git -C {} fetch --all",
                        path.display()
                    ),
                    "Or update the declared component ref to a commit the checkout can reach.".to_string(),
                ],
            ));
        };
        if resolved_declared != head {
            return Err(stale_dependency_error(
                label,
                &path.display().to_string(),
                &format!(
                    "the checkout HEAD `{head}` does not match the declared ref `{declared}` (`{resolved_declared}`)"
                ),
                vec![
                    format!(
                        "Check out the declared ref before rerunning the Lab proof: git -C {} checkout {declared}",
                        path.display()
                    ),
                    "A runtime dependency must be materialized at its declared ref so the proof runs against a known, reviewable component.".to_string(),
                ],
            ));
        }
    }

    Ok(CheckoutProvenance {
        resolved_ref: Some(head),
        branch,
        git_provenance: true,
    })
}

/// The explicitly declared ref on a contract, if any. Accepts the common
/// `ref` / `pinned_ref` / `revision` keys carried in the contract `extra` map.
fn declared_contract_ref(contract: &AgentTaskComponentContract) -> Option<String> {
    extra_string(
        &contract.extra,
        &["ref", "pinned_ref", "pinnedRef", "revision"],
    )
}

fn extra_string(extra: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = extra
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_string());
        }
    }
    None
}

fn contract_label(contract: &AgentTaskComponentContract, declared_path: &str) -> String {
    contract
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| declared_path.to_string())
}

fn unresolved_dependency_error(
    label: &str,
    declared_path: &str,
    declared_ref: &Option<String>,
) -> Error {
    let mut hints = vec![
        format!(
            "Materialize or check out the `{label}` runtime dependency at `{declared_path}` before dispatch."
        ),
        "Declare the component contract `path` to a real checkout so the Lab proof can resolve and materialize it at a known ref.".to_string(),
    ];
    if let Some(declared) = declared_ref {
        hints.push(format!(
            "The contract declared ref `{declared}`; ensure the checkout exists and can reach it."
        ));
    }
    Error::validation_invalid_argument(
        "runtime_component_contract",
        format!(
            "Lab proof preflight failed: required runtime dependency `{label}` could not be resolved at `{declared_path}`"
        ),
        Some(
            serde_json::json!({
                "schema": "homeboy/agent-task-runtime-dependency-preflight/v1",
                "dependency": label,
                "declared_path": declared_path,
                "declared_ref": declared_ref,
                "reason": "unresolved",
            })
            .to_string(),
        ),
        Some(hints),
    )
}

fn stale_dependency_error(label: &str, path: &str, reason: &str, hints: Vec<String>) -> Error {
    Error::validation_invalid_argument(
        "runtime_component_contract",
        format!(
            "Lab proof preflight failed: runtime dependency `{label}` at `{path}` is stale — {reason}"
        ),
        Some(
            serde_json::json!({
                "schema": "homeboy/agent-task-runtime-dependency-preflight/v1",
                "dependency": label,
                "path": path,
                "reason": "stale",
                "detail": reason,
            })
            .to_string(),
        ),
        Some(hints),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn contract(slug: &str, path: &str, extra: Value) -> AgentTaskComponentContract {
        AgentTaskComponentContract {
            slug: Some(slug.to_string()),
            path: Some(path.to_string()),
            load_as: Some("plugin".to_string()),
            activate: Some(true),
            extra: extra.as_object().cloned().unwrap_or_default(),
        }
    }

    fn run_git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo(path: &Path) {
        run_git(path, &["init", "-b", "main"]);
        run_git(path, &["config", "user.email", "homeboy@example.test"]);
        run_git(path, &["config", "user.name", "Homeboy Test"]);
    }

    fn commit_file(path: &Path, name: &str, contents: &str) -> String {
        fs::write(path.join(name), contents).expect("write file");
        run_git(path, &["add", name]);
        run_git(path, &["commit", "-m", name]);
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(path)
            .output()
            .expect("rev-parse");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn resolves_git_component_at_known_head_ref() {
        let repo = tempfile::tempdir().expect("repo");
        init_repo(repo.path());
        let head = commit_file(repo.path(), "plugin.php", "<?php");

        let contracts = vec![contract(
            "sample-component",
            &repo.path().display().to_string(),
            Value::Null,
        )];

        let graph = resolve_runtime_dependency_graph(&contracts).expect("resolved graph");

        assert_eq!(graph.dependencies.len(), 1);
        let dep = &graph.dependencies[0];
        assert_eq!(dep.slug.as_deref(), Some("sample-component"));
        assert_eq!(dep.resolved_ref.as_deref(), Some(head.as_str()));
        assert_eq!(dep.branch.as_deref(), Some("main"));
        assert!(dep.git_provenance);
        assert_eq!(dep.load_as.as_deref(), Some("plugin"));
        assert_eq!(dep.activate, Some(true));
    }

    #[test]
    fn missing_component_path_fails_preflight() {
        let contracts = vec![contract(
            "agents-api",
            "/nonexistent/agents-api/checkout",
            Value::Null,
        )];

        let error =
            resolve_runtime_dependency_graph(&contracts).expect_err("missing path fails preflight");

        assert!(error.message.contains("could not be resolved"));
        assert!(error.message.contains("agents-api"));
        // The structured preflight payload is carried in the invalid-argument
        // `id` field as a JSON string.
        let payload: Value =
            serde_json::from_str(error.details["id"].as_str().expect("structured id payload"))
                .expect("parse preflight payload");
        assert_eq!(payload["reason"], "unresolved");
        assert_eq!(payload["dependency"], "agents-api");
    }

    #[test]
    fn dirty_component_checkout_is_rejected_as_stale() {
        let repo = tempfile::tempdir().expect("repo");
        init_repo(repo.path());
        commit_file(repo.path(), "plugin.php", "<?php");
        fs::write(repo.path().join("dirty.txt"), "dirty").expect("write dirty");

        let contracts = vec![contract(
            "sample-runtime",
            &repo.path().display().to_string(),
            Value::Null,
        )];

        let error = resolve_runtime_dependency_graph(&contracts)
            .expect_err("dirty checkout fails preflight");

        assert!(error.message.contains("stale"));
        assert!(error.message.contains("dirty working tree"));
    }

    #[test]
    fn pinned_ref_mismatch_is_rejected_as_stale() {
        let repo = tempfile::tempdir().expect("repo");
        init_repo(repo.path());
        let first = commit_file(repo.path(), "a.txt", "a");
        let _second = commit_file(repo.path(), "b.txt", "b");

        // Declare the older commit as the pinned ref while HEAD is newer.
        let contracts = vec![contract(
            "wpsg",
            &repo.path().display().to_string(),
            serde_json::json!({ "ref": first }),
        )];

        let error = resolve_runtime_dependency_graph(&contracts)
            .expect_err("pinned ref mismatch fails preflight");

        assert!(error.message.contains("stale"));
        assert!(error.message.contains("does not match the declared ref"));
    }

    #[test]
    fn pinned_ref_match_resolves() {
        let repo = tempfile::tempdir().expect("repo");
        init_repo(repo.path());
        let head = commit_file(repo.path(), "a.txt", "a");

        let contracts = vec![contract(
            "wpsg",
            &repo.path().display().to_string(),
            serde_json::json!({ "ref": head }),
        )];

        let graph = resolve_runtime_dependency_graph(&contracts).expect("resolved graph");
        let dep = &graph.dependencies[0];
        assert_eq!(dep.resolved_ref.as_deref(), Some(head.as_str()));
        assert_eq!(dep.declared_ref.as_deref(), Some(head.as_str()));
    }

    #[test]
    fn non_git_path_materializes_without_provenance() {
        let dir = tempfile::tempdir().expect("dir");
        fs::write(dir.path().join("file.txt"), "x").expect("write");

        let contracts = vec![contract(
            "runtime-helper",
            &dir.path().display().to_string(),
            Value::Null,
        )];

        let graph = resolve_runtime_dependency_graph(&contracts).expect("resolved graph");
        let dep = &graph.dependencies[0];
        assert!(!dep.git_provenance);
        assert_eq!(dep.resolved_ref, None);
    }

    #[test]
    fn opaque_contracts_without_path_are_skipped() {
        let contracts = vec![AgentTaskComponentContract {
            slug: Some("opaque".to_string()),
            path: None,
            load_as: None,
            activate: None,
            extra: serde_json::json!({ "hint": "preserve" })
                .as_object()
                .cloned()
                .unwrap_or_default(),
        }];

        let graph = resolve_runtime_dependency_graph(&contracts).expect("resolved graph");
        assert!(graph.is_empty());
    }

    #[test]
    fn resolved_graph_serializes_for_evidence() {
        let repo = tempfile::tempdir().expect("repo");
        init_repo(repo.path());
        commit_file(repo.path(), "a.txt", "a");

        let contracts = vec![contract(
            "workspace-registry",
            &repo.path().display().to_string(),
            Value::Null,
        )];

        let graph = resolve_runtime_dependency_graph(&contracts).expect("resolved graph");
        let value = graph.to_evidence_value();
        assert_eq!(value["schema"], RUNTIME_DEPENDENCY_GRAPH_SCHEMA);
        assert_eq!(value["dependencies"][0]["slug"], "workspace-registry");
        assert!(value["dependencies"][0]["git_provenance"]
            .as_bool()
            .unwrap());
    }
}
