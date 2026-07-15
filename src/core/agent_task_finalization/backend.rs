use super::{
    AgentTaskPrDurableGateProof, AgentTaskPrFinalizationBackend, AgentTaskPrFinalizationOptions,
    AgentTaskPrRef,
};
use crate::core::agent_task_promotion::{AgentTaskPromotionCandidate, AgentTaskPromotionReport};
use crate::core::error::{Error, Result};
use crate::core::git::{
    commit_at, get_head_commit, get_uncommitted_changes, pr_create, pr_edit, pr_find, push_at,
    remote_branch_commit, resolve_default_remote, run_git, CommitOptions, PrCreateOptions,
    PrEditOptions, PrFindOptions, PrState, PushOptions,
};
use crate::core::run_lifecycle_record::RunLifecycleRecord;
use serde::de::DeserializeOwned;
use std::path::Path;

pub struct RealAgentTaskPrFinalizationBackend;

impl AgentTaskPrFinalizationBackend for RealAgentTaskPrFinalizationBackend {
    fn hydrate_run(&mut self, run_id: &str) -> Result<RunLifecycleRecord> {
        Ok(crate::core::agent_task_lifecycle::status(run_id)?.lifecycle)
    }

    fn hydrate_gate_proof(&mut self, run_id: &str) -> Result<AgentTaskPrDurableGateProof> {
        let record = crate::core::agent_task_lifecycle::status(run_id)?;
        let promotion = record.metadata.get("latest_promotion").cloned().ok_or_else(|| Error::validation_invalid_argument("run_id", "normal finalization requires the run's persisted applied promotion; run agent-task promote first or use --manual-finalization", None, None))?;
        let promotion: AgentTaskPromotionReport =
            serde_json::from_value(promotion).map_err(|_| {
                Error::validation_invalid_argument(
                    "run_id",
                    "durable latest promotion record is invalid",
                    None,
                    None,
                )
            })?;
        Ok(AgentTaskPrDurableGateProof {
            run_id: record.run_id,
            promotion,
        })
    }

    fn validate_candidate(&mut self, options: &AgentTaskPrFinalizationOptions) -> Result<()> {
        validate_real_candidate_fingerprint(options)
    }

    fn current_branch(&mut self, path: &str) -> Result<String> {
        let output = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(path)
            .output()
            .map_err(|error| Error::git_command_failed(error.to_string()))?;
        if !output.status.success() {
            return Err(Error::git_command_failed(format!(
                "git rev-parse failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn changed_files(&mut self, path: &str) -> Result<Vec<String>> {
        let changes = get_uncommitted_changes(path)?;
        let mut files = changes.staged;
        files.extend(changes.unstaged);
        files.extend(changes.untracked);
        files.sort();
        files.dedup();
        Ok(files)
    }

    fn changed_files_since(&mut self, path: &str, base: &str) -> Result<Vec<String>> {
        changed_files_since_remote_branch(path, base)
    }

    fn branch_is_published(&mut self, path: &str, head: &str) -> Result<bool> {
        let local_commit = get_head_commit(path)?;
        Ok(remote_branch_commit(path, head)?
            .is_some_and(|remote_commit| remote_commit == local_commit))
    }

    fn commit_all(&mut self, path: &str, message: &str) -> Result<()> {
        let output = commit_at(None, Some(message), CommitOptions::default(), Some(path))?;
        if !output.success {
            return Err(Error::git_command_failed(format!(
                "git commit failed: {}",
                output.stderr
            )));
        }
        Ok(())
    }

    fn push_branch(&mut self, path: &str, head: &str) -> Result<()> {
        let output = push_at(
            None,
            PushOptions {
                refspec: Some(format!("HEAD:refs/heads/{}", head)),
                ..Default::default()
            },
            Some(path),
        )?;
        if !output.success {
            return Err(Error::git_command_failed(format!(
                "git push failed: {}",
                output.stderr
            )));
        }
        Ok(())
    }

    fn find_open_pr(
        &mut self,
        path: &str,
        base: &str,
        head: &str,
    ) -> Result<Option<AgentTaskPrRef>> {
        let output = pr_find(
            None,
            PrFindOptions {
                base: Some(base.to_string()),
                head: Some(head.to_string()),
                state: PrState::Open,
                limit: 10,
                path: Some(path.to_string()),
            },
        )?;
        Ok(output.items.into_iter().next().map(|item| AgentTaskPrRef {
            number: item.number,
            url: item.url,
        }))
    }

    fn create_pr(
        &mut self,
        path: &str,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<AgentTaskPrRef> {
        let output = pr_create(
            None,
            PrCreateOptions {
                base: base.to_string(),
                head: head.to_string(),
                title: title.to_string(),
                body: body.to_string(),
                draft: false,
                path: Some(path.to_string()),
            },
        )?;
        Ok(AgentTaskPrRef {
            number: output.number.unwrap_or_default(),
            url: output.url.unwrap_or_default(),
        })
    }

    fn update_pr(
        &mut self,
        path: &str,
        number: u64,
        title: &str,
        body: &str,
    ) -> Result<AgentTaskPrRef> {
        let output = pr_edit(
            None,
            PrEditOptions {
                number,
                title: Some(title.to_string()),
                body: Some(body.to_string()),
                path: Some(path.to_string()),
            },
        )?;
        Ok(AgentTaskPrRef {
            number,
            url: output.url.unwrap_or_default(),
        })
    }
}

fn changed_files_since_remote_branch(path: &str, base: &str) -> Result<Vec<String>> {
    let root = Path::new(path);
    run_git(
        root,
        &["check-ref-format", "--branch", base],
        "validate PR base branch",
    )?;
    let remote = resolve_default_remote(root);
    let base_ref = format!("refs/heads/{base}");
    run_git(
        root,
        &["fetch", "--no-tags", &remote, &base_ref],
        "fetch PR base branch",
    )?;
    Ok(run_git(
        root,
        &["diff", "--name-only", "FETCH_HEAD...HEAD"],
        "discover changes from PR base branch",
    )?
    .lines()
    .map(str::to_string)
    .collect())
}

pub(super) fn validate_real_candidate_fingerprint(
    options: &AgentTaskPrFinalizationOptions,
) -> Result<()> {
    let record = crate::core::agent_task_lifecycle::status(&options.run_id)?;
    let promotion: AgentTaskPromotionReport = deserialize_persisted_value(
        record.metadata.get("latest_promotion").cloned(),
        "normal finalization requires persisted latest_promotion",
        "persisted latest_promotion is invalid",
    )?;
    let expected: AgentTaskPromotionCandidate = deserialize_persisted_value(
        promotion.provenance.get("candidate").cloned(),
        "applied promotion is missing a candidate capability; rerun promotion before normal finalization or use --manual-finalization to record the explicit bypass",
        "persisted candidate capability is invalid",
    )?;
    if !matches!(expected, AgentTaskPromotionCandidate::Git { .. }) {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "normal GitHub PR finalization requires an exact Git candidate fingerprint; the applied promotion target was not a Git worktree. Rerun promotion into a Git worktree or use --manual-finalization to record the explicit provenance bypass",
            None,
            None,
        ));
    }
    validate_candidate_fingerprint(options, &expected)
}

pub(super) fn validate_candidate_fingerprint(
    options: &AgentTaskPrFinalizationOptions,
    expected: &AgentTaskPromotionCandidate,
) -> Result<()> {
    let actual = crate::core::agent_task_promotion::candidate_fingerprint(&options.path)?;
    let AgentTaskPromotionCandidate::Git { fingerprint } = &actual else {
        return Err(Error::validation_invalid_argument(
            "path",
            "finalization path is not a Git worktree; normal GitHub PR finalization requires the promoted Git candidate",
            Some(options.path.clone()),
            None,
        ));
    };
    if actual != *expected {
        return Err(Error::validation_invalid_argument(
            "path",
            "candidate changed after promotion; rerun promotion gates before finalization",
            None,
            None,
        ));
    }
    let changed_files = normalize_changed_files(&options.changed_files);
    if !changed_files.is_empty() && changed_files != fingerprint.changed_files {
        return Err(Error::validation_invalid_argument(
            "changed-file",
            "caller changed files do not match promoted candidate",
            None,
            None,
        ));
    }
    Ok(())
}

fn normalize_changed_files(changed_files: &[String]) -> Vec<String> {
    let mut normalized = changed_files.to_vec();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn deserialize_persisted_value<T: DeserializeOwned>(
    value: Option<serde_json::Value>,
    missing_message: &str,
    invalid_message: &str,
) -> Result<T> {
    let value = value
        .ok_or_else(|| Error::validation_invalid_argument("run_id", missing_message, None, None))?;
    serde_json::from_value(value)
        .map_err(|_| Error::validation_invalid_argument("run_id", invalid_message, None, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("runs git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_output(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("runs git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("git output is UTF-8")
    }

    #[test]
    fn changed_files_since_remote_branch_ignores_a_stale_local_base() {
        let root = tempdir().expect("temporary directory");
        let remote = root.path().join("remote.git");
        let source = root.path().join("source");
        let checkout = root.path().join("checkout");
        git(
            root.path(),
            &["init", "--bare", remote.to_str().expect("remote path")],
        );
        std::fs::create_dir(&source).expect("source directory");
        git(&source, &["init", "--initial-branch=main"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "Test User"]);
        std::fs::write(source.join("base.txt"), "base\n").expect("base file");
        git(&source, &["add", "base.txt"]);
        git(&source, &["commit", "-m", "base"]);
        git(
            &source,
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().expect("remote path"),
            ],
        );
        git(&source, &["push", "-u", "origin", "main"]);
        git(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);

        git(
            root.path(),
            &[
                "clone",
                remote.to_str().expect("remote path"),
                checkout.to_str().expect("checkout path"),
            ],
        );
        git(&checkout, &["remote", "rename", "origin", "upstream"]);
        git(&checkout, &["config", "user.email", "test@example.com"]);
        git(&checkout, &["config", "user.name", "Test User"]);

        std::fs::write(source.join("upstream.txt"), "upstream\n").expect("upstream file");
        git(&source, &["add", "upstream.txt"]);
        git(&source, &["commit", "-m", "upstream change"]);
        git(&source, &["push", "origin", "main"]);

        git(&checkout, &["fetch", "upstream", "main"]);
        git(&checkout, &["switch", "-c", "fix/manual", "FETCH_HEAD"]);
        std::fs::write(checkout.join("candidate.txt"), "candidate\n").expect("candidate file");
        git(&checkout, &["add", "candidate.txt"]);
        git(&checkout, &["commit", "-m", "candidate change"]);

        assert_eq!(
            git_output(&checkout, &["diff", "--name-only", "main...HEAD"])
                .lines()
                .collect::<Vec<_>>(),
            vec!["candidate.txt", "upstream.txt"]
        );
        assert_eq!(
            changed_files_since_remote_branch(checkout.to_str().expect("checkout path"), "main")
                .expect("discovers remote-base changes"),
            vec!["candidate.txt"]
        );
    }
}
