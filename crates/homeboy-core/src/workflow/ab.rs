//! Built-in A/B parity recipe: `homeboy.ab`.
//!
//! Runs one command at two commits in isolated checkouts, concurrently, and
//! decides whether the declared outputs are byte-identical. The recipe is an
//! ordinary `homeboy/workflow/v1` definition: a two-item `foreach` step runs
//! both sides as supervised step jobs (each leads its own process group, so
//! cancelling the instance stops both), a `verdict` step compares them, and an
//! `always` `cleanup` step removes the checkouts.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

use super::WorkflowDefinition;

pub const AB_WORKFLOW_ID: &str = "homeboy.ab";
pub const AB_SIDE_SCHEMA: &str = "homeboy/workflow-ab-side/v1";
pub const AB_VERDICT_SCHEMA: &str = "homeboy/workflow-ab-verdict/v1";

/// Total unified-diff bytes a verdict carries.
pub const DIFF_LIMIT_BYTES: usize = 64 * 1024;
/// Tail of each side's command log kept in its record.
const LOG_TAIL_BYTES: usize = 4 * 1024;
/// Differing files listed in the PR summary.
const SUMMARY_FILES: usize = 20;

const AB_WORKFLOW: &str = r#"{
  "schema": "homeboy/workflow/v1",
  "id": "homeboy.ab",
  "inputs": {
    "base_repo": { "required": true },
    "base_sha": { "required": true },
    "candidate_repo": { "required": true },
    "candidate_sha": { "required": true },
    "run": { "required": true },
    "compare": { "required": true },
    "isolated": { "default": "false" }
  },
  "steps": [
    { "id": "side", "foreach": ["base", "candidate"], "max_parallel": 2,
      "homeboy": ["workflow", "ab-step", "side", "--side", "{{item}}", "--instance", "{{instance}}",
                  "--base-repo", "{{inputs.base_repo}}", "--base-sha", "{{inputs.base_sha}}",
                  "--candidate-repo", "{{inputs.candidate_repo}}", "--candidate-sha", "{{inputs.candidate_sha}}",
                  "--isolated", "{{inputs.isolated}}",
                  "--run", "{{inputs.run}}", "--compare", "{{inputs.compare}}"] },
    { "id": "verdict", "needs": ["side"],
      "homeboy": ["workflow", "ab-step", "verdict", "--run", "{{inputs.run}}", "--sides", "{{steps.side.output/items}}"] },
    { "id": "cleanup", "needs": ["verdict"], "always": true,
      "homeboy": ["workflow", "ab-step", "cleanup", "--instance", "{{instance}}",
                  "--base-repo", "{{inputs.base_repo}}", "--candidate-repo", "{{inputs.candidate_repo}}",
                  "--isolated", "{{inputs.isolated}}"] }
  ]
}"#;

/// The `homeboy.ab` recipe.
pub fn definition() -> WorkflowDefinition {
    WorkflowDefinition::parse(AB_WORKFLOW).expect("built-in A/B workflow parses")
}

/// Where an instance keeps its checkouts and command logs.
pub fn checkout_root(instance: &str) -> PathBuf {
    std::env::temp_dir().join("homeboy-ab").join(instance)
}

/// One side of an A/B run.
pub struct AbSide<'a> {
    /// `base` or `candidate`.
    pub side: &'a str,
    /// The repository the side's commit lives in.
    pub repo: &'a Path,
    /// The commit (or ref) the side runs at.
    pub sha: &'a str,
    /// `repo` is already a dedicated checkout of `sha` (a Lab workspace):
    /// run there instead of adding a worktree.
    pub isolated: bool,
    pub run: &'a str,
    pub compare: &'a [String],
    pub checkout_root: &'a Path,
    pub run_id: String,
}

/// Check out the side's commit in isolation, assert HEAD, run the command,
/// and hash every declared output file.
pub fn run_side(side: &AbSide<'_>) -> Result<Value> {
    validate_patterns(side.compare)?;
    let sha = git(
        side.repo,
        &["rev-parse", "--verify", &format!("{}^{{commit}}", side.sha)],
    )?;
    std::fs::create_dir_all(side.checkout_root).map_err(|error| io(error, side.checkout_root))?;
    let checkout = if side.isolated {
        side.repo.to_path_buf()
    } else {
        let checkout = side.checkout_root.join(side.side);
        remove_worktree(side.repo, &checkout);
        let target = checkout.display().to_string();
        git(
            side.repo,
            &["worktree", "add", "--detach", "--quiet", &target, &sha],
        )?;
        checkout
    };
    let head = git(&checkout, &["rev-parse", "HEAD"])?;
    if head != sha {
        return Err(Error::validation_invalid_argument(
            "sha",
            format!(
                "{} checkout HEAD is {head}, expected {sha}; refusing to run",
                side.side
            ),
            Some(sha),
            None,
        ));
    }

    let log_path = side.checkout_root.join(format!("{}.log", side.side));
    let log = std::fs::File::create(&log_path).map_err(|error| io(error, &log_path))?;
    let log_err = log.try_clone().map_err(|error| io(error, &log_path))?;
    // The command stays in this process's group: the step job leads that
    // group, so cancelling the step stops the command with it.
    let status = Command::new("sh")
        .arg("-c")
        .arg(side.run)
        .current_dir(&checkout)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .status()
        .map_err(|error| Error::internal_io(error.to_string(), Some("run A/B command".into())))?;

    let (files, unmatched) = collect_files(&checkout, side.compare)?;
    Ok(json!({
        "schema": AB_SIDE_SCHEMA,
        "side": side.side,
        "ref": side.sha,
        "sha": sha,
        "head": head,
        "head_matches": true,
        "run_id": side.run_id,
        "checkout": checkout.display().to_string(),
        "exit_code": status.code(),
        "files": files,
        "unmatched": unmatched,
        "log_tail": log_tail(&log_path),
    }))
}

/// Remove an instance's checkouts. Dedicated Lab workspaces belong to the
/// runner's workspace lifecycle and are left alone.
pub fn cleanup(repos: &[&Path], isolated: bool, checkout_root: &Path) -> Value {
    let mut removed = Vec::new();
    if !isolated {
        for (repo, side) in repos.iter().zip(["base", "candidate"]) {
            let checkout = checkout_root.join(side);
            if checkout.exists() {
                remove_worktree(repo, &checkout);
                removed.push(checkout.display().to_string());
            }
        }
    }
    let _ = std::fs::remove_dir_all(checkout_root);
    json!({ "removed": removed, "checkout_root": checkout_root.display().to_string() })
}

/// Compare the two side records: `identical` when every declared output file
/// matches byte for byte and the commands exited alike, otherwise `different`
/// with the differing files and a bounded unified diff.
///
/// `sides` is the `side` step's `items` (each with an `output` record) or the
/// side records themselves.
pub fn verdict(sides: &Value, run: &str) -> Result<Value> {
    let records = sides
        .as_array()
        .ok_or_else(|| invalid("sides", "expected a JSON array of side records"))?
        .iter()
        .map(|entry| {
            if entry["schema"] == AB_SIDE_SCHEMA {
                entry
            } else {
                &entry["output"]
            }
        })
        .collect::<Vec<_>>();
    let pick = |name: &str| {
        records
            .iter()
            .find(|record| record["side"] == name)
            .copied()
            .ok_or_else(|| invalid("sides", format!("no `{name}` side record")))
    };
    let base = pick("base")?;
    let candidate = pick("candidate")?;
    let files = |record: &Value| -> BTreeMap<String, String> {
        record["files"]
            .as_object()
            .map(|files| {
                files
                    .iter()
                    .filter_map(|(path, sha)| Some((path.clone(), sha.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default()
    };
    let base_files = files(base);
    let candidate_files = files(candidate);
    let paths = base_files
        .keys()
        .chain(candidate_files.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    if paths.is_empty() {
        return Err(invalid(
            "compare",
            "the compare paths matched no files on either side",
        ));
    }
    let differing = paths
        .iter()
        .filter(|path| base_files.get(*path) != candidate_files.get(*path))
        .map(|path| {
            json!({
                "path": path,
                "base_sha256": base_files.get(path),
                "candidate_sha256": candidate_files.get(path),
            })
        })
        .collect::<Vec<_>>();
    let exit_codes_match = base["exit_code"] == candidate["exit_code"];
    let identical = differing.is_empty() && exit_codes_match;
    let (diff, diff_truncated) = bounded_diff(base, candidate, &differing);
    let mut verdict = json!({
        "schema": AB_VERDICT_SCHEMA,
        "verdict": if identical { "identical" } else { "different" },
        "run": run,
        "compared_files": paths.len(),
        "exit_codes_match": exit_codes_match,
        "differing": differing,
        "diff": diff,
        "diff_truncated": diff_truncated,
        "base": side_summary(base, base_files),
        "candidate": side_summary(candidate, candidate_files),
    });
    verdict["summary"] = json!(render_summary(&verdict));
    Ok(verdict)
}

/// A short Markdown summary of a verdict that pastes into a PR body.
pub fn render_summary(verdict: &Value) -> String {
    let short = |side: &str| {
        verdict[side]["sha"]
            .as_str()
            .map(|sha| sha.chars().take(12).collect::<String>())
            .unwrap_or_default()
    };
    let run_id = |side: &str| verdict[side]["run_id"].as_str().unwrap_or("-").to_string();
    let mut out = format!(
        "**A/B parity: {}** (`homeboy workflow ab`)\n\n- base `{}` (run `{}`), candidate `{}` (run `{}`)\n- command: `{}`\n",
        verdict["verdict"].as_str().unwrap_or("unknown"),
        short("base"),
        run_id("base"),
        short("candidate"),
        run_id("candidate"),
        verdict["run"].as_str().unwrap_or_default(),
    );
    let compared = verdict["compared_files"].as_u64().unwrap_or(0);
    let differing = verdict["differing"].as_array().cloned().unwrap_or_default();
    if differing.is_empty() {
        out.push_str(&format!(
            "- {compared} output file(s) byte-identical (sha256)\n"
        ));
    } else {
        out.push_str(&format!(
            "- {} of {compared} output file(s) differ:\n",
            differing.len()
        ));
        for entry in differing.iter().take(SUMMARY_FILES) {
            out.push_str(&format!(
                "  - `{}`\n",
                entry["path"].as_str().unwrap_or_default()
            ));
        }
        if differing.len() > SUMMARY_FILES {
            out.push_str(&format!(
                "  - … and {} more\n",
                differing.len() - SUMMARY_FILES
            ));
        }
    }
    if verdict["exit_codes_match"] == false {
        out.push_str(&format!(
            "- exit codes differ: base {}, candidate {}\n",
            verdict["base"]["exit_code"], verdict["candidate"]["exit_code"]
        ));
    }
    out
}

fn side_summary(record: &Value, files: BTreeMap<String, String>) -> Value {
    json!({
        "ref": record["ref"],
        "sha": record["sha"],
        "head_matches": record["head_matches"],
        "run_id": record["run_id"],
        "exit_code": record["exit_code"],
        "unmatched": record["unmatched"],
        "files": files,
    })
}

fn bounded_diff(base: &Value, candidate: &Value, differing: &[Value]) -> (String, bool) {
    let base_root = base["checkout"].as_str().unwrap_or_default();
    let candidate_root = candidate["checkout"].as_str().unwrap_or_default();
    let mut diff = String::new();
    for entry in differing {
        let path = entry["path"].as_str().unwrap_or_default();
        let side_path = |root: &str, present: bool| {
            if present {
                Path::new(root).join(path).display().to_string()
            } else {
                "/dev/null".to_string()
            }
        };
        let left = side_path(base_root, !entry["base_sha256"].is_null());
        let right = side_path(candidate_root, !entry["candidate_sha256"].is_null());
        let Ok(output) = Command::new("git")
            .args(["diff", "--no-index", "--no-color", "--no-prefix", "--"])
            .args([&left, &right])
            .output()
        else {
            continue;
        };
        let text = String::from_utf8_lossy(&output.stdout)
            .replace(&format!("{base_root}/"), "base/")
            .replace(&format!("{candidate_root}/"), "candidate/");
        diff.push_str(&text);
        if diff.len() > DIFF_LIMIT_BYTES {
            let mut end = DIFF_LIMIT_BYTES;
            while !diff.is_char_boundary(end) {
                end -= 1;
            }
            diff.truncate(end);
            return (diff, true);
        }
    }
    (diff, false)
}

/// Compare patterns are relative to the checkout and stay inside it.
fn validate_patterns(patterns: &[String]) -> Result<()> {
    if patterns.is_empty() {
        return Err(invalid("compare", "declare at least one --compare path"));
    }
    for pattern in patterns {
        let path = Path::new(pattern);
        if path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
            return Err(invalid(
                "compare",
                format!("`{pattern}` must be a path or glob relative to the checkout"),
            ));
        }
    }
    Ok(())
}

/// Every file the patterns name, relative to `root`, with its sha256. A
/// directory contributes every file under it; `.git` is never compared.
fn collect_files(
    root: &Path,
    patterns: &[String],
) -> Result<(BTreeMap<String, String>, Vec<String>)> {
    let mut files = BTreeMap::new();
    let mut unmatched = Vec::new();
    for pattern in patterns {
        let direct = root.join(pattern);
        let candidates = if direct.exists() {
            vec![direct]
        } else {
            let glob_pattern = direct.display().to_string();
            glob::glob(&glob_pattern)
                .map_err(|error| invalid("compare", format!("`{pattern}`: {error}")))?
                .filter_map(std::result::Result::ok)
                .collect()
        };
        let mut matched = Vec::new();
        for candidate in candidates {
            walk(&candidate, &mut matched);
        }
        let mut matched_any = false;
        for path in matched {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            if relative.components().any(|part| part.as_os_str() == ".git") {
                continue;
            }
            files.insert(
                relative.to_string_lossy().replace('\\', "/"),
                sha256_file(&path)?,
            );
            matched_any = true;
        }
        if !matched_any {
            unmatched.push(pattern.clone());
        }
    }
    Ok((files, unmatched))
}

fn walk(path: &Path, out: &mut Vec<PathBuf>) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if metadata.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                walk(&entry.path(), out);
            }
        }
    } else if path.is_file() {
        out.push(path.to_path_buf());
    }
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).map_err(|error| io(error, path))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| io(error, path))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn log_tail(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    let start = bytes.len().saturating_sub(LOG_TAIL_BYTES);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

fn remove_worktree(repo: &Path, checkout: &Path) {
    let target = checkout.display().to_string();
    let _ = git(repo, &["worktree", "remove", "--force", &target]);
    let _ = std::fs::remove_dir_all(checkout);
    let _ = git(repo, &["worktree", "prune"]);
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| Error::internal_io(error.to_string(), Some("run git".into())))?;
    if !output.status.success() {
        return Err(Error::internal_unexpected(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn io(error: std::io::Error, path: &Path) -> Error {
    Error::internal_io(error.to_string(), Some(path.display().to_string()))
}

fn invalid(field: &str, message: impl Into<String>) -> Error {
    Error::validation_invalid_argument(field, message.into(), None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_in(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// A repo whose two commits change `gen.sh`'s output only when
    /// `change_output` is set.
    fn repo(dir: &Path, change_output: bool) -> (String, String) {
        git_in(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("gen.sh"), "mkdir -p out\necho hello > out/a.txt\n").unwrap();
        git_in(dir, &["add", "."]);
        git_in(dir, &["commit", "-qm", "base"]);
        let base = git_in(dir, &["rev-parse", "HEAD"]);
        let script = if change_output {
            "mkdir -p out\necho goodbye > out/a.txt\n"
        } else {
            "# refactored\nmkdir -p out\necho hello > out/a.txt\n"
        };
        std::fs::write(dir.join("gen.sh"), script).unwrap();
        git_in(dir, &["commit", "-qam", "candidate"]);
        let candidate = git_in(dir, &["rev-parse", "HEAD"]);
        (base, candidate)
    }

    fn run_both(dir: &Path, base: &str, candidate: &str) -> Value {
        let root = dir.join("ab-root");
        let compare = vec!["out".to_string()];
        let sides = [("base", base), ("candidate", candidate)]
            .iter()
            .map(|(side, sha)| {
                run_side(&AbSide {
                    side,
                    repo: &dir.join("repo"),
                    sha,
                    isolated: false,
                    run: "sh gen.sh",
                    compare: &compare,
                    checkout_root: &root,
                    run_id: format!("run-{side}"),
                })
                .unwrap()
            })
            .collect::<Vec<_>>();
        let verdict = verdict(&Value::Array(sides), "sh gen.sh").unwrap();
        let repo = dir.join("repo");
        cleanup(&[&repo, &repo], false, &root);
        assert!(!root.exists());
        verdict
    }

    #[test]
    fn workflow_ab_matching_outputs_are_identical_with_both_shas() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("repo")).unwrap();
        let (base, candidate) = repo(&dir.path().join("repo"), false);
        let verdict = run_both(dir.path(), &base, &candidate);
        assert_eq!(verdict["verdict"], "identical");
        assert_eq!(verdict["base"]["sha"], base);
        assert_eq!(verdict["candidate"]["sha"], candidate);
        assert_eq!(verdict["base"]["head_matches"], true);
        assert_eq!(verdict["base"]["run_id"], "run-base");
        assert_eq!(verdict["candidate"]["run_id"], "run-candidate");
        assert_eq!(
            verdict["base"]["files"]["out/a.txt"],
            verdict["candidate"]["files"]["out/a.txt"]
        );
        assert_eq!(verdict["diff"], "");
        assert!(verdict["summary"]
            .as_str()
            .unwrap()
            .contains("A/B parity: identical"));
    }

    #[test]
    fn workflow_ab_differing_output_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("repo")).unwrap();
        let (base, candidate) = repo(&dir.path().join("repo"), true);
        let verdict = run_both(dir.path(), &base, &candidate);
        assert_eq!(verdict["verdict"], "different");
        assert_eq!(verdict["differing"][0]["path"], "out/a.txt");
        assert_ne!(
            verdict["base"]["files"]["out/a.txt"],
            verdict["candidate"]["files"]["out/a.txt"]
        );
        let diff = verdict["diff"].as_str().unwrap();
        assert!(
            diff.contains("-hello") && diff.contains("+goodbye"),
            "{diff}"
        );
        assert!(diff.contains("base/out/a.txt"), "{diff}");
        assert!(verdict["summary"].as_str().unwrap().contains("`out/a.txt`"));
    }

    #[test]
    fn workflow_ab_recipe_validates_and_rejects_escaping_patterns() {
        definition().validate(&|_| false).expect("valid recipe");
        assert!(validate_patterns(&["../x".to_string()]).is_err());
        assert!(validate_patterns(&["/etc".to_string()]).is_err());
        assert!(validate_patterns(&["out/**/*.html".to_string()]).is_ok());
    }
}
