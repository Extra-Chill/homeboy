use serde::{Deserialize, Serialize};

use crate::core::error::{Error, Result};
use crate::core::git::{
    commit_at, get_uncommitted_changes, pr_create, pr_edit, pr_find, push_at, CommitOptions,
    PrCreateOptions, PrEditOptions, PrFindOptions, PrState, PushOptions,
};

pub const AGENT_TASK_PR_FINALIZATION_SCHEMA: &str = "homeboy/agent-task-pr-finalization/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateResult {
    pub name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskPrFinalizationReport {
    pub schema: String,
    pub run_id: String,
    pub status: String,
    pub path: String,
    pub base: String,
    pub head: String,
    pub pr_action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    pub changed_files: Vec<String>,
    pub gate_results: Vec<AgentTaskGateResult>,
    #[serde(flatten)]
    pub evidence: AgentTaskPrEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPrEvidence {
    pub source_refs: Vec<String>,
    pub artifact_refs: Vec<String>,
    pub attempt_summary: String,
    pub ai_tool: String,
}

#[derive(Debug, Clone)]
pub struct AgentTaskPrFinalizationOptions {
    pub path: String,
    pub run_id: String,
    pub base: String,
    pub head: Option<String>,
    pub title: String,
    pub commit_message: String,
    pub gate_results: Vec<AgentTaskGateResult>,
    pub changed_files: Vec<String>,
    pub evidence: AgentTaskPrEvidence,
    pub ai_used_for: String,
    pub protected_branches: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTaskPrRef {
    pub number: u64,
    pub url: String,
}

pub trait AgentTaskPrFinalizationBackend {
    fn current_branch(&mut self, path: &str) -> Result<String>;
    fn changed_files(&mut self, path: &str) -> Result<Vec<String>>;
    fn commit_all(&mut self, path: &str, message: &str) -> Result<()>;
    fn push_branch(&mut self, path: &str, head: &str) -> Result<()>;
    fn find_open_pr(
        &mut self,
        path: &str,
        base: &str,
        head: &str,
    ) -> Result<Option<AgentTaskPrRef>>;
    fn create_pr(
        &mut self,
        path: &str,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<AgentTaskPrRef>;
    fn update_pr(
        &mut self,
        path: &str,
        number: u64,
        title: &str,
        body: &str,
    ) -> Result<AgentTaskPrRef>;
}

pub struct RealAgentTaskPrFinalizationBackend;

impl AgentTaskPrFinalizationBackend for RealAgentTaskPrFinalizationBackend {
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

pub fn finalize_pr(
    options: AgentTaskPrFinalizationOptions,
) -> Result<AgentTaskPrFinalizationReport> {
    finalize_pr_with_backend(options, &mut RealAgentTaskPrFinalizationBackend)
}

pub fn finalize_pr_with_backend<B: AgentTaskPrFinalizationBackend>(
    options: AgentTaskPrFinalizationOptions,
    backend: &mut B,
) -> Result<AgentTaskPrFinalizationReport> {
    validate_green_gates(&options.gate_results)?;
    let head = options
        .head
        .clone()
        .map(Ok)
        .unwrap_or_else(|| backend.current_branch(&options.path))?;
    refuse_protected_head(&head, &options.protected_branches)?;

    let mut changed_files = if options.changed_files.is_empty() {
        backend.changed_files(&options.path)?
    } else {
        options.changed_files.clone()
    };
    changed_files.sort();
    changed_files.dedup();

    if changed_files.is_empty() {
        return Ok(report(
            &options,
            &head,
            "no_changes",
            "none",
            None,
            None,
            changed_files,
        ));
    }

    backend.commit_all(&options.path, &options.commit_message)?;
    backend.push_branch(&options.path, &head)?;
    let body = render_pr_body(&options, &head, &changed_files);
    let existing = backend.find_open_pr(&options.path, &options.base, &head)?;
    let (action, pr) = match existing {
        Some(existing) => (
            "updated",
            backend.update_pr(&options.path, existing.number, &options.title, &body)?,
        ),
        None => (
            "created",
            backend.create_pr(&options.path, &options.base, &head, &options.title, &body)?,
        ),
    };

    Ok(report(
        &options,
        &head,
        "review_ready",
        action,
        Some(pr.number),
        Some(pr.url),
        changed_files,
    ))
}

fn validate_green_gates(gates: &[AgentTaskGateResult]) -> Result<()> {
    if gates.is_empty() {
        return Err(Error::validation_invalid_argument(
            "gate_results",
            "at least one deterministic green gate is required before PR finalization",
            None,
            None,
        ));
    }
    let red: Vec<String> = gates
        .iter()
        .filter(|gate| !is_green_status(&gate.status))
        .map(|gate| format!("{}={}", gate.name, gate.status))
        .collect();
    if !red.is_empty() {
        return Err(Error::validation_invalid_argument(
            "gate_results",
            format!(
                "finalization requires green gates; red gates: {}",
                red.join(", ")
            ),
            None,
            None,
        ));
    }
    Ok(())
}

fn is_green_status(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "green" | "passed" | "pass" | "succeeded" | "success" | "ok"
    )
}

fn refuse_protected_head(head: &str, protected_branches: &[String]) -> Result<()> {
    if protected_branches.iter().any(|branch| branch == head) {
        return Err(Error::validation_invalid_argument(
            "head",
            format!(
                "refusing to finalize directly on protected branch '{}'",
                head
            ),
            None,
            Some(protected_branches.to_vec()),
        ));
    }
    Ok(())
}

fn render_pr_body(
    options: &AgentTaskPrFinalizationOptions,
    head: &str,
    changed_files: &[String],
) -> String {
    format!(
        "## Summary\n- Finalized Homeboy agent-task cook run `{}` into review-ready branch `{}`.\n\n## Source refs\n{}\n\n## Attempt summary\n{}\n\n## Gate results\n{}\n\n## Changed files\n{}\n\n## Artifact refs\n{}\n\n## Final status\n- **Status:** review-ready\n- **Base:** `{}`\n- **Head:** `{}`\n- **Merge/deploy:** not performed\n\n## AI assistance\n- **AI assistance:** Yes\n- **Tool(s):** {}\n- **Used for:** {}\n",
        options.run_id,
        head,
        bullets(&options.evidence.source_refs),
        options.evidence.attempt_summary,
        gate_bullets(&options.gate_results),
        bullets(changed_files),
        bullets(&options.evidence.artifact_refs),
        options.base,
        head,
        options.evidence.ai_tool,
        options.ai_used_for
    )
}

fn bullets(values: &[String]) -> String {
    if values.is_empty() {
        return "- none recorded".to_string();
    }
    values
        .iter()
        .map(|value| format!("- {}", value))
        .collect::<Vec<_>>()
        .join("\n")
}

fn gate_bullets(gates: &[AgentTaskGateResult]) -> String {
    gates
        .iter()
        .map(|gate| match &gate.detail {
            Some(detail) if !detail.trim().is_empty() => {
                format!("- {}: {} ({})", gate.name, gate.status, detail)
            }
            _ => format!("- {}: {}", gate.name, gate.status),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn report(
    options: &AgentTaskPrFinalizationOptions,
    head: &str,
    status: &str,
    pr_action: &str,
    pr_number: Option<u64>,
    pr_url: Option<String>,
    changed_files: Vec<String>,
) -> AgentTaskPrFinalizationReport {
    AgentTaskPrFinalizationReport {
        schema: AGENT_TASK_PR_FINALIZATION_SCHEMA.to_string(),
        run_id: options.run_id.clone(),
        status: status.to_string(),
        path: options.path.clone(),
        base: options.base.clone(),
        head: head.to_string(),
        pr_action: pr_action.to_string(),
        pr_number,
        pr_url,
        changed_files,
        gate_results: options.gate_results.clone(),
        evidence: options.evidence.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MockBackend {
        branch: String,
        changed_files: Vec<String>,
        existing_pr: Option<AgentTaskPrRef>,
        create_error: bool,
        committed: bool,
        pushed: bool,
        created: bool,
        updated: bool,
        last_body: String,
    }

    impl AgentTaskPrFinalizationBackend for MockBackend {
        fn current_branch(&mut self, _path: &str) -> Result<String> {
            Ok(if self.branch.is_empty() {
                "fix/cook".to_string()
            } else {
                self.branch.clone()
            })
        }

        fn changed_files(&mut self, _path: &str) -> Result<Vec<String>> {
            Ok(self.changed_files.clone())
        }

        fn commit_all(&mut self, _path: &str, _message: &str) -> Result<()> {
            self.committed = true;
            Ok(())
        }

        fn push_branch(&mut self, _path: &str, _head: &str) -> Result<()> {
            self.pushed = true;
            Ok(())
        }

        fn find_open_pr(
            &mut self,
            _path: &str,
            _base: &str,
            _head: &str,
        ) -> Result<Option<AgentTaskPrRef>> {
            Ok(self.existing_pr.clone())
        }

        fn create_pr(
            &mut self,
            _path: &str,
            _base: &str,
            _head: &str,
            _title: &str,
            body: &str,
        ) -> Result<AgentTaskPrRef> {
            if self.create_error {
                return Err(Error::git_command_failed("gh pr create failed"));
            }
            self.created = true;
            self.last_body = body.to_string();
            Ok(AgentTaskPrRef {
                number: 123,
                url: "https://github.com/Extra-Chill/homeboy/pull/123".to_string(),
            })
        }

        fn update_pr(
            &mut self,
            _path: &str,
            number: u64,
            _title: &str,
            body: &str,
        ) -> Result<AgentTaskPrRef> {
            self.updated = true;
            self.last_body = body.to_string();
            Ok(AgentTaskPrRef {
                number,
                url: format!("https://github.com/Extra-Chill/homeboy/pull/{}", number),
            })
        }
    }

    #[test]
    fn creates_new_pr_after_green_gates() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        };

        let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

        assert_eq!(report.status, "review_ready");
        assert_eq!(report.pr_action, "created");
        assert_eq!(report.pr_number, Some(123));
        assert!(backend.committed);
        assert!(backend.pushed);
        assert!(backend.created);
        assert!(backend.last_body.contains("## AI assistance"));
        assert!(backend.last_body.contains("## Gate results"));
        assert!(backend.last_body.contains("review-ready"));
    }

    #[test]
    fn updates_existing_pr_for_same_branch() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            existing_pr: Some(AgentTaskPrRef {
                number: 77,
                url: "https://github.com/Extra-Chill/homeboy/pull/77".to_string(),
            }),
            ..Default::default()
        };

        let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

        assert_eq!(report.status, "review_ready");
        assert_eq!(report.pr_action, "updated");
        assert_eq!(report.pr_number, Some(77));
        assert!(backend.updated);
        assert!(!backend.created);
    }

    #[test]
    fn reports_no_changes_without_commit_push_or_pr() {
        let mut backend = MockBackend::default();

        let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

        assert_eq!(report.status, "no_changes");
        assert_eq!(report.pr_action, "none");
        assert!(!backend.committed);
        assert!(!backend.pushed);
        assert!(!backend.created);
    }

    #[test]
    fn refuses_protected_branch() {
        let mut backend = MockBackend {
            branch: "main".to_string(),
            changed_files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        };

        let error = finalize_pr_with_backend(options(), &mut backend).expect_err("blocked");

        assert!(error.message.contains("protected branch"));
        assert!(!backend.committed);
    }

    #[test]
    fn propagates_pr_creation_failure() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            create_error: true,
            ..Default::default()
        };

        let error = finalize_pr_with_backend(options(), &mut backend).expect_err("failed");

        assert!(error.message.contains("gh pr create failed"));
        assert!(backend.committed);
        assert!(backend.pushed);
    }

    #[test]
    fn refuses_red_gates() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        };
        let mut options = options();
        options.gate_results[0].status = "failed".to_string();

        let error = finalize_pr_with_backend(options, &mut backend).expect_err("blocked");

        assert!(error.message.contains("green gates"));
        assert!(!backend.committed);
    }

    fn options() -> AgentTaskPrFinalizationOptions {
        AgentTaskPrFinalizationOptions {
            path: "/repo".to_string(),
            run_id: "cook-3678".to_string(),
            base: "main".to_string(),
            head: None,
            title: "Cook issue #3678".to_string(),
            commit_message: "finalize cook loop PR plumbing".to_string(),
            gate_results: vec![AgentTaskGateResult {
                name: "cargo test".to_string(),
                status: "passed".to_string(),
                detail: Some("targeted".to_string()),
            }],
            changed_files: Vec::new(),
            evidence: AgentTaskPrEvidence {
                source_refs: vec!["https://github.com/Extra-Chill/homeboy/issues/3678".to_string()],
                artifact_refs: vec!["artifact://aggregate.json".to_string()],
                attempt_summary: "attempt 1 passed deterministic gates".to_string(),
                ai_tool: "OpenCode (GPT-5.5)".to_string(),
            },
            ai_used_for: "Drafted implementation and tests; Chris reviews and owns the change."
                .to_string(),
            protected_branches: vec![
                "main".to_string(),
                "master".to_string(),
                "trunk".to_string(),
            ],
        }
    }
}
