use std::process::Command;

fn ci_workflow() -> &'static str {
    include_str!("../.github/workflows/ci.yml")
}

fn pr_state(event_name: &str, event_action: &str) -> String {
    let output = std::env::temp_dir().join(format!(
        "homeboy-pr-ci-state-{}-{event_name}-{event_action}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&output);

    let status = Command::new("bash")
        .arg(".github/ci-pr-state.sh")
        .env("GITHUB_EVENT_NAME", event_name)
        .env("GITHUB_EVENT_ACTION", event_action)
        .env("GITHUB_OUTPUT", &output)
        .status()
        .expect("PR-state guard should run");
    assert!(status.success());

    let state = std::fs::read_to_string(&output).expect("PR-state guard output");
    let _ = std::fs::remove_file(output);
    state
}

fn job_section<'a>(workflow: &'a str, job: &str) -> &'a str {
    let marker = format!("  {job}:\n");
    let start = workflow
        .find(&marker)
        .unwrap_or_else(|| panic!("missing {job} job"));
    let rest = &workflow[start + marker.len()..];
    let end = rest
        .lines()
        .scan(0usize, |offset, line| {
            let current = *offset;
            *offset += line.len() + 1;
            Some((current, line))
        })
        .find_map(|(offset, line)| {
            (line.starts_with("  ") && !line.starts_with("    ") && line.ends_with(':'))
                .then_some(offset.saturating_sub(1))
        })
        .unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn closed_prs_stop_candidate_admission_at_every_fanout_boundary() {
    // These model closure arriving before candidate build, inventory planning,
    // or shard fanout. The closure run cancels the prior concurrency-group
    // member and every candidate boundary waits for its false output.
    for phase in ["candidate-build", "inventory", "shard-fanout"] {
        assert_eq!(
            pr_state("pull_request", "closed"),
            "active=false\n",
            "a closure before {phase} must not admit more candidate work"
        );
    }

    let workflow = ci_workflow();
    assert!(workflow.contains(
        "pull_request:\n    branches: [main]\n    # A closed PR starts one lightweight run in the existing concurrency group.\n    # That cancels an in-flight candidate DAG, while pr-state keeps this run from\n    # admitting the reusable workflow's binary, inventory, and shard jobs.\n    types: [opened, synchronize, reopened, closed]"
    ));
    assert!(workflow.contains("group: ci-${{ github.event.pull_request.number || github.ref }}"));
    assert!(workflow.contains("cancel-in-progress: true"));

    let state = job_section(workflow, "pr-state");
    assert!(state.contains("active: ${{ steps.state.outputs.active }}"));
    assert!(state.contains("ref: ${{ github.event.repository.default_branch }}"));
    assert!(state.contains("bash .github/ci-pr-state.sh"));

    for (phase, job) in [
        ("candidate build", "homeboy-fast"),
        ("inventory planning", "homeboy"),
        ("shard fanout", "homeboy"),
    ] {
        let candidate = job_section(workflow, job);
        assert!(
            candidate.contains("needs: pr-state"),
            "{phase} must await PR state"
        );
        assert!(
            candidate.contains("if: ${{ needs.pr-state.outputs.active == 'true' }}"),
            "{phase} must be skipped after closure"
        );
    }

    for job in [
        "required-gates-declaration",
        "workspace-tests-compile",
        "warning-clean",
        "windows-compile",
        "rustfmt",
        "homeboy-fast",
        "homeboy",
    ] {
        let section = job_section(workflow, job);
        assert!(
            section.contains("needs: pr-state"),
            "{job} must await PR state"
        );
        assert!(
            section.contains("if: ${{ needs.pr-state.outputs.active == 'true' }}"),
            "{job} must be skipped after closure"
        );
    }

    let test = job_section(workflow, "homeboy");
    assert!(test.contains("uses: Extra-Chill/homeboy-action/.github/workflows/ci.yml@v2"));
    assert!(test.contains("test-shards: '16'"));
}

#[test]
fn non_pr_ci_invocations_remain_admitted() {
    assert_eq!(pr_state("pull_request", "synchronize"), "active=true\n");
    assert_eq!(pr_state("workflow_dispatch", ""), "active=true\n");
    assert_eq!(pr_state("push", ""), "active=true\n");
}
