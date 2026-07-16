fn release_workflow() -> &'static str {
    include_str!("../.github/workflows/release.yml")
}

fn release_quality_policy_script() -> &'static str {
    include_str!("../.github/release-quality-policy.sh")
}

fn cargo_manifest() -> &'static str {
    include_str!("../Cargo.toml")
}

fn dist_workspace_manifest() -> &'static str {
    include_str!("../dist-workspace.toml")
}

fn release_quality_policy(
    blocking_commands: &str,
    audit_result: &str,
    lint_result: &str,
    test_result: &str,
) -> std::process::Output {
    std::process::Command::new("bash")
        .arg(".github/release-quality-policy.sh")
        .env("BLOCKING_COMMANDS", blocking_commands)
        .env("AUDIT_RESULT", audit_result)
        .env("LINT_RESULT", lint_result)
        .env("TEST_RESULT", test_result)
        .output()
        .expect("release quality policy should run")
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
        .skip(1)
        .find_map(|(offset, line)| {
            let is_next_job = line.starts_with("  ")
                && !line.starts_with("    ")
                && line.trim_end().ends_with(':');

            is_next_job.then_some(offset.saturating_sub(1))
        })
        .unwrap_or(rest.len());

    &rest[..end]
}

#[test]
fn release_workflow_has_no_generic_source_autofix() {
    let workflow = release_workflow();

    // Generic release auto-refactor / autofix was removed (#8046). The release
    // pipeline must not mutate source, open autofix branches, or create
    // autofix PRs. The gate-refactor job, the autofix action inputs, and the
    // `refactor --from all` broad source-sweep command must all be absent.
    assert!(
        !workflow.contains("gate-refactor"),
        "release workflow must not contain a gate-refactor job"
    );
    assert!(
        !workflow.contains("autofix: 'true'"),
        "release workflow must not enable generic source autofix"
    );
    assert!(
        !workflow.contains("autofix-mode: always"),
        "release workflow must not run autofix in always mode"
    );
    assert!(
        !workflow.contains("autofix-open-pr: 'true'"),
        "release workflow must not create autofix PRs"
    );
    assert!(
        !workflow.contains("refactor --from all"),
        "release workflow must not run a broad refactor --from all sweep"
    );
}

#[test]
fn release_workflow_declared_drift_maintenance_is_narrow_not_generic() {
    let workflow = release_workflow();

    // The only code path that can push generated drift to the base branch is
    // the narrow allowlisted transaction in core
    // (changes_are_only_drift / drift_file_paths), which gates on
    // extension-declared lockfile_paths + the audit baseline (homeboy.json).
    // The release workflow must not widen this into generic source mutation:
    // every quality gate must run read-only (autofix disabled) so that no
    // authored source fix is produced for the transaction to route.
    for job in ["gate-audit", "gate-lint", "gate-test"] {
        let section = job_section(workflow, job);
        assert!(
            section.contains("autofix: 'false'"),
            "{job} must run read-only (autofix disabled) so only declared generated drift can be maintained"
        );
    }

    // The release workflow has no other writable action invocation. Generated
    // drift remains owned by the core allowlist, rather than a workflow-level
    // repair action that could stage authored source files.
    assert_eq!(
        workflow.matches("autofix:").count(),
        3,
        "only the three read-only quality gates may declare autofix behavior"
    );
}

#[test]
fn release_quality_policy_defaults_to_lint_and_test_blocking() {
    assert!(release_workflow().contains(
        "RELEASE_BLOCKING_COMMANDS: ${{ inputs.release_blocking_commands || 'review lint,review test' }}"
    ));

    let policy = job_section(release_workflow(), "release-quality-policy");

    assert!(policy.contains("BLOCKING_COMMANDS: ${{ env.RELEASE_BLOCKING_COMMANDS }}"));
    assert!(policy.contains("bash .github/release-quality-policy.sh"));
    assert!(policy.contains(
        "AUDIT_RESULT: ${{ needs.gate-audit.outputs.audit-result || needs.gate-audit.result }}"
    ));
    assert!(release_quality_policy_script().contains("check_command audit"));
    assert!(release_quality_policy_script().contains("check_command lint"));
    assert!(release_quality_policy_script().contains("check_command test"));
    assert!(release_quality_policy_script()
        .contains("Command ${command} is tracked but not release-blocking"));
}

#[test]
fn release_quality_policy_checks_out_event_commit_before_running_script() {
    let policy = job_section(release_workflow(), "release-quality-policy");

    let checkout_index = policy.find("actions/checkout@v4").expect(
        "release-quality-policy must check out the repository before running the policy script",
    );
    let script_index = policy
        .find("bash .github/release-quality-policy.sh")
        .expect("release-quality-policy must invoke the policy script");

    assert!(
        checkout_index < script_index,
        "release-quality-policy checkout must precede the policy script invocation"
    );

    assert!(
        policy.contains("ref: ${{ github.sha }}"),
        "release-quality-policy must check out the exact workflow/event commit (github.sha)"
    );
}

#[test]
fn release_quality_policy_blocks_review_test_failures_and_allows_passing_gates() {
    let failed = release_quality_policy("review lint,review test", "failure", "success", "failure");

    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stdout)
        .contains("Release-blocking command test finished with result: failure"));

    let passed = release_quality_policy("review lint,review test", "failure", "success", "success");

    assert!(passed.status.success());
}

#[test]
fn release_audit_is_advisory_without_losing_raw_failure_outcome() {
    let gate_audit = job_section(release_workflow(), "gate-audit");

    assert!(gate_audit.contains("audit-result: ${{ steps.audit.outcome }}"));
    assert!(gate_audit.contains("id: audit"));
    assert!(gate_audit.contains("continue-on-error: true"));
    assert!(gate_audit.contains("--profile=pr"));
}

#[test]
fn release_audit_preserves_changed_since_baseline_consumption() {
    let gate_audit = job_section(release_workflow(), "gate-audit");

    // Audit baseline consumption is preserved (#8046): the changed-since
    // comparison reads homeboy.json baselines.audit.known_fingerprints from
    // the merge-base so only newly-introduced audit findings are reported.
    assert!(
        gate_audit.contains("--changed-since"),
        "gate-audit must consume the audit baseline via --changed-since"
    );
    assert!(
        gate_audit.contains("app-token"),
        "gate-audit must retain app-token for automated categorized issue filing"
    );
}

#[test]
fn release_ci_disables_homeboy_update_checks() {
    assert!(release_workflow().contains("HOMEBOY_NO_UPDATE_CHECK: '1'"));
}

#[test]
fn release_test_gate_does_not_repeat_separate_lint_gate() {
    let gate_test = job_section(release_workflow(), "gate-test");

    assert!(gate_test.contains("commands: review test"));
    assert!(gate_test.contains("--skip-lint"));
    assert!(gate_test.contains("--changed-since {0}"));
}

#[test]
fn release_planning_skips_quality_gates_already_owned_by_gate_jobs() {
    let check = job_section(release_workflow(), "check");
    let prepare = job_section(release_workflow(), "prepare");

    for section in [check, prepare] {
        assert!(section.contains("commands: release"));
        assert!(section.contains("args: --skip-checks=audit,lint,test"));
    }
}

#[test]
fn release_preflight_validates_the_private_workspace_build_before_mutating_release_state() {
    let prepare = job_section(release_workflow(), "prepare");
    let package_preflight = prepare
        .find("name: Preflight release workspace build")
        .expect("prepare must validate the complete release build");
    let release_action = prepare
        .find("uses: Extra-Chill/homeboy-action@v2")
        .expect("prepare must run the release action");

    assert!(
        prepare.contains("run: cargo build --workspace --all-targets --locked"),
        "release preflight must build every private workspace target with the locked dependency graph"
    );
    assert!(
        cargo_manifest().contains("publish = false"),
        "the root package must not be planned for crates.io publication"
    );
    assert!(
        cargo_manifest().contains("homeboy-lab-contract = { path = \"crates/homeboy-lab-contract\" }"),
        "the root package must consume the extracted Lab contract crate as a private path dependency"
    );
    assert!(
        package_preflight < release_action,
        "package preflight must run before release preparation can create a tag"
    );
}

#[test]
fn release_workflow_publishes_binary_channels_not_crates_io() {
    let workflow = release_workflow();
    let host = job_section(workflow, "host");

    assert!(!workflow.contains("crates.io"));
    assert!(!host.contains("CARGO_REGISTRY_TOKEN"));
    assert!(host.contains("release-skip-publish: 'true'"));
    assert!(dist_workspace_manifest().contains("ci = \"github\""));
    assert!(dist_workspace_manifest().contains("publish-jobs = [\"homebrew\"]"));
}

#[test]
fn release_prepare_waits_for_command_policy_not_raw_gates() {
    let prepare = job_section(release_workflow(), "prepare");

    // gate-refactor was removed (#8046); prepare must never reference it.
    assert!(
        !prepare.contains("- gate-refactor"),
        "prepare must not wait on the removed gate-refactor job"
    );

    for gate in ["gate-audit", "gate-lint", "gate-test"] {
        assert!(
            !prepare.contains(&format!("- {gate}")),
            "prepare should wait on release-quality-policy, not raw {gate}"
        );
    }

    assert!(prepare.contains("- release-quality-policy"));
    assert!(prepare.contains("needs.release-quality-policy.result == 'success'"));
    assert!(prepare.contains("inputs.release_tag != ''"));
}

#[test]
fn release_prepare_uses_prepared_output_to_unlock_publish_jobs() {
    let prepare = job_section(release_workflow(), "prepare");
    let plan = job_section(release_workflow(), "plan");

    assert!(prepare.contains("prepared: ${{ steps.outputs.outputs.prepared }}"));
    assert!(prepare.contains("id: prepared"));
    assert!(prepare.contains("id: outputs"));
    assert!(prepare.contains("steps.release.outputs['release-tag'] != ''"));
    assert!(prepare.contains("echo \"prepared=true\" >> \"$GITHUB_OUTPUT\""));
    assert!(prepare.contains(
        "PREPARED=\"${{ steps.recovery.outputs.prepared || steps.prepared.outputs.prepared }}\""
    ));
    assert!(prepare.contains("release-tag: ${{ steps.outputs.outputs['release-tag'] }}"));
    assert!(prepare.contains("downstream jobs will publish it in this run"));
    assert!(
        prepare.contains("Skipping release preparation; downstream jobs will publish existing tag")
    );

    assert!(plan.contains("needs.prepare.outputs.prepared == 'true'"));
    assert!(plan.contains("needs.prepare.outputs['release-tag'] != ''"));
    assert!(plan.contains("needs.prepare.outputs['release-tag']"));
    assert!(!plan.contains("needs.prepare.outputs.released == 'true'"));
}

#[test]
fn release_recovery_bypasses_quality_gates_and_still_prepares() {
    let check = job_section(release_workflow(), "check");
    let gate_audit = job_section(release_workflow(), "gate-audit");
    let gate_lint = job_section(release_workflow(), "gate-lint");
    let gate_test = job_section(release_workflow(), "gate-test");
    let policy = job_section(release_workflow(), "release-quality-policy");
    let prepare = job_section(release_workflow(), "prepare");

    assert!(check.contains("recovery-release: ${{ steps.check.outputs.recovery-release }}"));
    assert!(check.contains("release-version: ${{ steps.check.outputs['release-version'] }}"));
    assert!(check.contains("release-tag: ${{ steps.check.outputs['release-tag'] }}"));
    assert!(check.contains("RELEASE_TAG=\"${{ steps.recovery.outputs['release-tag'] || steps.release-check.outputs['release-tag'] }}\""));
    assert!(check.contains("echo \"recovery-release=true\" >> \"$GITHUB_OUTPUT\""));
    assert!(check.contains("echo \"release-tag=${RELEASE_TAG}\" >> \"$GITHUB_OUTPUT\""));
    assert!(
        check.contains("Recovered prepared release tag ${RELEASE_TAG}; bypassing quality gates")
    );

    for section in [gate_audit, gate_lint, gate_test, policy] {
        assert!(section.contains("needs.check.outputs.recovery-release != 'true'"));
    }

    assert!(prepare.contains("needs.check.outputs.recovery-release == 'true'"));
    assert!(prepare.contains(
        "if: inputs.release_tag == '' && needs.check.outputs.recovery-release != 'true'"
    ));
    assert!(prepare.contains(
        "if: inputs.release_tag != '' || needs.check.outputs.recovery-release == 'true'"
    ));
    assert!(
        prepare.contains("TAG=\"${{ inputs.release_tag || needs.check.outputs['release-tag'] }}\"")
    );
}

#[test]
fn release_test_gate_exposes_release_blocking_policy_to_rust_tests() {
    let gate_test = job_section(release_workflow(), "gate-test");

    assert!(gate_test.contains("RELEASE_BLOCKING_COMMANDS: ${{ env.RELEASE_BLOCKING_COMMANDS }}"));
}

#[test]
fn release_finish_head_pipeline_uses_homeboy_action_head_inputs() {
    let host = job_section(release_workflow(), "host");

    assert!(host.contains("uses: Extra-Chill/homeboy-action@v2"));
    assert!(host.contains("release-head: 'true'"));
    assert!(host.contains("release-from-artifacts: artifacts"));
}

#[test]
fn release_prepare_and_publish_preflight_runner_disk() {
    let workflow = release_workflow();

    assert!(workflow.contains("RELEASE_MIN_FREE_KB: '5242880'"));
    assert!(workflow.contains("Preflight release runner disk"));
    assert!(workflow.contains("Preflight release publisher disk"));
    assert!(workflow.contains("df -h ."));
    assert!(workflow.contains("$RUNNER_TEMP"));
    assert!(workflow.contains("rm -rf target/distrib target/package .homeboy-bin artifacts"));
    assert!(workflow
        .contains("refusing prepare before the runner exhausts disk while writing diagnostics"));
    assert!(workflow
        .contains("refusing publish before the runner exhausts disk while writing diagnostics"));
}
