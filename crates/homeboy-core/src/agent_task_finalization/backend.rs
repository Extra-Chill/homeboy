use super::{
    AgentTaskPrCandidateState, AgentTaskPrDurableGateProof, AgentTaskPrFinalizationBackend,
    AgentTaskPrFinalizationOptions, AgentTaskPrRef, AgentTaskPrResolvedBase,
};
use crate::agent_task_promotion::{AgentTaskPromotionCandidate, AgentTaskPromotionReport};
use crate::error::{Error, Result};
use crate::git::{
    commit_at, get_uncommitted_changes, pr_create, pr_edit, pr_find, push_at, CommitOptions,
    PrCreateOptions, PrEditOptions, PrFindOptions, PrState, PushOptions,
};
use crate::run_lifecycle_record::RunLifecycleRecord;
use serde::de::DeserializeOwned;

pub struct RealAgentTaskPrFinalizationBackend;

impl AgentTaskPrFinalizationBackend for RealAgentTaskPrFinalizationBackend {
    fn hydrate_run(&mut self, run_id: &str) -> Result<RunLifecycleRecord> {
        Ok(crate::agent_task_lifecycle::status(run_id)?.lifecycle)
    }

    fn hydrate_gate_proof(&mut self, run_id: &str) -> Result<AgentTaskPrDurableGateProof> {
        let record = crate::agent_task_lifecycle::status(run_id)?;
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

    fn resolve_base(&mut self, path: &str, base: &str) -> Result<AgentTaskPrResolvedBase> {
        let reference = format!("refs/homeboy/finalization/base/{base}");
        let output = std::process::Command::new("git")
            .args([
                "fetch",
                "--no-tags",
                "origin",
                &format!("refs/heads/{base}:{reference}"),
            ])
            .current_dir(path)
            .output()
            .map_err(|error| Error::git_command_failed(error.to_string()))?;
        if !output.status.success() {
            return Err(Error::validation_invalid_argument(
                "base",
                &format!(
                    "could not fetch requested base `refs/heads/{base}` from origin; verify remote access and that the branch exists, then retry: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                None,
                None,
            ));
        }
        let sha = git_output(
            path,
            &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
        )?;
        Ok(AgentTaskPrResolvedBase { reference, sha })
    }

    fn resolve_verified_base(
        &mut self,
        path: &str,
        verified_base_sha: &str,
    ) -> Result<AgentTaskPrResolvedBase> {
        if !is_git_object_id(verified_base_sha) {
            return Err(Error::validation_invalid_argument(
                "verified_base_sha",
                "verified base snapshot must be a full Git object ID",
                Some(verified_base_sha.to_string()),
                None,
            ));
        }
        let sha = git_output(
            path,
            &[
                "rev-parse",
                "--verify",
                &format!("{verified_base_sha}^{{commit}}"),
            ],
        )
        .or_else(|_| {
            let fetch = std::process::Command::new("git")
                .args([
                    "fetch",
                    "--no-tags",
                    "--no-write-fetch-head",
                    "origin",
                    verified_base_sha,
                ])
                .current_dir(path)
                .output()
                .map_err(|error| Error::git_command_failed(error.to_string()))?;
            if !fetch.status.success() {
                return Err(Error::validation_invalid_argument(
                    "verified_base_sha",
                    format!(
                        "verified base snapshot is unavailable locally and origin could not materialize exact commit `{verified_base_sha}`; retry from a worktree with origin access: {}",
                        String::from_utf8_lossy(&fetch.stderr).trim()
                    ),
                    Some(verified_base_sha.to_string()),
                    None,
                ));
            }
            git_output(
                path,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("{verified_base_sha}^{{commit}}"),
                ],
            )
            .map_err(|_| {
                Error::validation_invalid_argument(
                    "verified_base_sha",
                    "origin did not materialize the persisted verified base snapshot as a commit",
                    Some(verified_base_sha.to_string()),
                    None,
                )
            })
        })?;
        if sha != verified_base_sha {
            return Err(Error::validation_invalid_argument(
                "verified_base_sha",
                "verified base snapshot did not resolve to the supplied immutable Git object ID",
                Some(verified_base_sha.to_string()),
                None,
            ));
        }
        Ok(AgentTaskPrResolvedBase {
            reference: verified_base_sha.to_string(),
            sha,
        })
    }

    fn publication_base_sha(&mut self, path: &str, base: &str) -> Result<Option<String>> {
        let output = std::process::Command::new("git")
            .args([
                "ls-remote",
                "--heads",
                "origin",
                &format!("refs/heads/{base}"),
            ])
            .current_dir(path)
            .output()
            .map_err(|error| Error::git_command_failed(error.to_string()))?;
        if !output.status.success() {
            return Err(Error::git_command_failed(format!(
                "could not observe live origin base `{base}` before publication: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
            .map(str::to_string))
    }

    fn candidate_state(
        &mut self,
        path: &str,
        base: &AgentTaskPrResolvedBase,
        head: &str,
    ) -> Result<AgentTaskPrCandidateState> {
        let base_ref = &base.reference;
        let counts = git_output(
            path,
            &[
                "rev-list",
                "--left-right",
                "--count",
                &format!("{}...HEAD", base_ref),
            ],
        )?;
        let Some((behind, ahead)) = counts
            .split_whitespace()
            .collect::<Vec<_>>()
            .get(..2)
            .and_then(|parts| Some((parts[0].parse::<u64>().ok()?, parts[1].parse::<u64>().ok()?)))
        else {
            return Ok(AgentTaskPrCandidateState::Invalid {
                diagnostic: format!(
                    "could not compare resolved base `{base_ref}` at `{}` to HEAD",
                    base.sha
                ),
            });
        };
        if behind != 0 {
            return Ok(AgentTaskPrCandidateState::Invalid {
                diagnostic: format!("HEAD is behind or diverged from resolved base `{base_ref}` at `{}` ({behind} base-only commit(s)); rebase or merge that ref before finalizing", base.sha),
            });
        }
        let dirty = self.changed_files(path)?;
        if !dirty.is_empty() {
            return Ok(AgentTaskPrCandidateState::Dirty {
                changed_files: dirty,
            });
        }
        if ahead == 0 {
            return Ok(AgentTaskPrCandidateState::Equivalent);
        }

        let changed_files = committed_changed_files(path, base_ref)?;
        let local_head = git_output(path, &["rev-parse", "HEAD"])?;
        let remote_head = remote_branch_head(path, head)?;
        Ok(AgentTaskPrCandidateState::Committed {
            changed_files,
            push_required: remote_head.as_deref() != Some(local_head.trim()),
        })
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

fn committed_changed_files(path: &str, base: &str) -> Result<Vec<String>> {
    let output = git_output(
        path,
        &["diff", "--name-status", "-M", &format!("{base}...HEAD")],
    )?;
    let mut files = Vec::new();
    for line in output.lines() {
        let mut fields = line.split('\t');
        let status = fields.next().unwrap_or_default();
        if status.starts_with('R') || status.starts_with('C') {
            files.extend(fields.take(2).map(str::to_string));
        } else if let Some(file) = fields.next() {
            files.push(file.to_string());
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn remote_branch_head(path: &str, head: &str) -> Result<Option<String>> {
    let output = std::process::Command::new("git")
        .args([
            "ls-remote",
            "--heads",
            "origin",
            &format!("refs/heads/{head}"),
        ])
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !output.status.success() {
        return Err(Error::git_command_failed(format!(
            "could not read live origin head `{head}` before publication: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string))
}

fn git_output(path: &str, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !output.status.success() {
        return Err(Error::git_command_failed(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn is_git_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod remote_base_tests {
    use super::*;
    use std::process::Command;

    fn git(path: &std::path::Path, args: &[&str]) {
        assert!(Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("git runs")
            .success());
    }

    fn repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().expect("temp repo");
        git(repo.path(), &["init", "-b", "main"]);
        git(repo.path(), &["config", "user.email", "test@example.com"]);
        git(repo.path(), &["config", "user.name", "Test"]);
        std::fs::write(repo.path().join("old.txt"), "old").expect("base file");
        std::fs::write(repo.path().join("removed.txt"), "removed").expect("base file");
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "base"]);
        repo
    }

    fn base(reference: &str, sha: &str) -> AgentTaskPrResolvedBase {
        AgentTaskPrResolvedBase {
            reference: reference.to_string(),
            sha: sha.to_string(),
        }
    }

    #[test]
    fn classifies_staged_unstaged_and_untracked_files_as_dirty() {
        let repo = repo();
        std::fs::write(repo.path().join("old.txt"), "staged").unwrap();
        git(repo.path(), &["add", "old.txt"]);
        std::fs::write(repo.path().join("removed.txt"), "unstaged").unwrap();
        std::fs::write(repo.path().join("untracked.txt"), "untracked").unwrap();

        let state = RealAgentTaskPrFinalizationBackend
            .candidate_state(
                repo.path().to_str().unwrap(),
                &base("main", "base"),
                "feature",
            )
            .expect("state");

        assert_eq!(
            state,
            AgentTaskPrCandidateState::Dirty {
                changed_files: vec![
                    "old.txt".to_string(),
                    "removed.txt".to_string(),
                    "untracked.txt".to_string(),
                ],
            }
        );
    }

    #[test]
    fn recognizes_equivalent_main_and_trunk_bases() {
        let repo = repo();
        git(repo.path(), &["branch", "trunk"]);
        for base_name in ["main", "trunk"] {
            let state = RealAgentTaskPrFinalizationBackend
                .candidate_state(
                    repo.path().to_str().unwrap(),
                    &base(base_name, "base"),
                    "feature",
                )
                .expect("state");
            assert_eq!(state, AgentTaskPrCandidateState::Equivalent);
        }
    }

    #[test]
    fn classifies_committed_merge_candidate_and_tracks_renames_and_deletes() {
        let repo = repo();
        git(repo.path(), &["checkout", "-b", "feature"]);
        git(repo.path(), &["mv", "old.txt", "renamed.txt"]);
        std::fs::remove_file(repo.path().join("removed.txt")).unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-m", "rename and delete"]);
        git(repo.path(), &["checkout", "main"]);
        std::fs::write(repo.path().join("main.txt"), "main").unwrap();
        git(repo.path(), &["add", "main.txt"]);
        git(repo.path(), &["commit", "-m", "main advance"]);
        git(repo.path(), &["checkout", "feature"]);
        git(repo.path(), &["merge", "main", "--no-edit"]);

        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare"]);
        git(
            repo.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        let state = RealAgentTaskPrFinalizationBackend
            .candidate_state(
                repo.path().to_str().unwrap(),
                &base("main", "base"),
                "feature",
            )
            .expect("state");

        assert_eq!(
            state,
            AgentTaskPrCandidateState::Committed {
                changed_files: vec![
                    "old.txt".to_string(),
                    "removed.txt".to_string(),
                    "renamed.txt".to_string(),
                ],
                push_required: true,
            }
        );
    }

    #[test]
    fn remote_tracking_base_wins_over_stale_local_branch() {
        let repo = repo();
        git(repo.path(), &["checkout", "-b", "origin-base"]);
        std::fs::write(repo.path().join("origin.txt"), "origin").unwrap();
        git(repo.path(), &["add", "origin.txt"]);
        git(repo.path(), &["commit", "-m", "origin advance"]);
        let origin_base =
            git_output(repo.path().to_str().unwrap(), &["rev-parse", "HEAD"]).unwrap();
        git(
            repo.path(),
            &["update-ref", "refs/remotes/origin/main", origin_base.trim()],
        );
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare"]);
        git(
            repo.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(repo.path(), &["checkout", "-b", "feature"]);
        std::fs::write(repo.path().join("feature.txt"), "feature").unwrap();
        git(repo.path(), &["add", "feature.txt"]);
        git(repo.path(), &["commit", "-m", "feature"]);

        let state = RealAgentTaskPrFinalizationBackend
            .candidate_state(
                repo.path().to_str().unwrap(),
                &base("refs/remotes/origin/main", origin_base.trim()),
                "feature",
            )
            .expect("state");

        assert!(matches!(state, AgentTaskPrCandidateState::Committed { .. }));
    }

    #[test]
    fn resolve_base_fetches_a_fresh_immutable_tracking_ref() {
        let repo = repo();
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare"]);
        git(
            repo.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(repo.path(), &["push", "-u", "origin", "main"]);
        let clone = tempfile::tempdir().unwrap();
        assert!(Command::new("git")
            .args([
                "clone",
                "-b",
                "main",
                remote.path().to_str().unwrap(),
                clone.path().to_str().unwrap()
            ])
            .status()
            .unwrap()
            .success());
        git(clone.path(), &["config", "user.email", "test@example.com"]);
        git(clone.path(), &["config", "user.name", "Test"]);
        std::fs::write(clone.path().join("remote.txt"), "fresh").unwrap();
        git(clone.path(), &["add", "remote.txt"]);
        git(clone.path(), &["commit", "-m", "remote advance"]);
        git(clone.path(), &["push", "origin", "main"]);
        let remote_head =
            git_output(clone.path().to_str().unwrap(), &["rev-parse", "HEAD"]).unwrap();

        let resolved = RealAgentTaskPrFinalizationBackend
            .resolve_base(repo.path().to_str().unwrap(), "main")
            .expect("base fetched");

        assert_eq!(resolved.sha, remote_head.trim());
        assert_eq!(resolved.reference, "refs/homeboy/finalization/base/main");
    }

    #[test]
    fn resolve_verified_base_rejects_malformed_and_unresolvable_snapshots() {
        let repo = repo();
        for snapshot in ["not-a-sha", &"f".repeat(40)] {
            let error = RealAgentTaskPrFinalizationBackend
                .resolve_verified_base(repo.path().to_str().unwrap(), snapshot)
                .expect_err("invalid snapshot is rejected");
            assert_eq!(error.code, crate::ErrorCode::ValidationInvalidArgument);
        }
    }

    #[test]
    fn resolve_verified_base_rematerializes_only_the_persisted_snapshot() {
        let repo = repo();
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare"]);
        git(
            repo.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(repo.path(), &["push", "-u", "origin", "main"]);
        git(repo.path(), &["checkout", "-b", "snapshot"]);
        std::fs::write(repo.path().join("snapshot.txt"), "snapshot").unwrap();
        git(repo.path(), &["add", "snapshot.txt"]);
        git(repo.path(), &["commit", "-m", "verified snapshot"]);
        let snapshot = git_output(repo.path().to_str().unwrap(), &["rev-parse", "HEAD"]).unwrap();
        git(repo.path(), &["push", "origin", "snapshot"]);

        let clone = tempfile::tempdir().unwrap();
        assert!(Command::new("git")
            .args([
                "clone",
                "--no-local",
                "--single-branch",
                "--branch",
                "main",
                remote.path().to_str().unwrap(),
                clone.path().to_str().unwrap(),
            ])
            .status()
            .unwrap()
            .success());
        assert!(git_output(
            clone.path().to_str().unwrap(),
            &["cat-file", "-e", &format!("{snapshot}^{{commit}}")],
        )
        .is_err());

        let resolved = RealAgentTaskPrFinalizationBackend
            .resolve_verified_base(clone.path().to_str().unwrap(), &snapshot)
            .expect("exact snapshot rematerialized");
        assert_eq!(resolved.sha, snapshot);
        assert!(git_output(
            clone.path().to_str().unwrap(),
            &["cat-file", "-e", &format!("{}^{{commit}}", resolved.sha)],
        )
        .is_ok());

        let unavailable = "f".repeat(40);
        let error = RealAgentTaskPrFinalizationBackend
            .resolve_verified_base(clone.path().to_str().unwrap(), &unavailable)
            .expect_err("unavailable exact snapshot is rejected");
        assert_eq!(error.code, crate::ErrorCode::ValidationInvalidArgument);
    }

    #[test]
    fn captured_base_survives_remote_advance_while_newer_snapshot_rejects_stale_candidate() {
        let repo = repo();
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare"]);
        git(
            repo.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(repo.path(), &["push", "-u", "origin", "main"]);
        let captured_base = git_output(repo.path().to_str().unwrap(), &["rev-parse", "HEAD"])
            .expect("captured declared base");

        git(repo.path(), &["checkout", "-b", "feature"]);
        std::fs::write(repo.path().join("feature.txt"), "feature").unwrap();
        git(repo.path(), &["add", "feature.txt"]);
        git(
            repo.path(),
            &["commit", "-m", "candidate verified by gates"],
        );

        git(repo.path(), &["checkout", "main"]);
        std::fs::write(repo.path().join("main.txt"), "advance").unwrap();
        git(repo.path(), &["add", "main.txt"]);
        git(repo.path(), &["commit", "-m", "remote advance after gates"]);
        git(repo.path(), &["push", "origin", "main"]);
        let advanced_base = git_output(repo.path().to_str().unwrap(), &["rev-parse", "HEAD"])
            .expect("advanced base");
        git(repo.path(), &["checkout", "feature"]);

        let published = RealAgentTaskPrFinalizationBackend
            .candidate_state(
                repo.path().to_str().unwrap(),
                &base(&captured_base, &captured_base),
                "feature",
            )
            .expect("captured base validates candidate");
        assert!(matches!(
            published,
            AgentTaskPrCandidateState::Committed { .. }
        ));

        let stale = RealAgentTaskPrFinalizationBackend
            .candidate_state(
                repo.path().to_str().unwrap(),
                &base(&advanced_base, &advanced_base),
                "feature",
            )
            .expect("newer snapshot compares candidate");
        assert!(matches!(stale, AgentTaskPrCandidateState::Invalid { .. }));
    }
}

pub(super) fn validate_real_candidate_fingerprint(
    options: &AgentTaskPrFinalizationOptions,
) -> Result<()> {
    let record = crate::agent_task_lifecycle::status(&options.run_id)?;
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
    let AgentTaskPromotionCandidate::Git {
        fingerprint: expected_fingerprint,
    } = expected
    else {
        unreachable!("caller validates Git promotion candidate")
    };
    let actual = crate::agent_task_promotion::candidate_fingerprint(&options.path)?;
    let AgentTaskPromotionCandidate::Git {
        fingerprint: actual_fingerprint,
    } = &actual
    else {
        return Err(Error::validation_invalid_argument(
            "path",
            "finalization path is not a Git worktree; normal GitHub PR finalization requires the promoted Git candidate",
            Some(options.path.clone()),
            None,
        ));
    };
    if actual == *expected {
        let changed_files = normalize_changed_files(&options.changed_files);
        if !changed_files.is_empty() && changed_files != actual_fingerprint.changed_files {
            return Err(Error::validation_invalid_argument(
                "changed-file",
                "caller changed files do not match promoted candidate",
                None,
                None,
            ));
        }
        return Ok(());
    }
    if !actual_fingerprint.changed_files.is_empty()
        || expected_fingerprint.tree.is_empty()
        || !committed_candidate_matches(options, expected_fingerprint)?
    {
        return Err(Error::validation_invalid_argument(
            "path",
            "candidate changed after promotion; durable finalization accepts a recovery commit only when its parent and tree exactly match the recorded promoted candidate. Rerun promotion gates before finalization.",
            None,
            None,
        ));
    }
    let changed_files = normalize_changed_files(&options.changed_files);
    if changed_files != expected_fingerprint.changed_files {
        return Err(Error::validation_invalid_argument(
            "changed-file",
            "recovery commit changed files must exactly match the recorded promoted candidate",
            None,
            None,
        ));
    }
    Ok(())
}

fn committed_candidate_matches(
    options: &AgentTaskPrFinalizationOptions,
    fingerprint: &crate::agent_task_promotion::AgentTaskCandidateFingerprint,
) -> Result<bool> {
    let parent = git_output(&options.path, &["rev-parse", "HEAD^"])?;
    if parent.trim() != fingerprint.head {
        return Ok(false);
    }
    let tree = git_output(&options.path, &["rev-parse", "HEAD^{tree}"])?;
    Ok(tree.trim() == fingerprint.tree)
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
