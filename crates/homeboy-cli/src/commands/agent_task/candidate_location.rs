//! Which repository holds a Cook's candidate commit (#18856).
//!
//! `candidate_ref` and `candidate_sha` are only actionable when the operator
//! knows the git directory that contains them. Status, diagnose and the stable
//! summary resolve that location from the Cook's own durable records (never
//! the caller's cwd) and prove it with `git cat-file -e` before reporting it.

use std::path::{Path, PathBuf};

use homeboy::agents::agent_task_lifecycle::AgentTaskRunRecord;
use homeboy::core::engine::shell::quote_arg;
use serde_json::{json, Value};

/// One place the Cook's durable records say its candidate may live.
struct CandidateRepoProbe {
    source: &'static str,
    path: PathBuf,
}

fn git(path: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The candidate commit the record names: the harvested commit first, then
/// the promotion report's candidate.
fn candidate_sha(record: &AgentTaskRunRecord) -> Option<String> {
    record
        .candidate
        .as_ref()
        .map(|candidate| candidate.candidate_sha.clone())
        .or_else(|| {
            promotion(record)
                .and_then(|promotion| promotion.pointer("/candidate/candidate_sha"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|sha| !sha.trim().is_empty())
}

fn promotion(record: &AgentTaskRunRecord) -> Option<&Value> {
    record
        .metadata
        .pointer("/cook_delivery/artifacts/promotion")
}

/// Ordered by authority: where the ref was pinned, the destination worktree,
/// the recipe's source worktree, the recorded source checkout (and the main
/// worktree sharing its object store), then the registered component.
fn probes(record: &AgentTaskRunRecord, recipe_source: Option<&str>) -> Vec<CandidateRepoProbe> {
    let mut probes = Vec::new();
    let mut push = |source: &'static str, path: Option<&str>| {
        if let Some(path) = path.filter(|path| !path.trim().is_empty()) {
            probes.push(CandidateRepoProbe {
                source,
                path: PathBuf::from(path),
            });
        }
    };
    let promotion = promotion(record);
    push(
        "promotion_candidate_repo_path",
        promotion
            .and_then(|promotion| promotion.get("candidate_repo_path"))
            .and_then(Value::as_str),
    );
    push(
        "promotion_target",
        promotion
            .and_then(|promotion| promotion.pointer("/target/path"))
            .and_then(Value::as_str),
    );
    push("recipe_source_worktree", recipe_source);
    push(
        "source_checkout",
        record
            .metadata
            .pointer("/source_checkout/local_path")
            .and_then(Value::as_str),
    );
    let component_id = record
        .metadata
        .pointer("/cook_repository_identity/component_id")
        .and_then(Value::as_str)
        .or_else(|| record.metadata["repo"].as_str());
    if let Some(component) = component_id.and_then(|id| homeboy::core::component::load(id).ok()) {
        probes.push(CandidateRepoProbe {
            source: "registered_component",
            path: PathBuf::from(component.local_path),
        });
    }
    probes
}

fn holds_commit(path: &Path, sha: &str) -> bool {
    path.is_dir() && git(path, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_some()
}

/// The worktree root for `path`, so the reported location is a directory an
/// operator can `git -C` into from any cwd.
fn worktree_root(path: &Path) -> PathBuf {
    git(path, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .or_else(|| path.canonicalize().ok())
        .unwrap_or_else(|| path.to_path_buf())
}

/// Resolve the candidate location from the record and an optional recipe
/// source worktree. Returns `None` when the record names no candidate commit.
pub(crate) fn candidate_location_from(
    record: &AgentTaskRunRecord,
    recipe_source: Option<&str>,
) -> Option<Value> {
    let sha = candidate_sha(record)?;
    let reference = promotion(record)
        .and_then(|promotion| promotion.get("candidate_ref"))
        .and_then(Value::as_str);
    let probes = probes(record, recipe_source);
    let recorded = probes.first().map(|probe| worktree_root(&probe.path));
    let Some(found) = probes.iter().find(|probe| holds_commit(&probe.path, &sha)) else {
        return Some(json!({
            "candidate_repo_path": Value::Null,
            "candidate_sha": sha,
            "candidate_repo_unavailable": {
                "reason": "no repository recorded for this Cook contains the candidate commit",
                "tried": probes
                    .iter()
                    .map(|probe| json!({"source": probe.source, "path": probe.path}))
                    .collect::<Vec<_>>(),
            },
        }));
    };
    let root = worktree_root(&found.path);
    let display = root.to_string_lossy().into_owned();
    let ref_resolves = reference.is_some_and(|reference| {
        git(&root, &["rev-parse", "--verify", "--quiet", reference]).is_some()
    });
    let mut location = json!({
        "candidate_repo_path": display,
        "candidate_repo_source": found.source,
        "candidate_inspect_command": format!(
            "git -C {} show {}",
            quote_arg(&display),
            quote_arg(&sha)
        ),
    });
    if let Some(reference) = reference {
        location["candidate_ref_resolves"] = json!(ref_resolves);
        if ref_resolves {
            location["candidate_ref_inspect_command"] = json!(format!(
                "git -C {} show {}",
                quote_arg(&display),
                quote_arg(reference)
            ));
        }
    }
    if let Some(git_dir) = git(
        &root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ) {
        location["candidate_git_common_dir"] = json!(git_dir);
    }
    if recorded.as_ref().is_some_and(|recorded| recorded != &root) {
        location["candidate_repo_note"] = json!(
            "The candidate was not found where the Cook recorded its destination; this location was resolved from the Cook's other recorded checkouts."
        );
    }
    Some(location)
}

/// Resolve the candidate location for a record, reading the recipe's source
/// worktree when one exists.
pub(crate) fn candidate_location(record: &AgentTaskRunRecord) -> Option<Value> {
    candidate_sha(record)?;
    let recipe = homeboy::agents::agent_task_service::load_recipe_for_attempt(&record.run_id)
        .unwrap_or(None);
    let recipe_source = recipe
        .as_ref()
        .and_then(|recipe| recipe.finalization["source_worktree_path"].as_str());
    candidate_location_from(record, recipe_source)
}

/// Copy the resolved location fields onto a status or diagnose payload.
pub(crate) fn attach_candidate_location(value: &mut Value, record: &AgentTaskRunRecord) {
    let Some(Value::Object(location)) = candidate_location(record) else {
        return;
    };
    for (key, field) in location {
        if key == "candidate_sha" && !value["candidate_sha"].is_null() {
            continue;
        }
        value[key] = field;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent_task::tests::support::with_isolated_home;

    fn run_git(path: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn repo(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        run_git(path, &["init", "-q"]);
        run_git(path, &["config", "user.email", "t@example.com"]);
        run_git(path, &["config", "user.name", "t"]);
        run_git(path, &["commit", "-q", "--allow-empty", "-m", "base"]);
    }

    /// A real durable lifecycle record, so the projection reads exactly what
    /// status reads.
    fn record_with(metadata: Value) -> AgentTaskRunRecord {
        let run_id = "cook-attempt-1-a";
        let plan = crate::commands::agent_task::tests::support::test_plan();
        homeboy::agents::agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
        homeboy::agents::agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata = metadata.clone();
        })
        .unwrap()
    }

    #[test]
    fn candidate_repo_path_resolves_source_checkout_independent_of_cwd() {
        with_isolated_home(|home| {
            // An unrelated checkout recorded as the destination lacks the object.
            let unrelated = home.path().join("unrelated-issue-worktree");
            repo(&unrelated);
            // The Cook's own source checkout holds the candidate commit.
            let source = home.path().join("cook-source");
            repo(&source);
            std::fs::write(source.join("change.txt"), "candidate").unwrap();
            run_git(&source, &["add", "change.txt"]);
            run_git(&source, &["commit", "-q", "-m", "candidate"]);
            let sha = run_git(&source, &["rev-parse", "HEAD"]);
            // The caller's cwd is a third repository that knows nothing of it.
            let caller = home.path().join("caller-cwd");
            repo(&caller);
            assert!(std::process::Command::new("git")
                .current_dir(&caller)
                .args(["cat-file", "-e", &sha])
                .status()
                .unwrap()
                .code()
                .is_some_and(|code| code != 0));

            let record = record_with(json!({
                "cook_id": "cook",
                "source_checkout": {"local_path": source},
                "cook_delivery": {"artifacts": {"promotion": {
                    "target": {"path": unrelated},
                    "candidate_ref": "refs/homeboy/candidates/cook",
                    "candidate": {"candidate_sha": sha},
                }}},
            }));
            let location = candidate_location_from(&record, None).unwrap();
            let path = location["candidate_repo_path"].as_str().unwrap();
            assert_eq!(
                Path::new(path).canonicalize().unwrap(),
                source.canonicalize().unwrap()
            );
            assert_eq!(location["candidate_repo_source"], "source_checkout");
            assert!(location["candidate_repo_note"].is_string());
            assert_eq!(location["candidate_ref_resolves"], false);
            // The reported path proves the object from the caller's cwd.
            assert!(std::process::Command::new("git")
                .current_dir(&caller)
                .args(["-C", path, "cat-file", "-e", &sha])
                .status()
                .unwrap()
                .success());
            assert!(location["candidate_inspect_command"]
                .as_str()
                .unwrap()
                .starts_with("git -C "));
        });
    }

    #[test]
    fn candidate_repo_path_prefers_pinned_destination_with_ref() {
        with_isolated_home(|home| {
            let destination = home.path().join("destination");
            repo(&destination);
            let sha = run_git(&destination, &["rev-parse", "HEAD"]);
            run_git(
                &destination,
                &["update-ref", "refs/homeboy/candidates/cook", &sha],
            );
            let record = record_with(json!({
                "cook_delivery": {"artifacts": {"promotion": {
                    "target": {"path": destination},
                    "candidate_ref": "refs/homeboy/candidates/cook",
                    "candidate": {"candidate_sha": sha},
                }}},
            }));
            let location = candidate_location_from(&record, None).unwrap();
            assert_eq!(location["candidate_repo_source"], "promotion_target");
            assert_eq!(location["candidate_ref_resolves"], true);
            assert!(location.get("candidate_repo_note").is_none());
            assert!(location["candidate_ref_inspect_command"].is_string());
        });
    }

    #[test]
    fn missing_candidate_object_reports_tried_paths() {
        with_isolated_home(|home| {
            let destination = home.path().join("destination");
            repo(&destination);
            let record = record_with(json!({
                "cook_delivery": {"artifacts": {"promotion": {
                    "target": {"path": destination},
                    "candidate": {"candidate_sha": "0123456789abcdef0123456789abcdef01234567"},
                }}},
            }));
            let location = candidate_location_from(&record, None).unwrap();
            assert!(location["candidate_repo_path"].is_null());
            assert_eq!(
                location["candidate_repo_unavailable"]["tried"][0]["source"],
                "promotion_target"
            );
        });
    }
}
