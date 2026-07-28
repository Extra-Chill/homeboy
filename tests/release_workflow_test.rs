fn release_workflow() -> &'static str {
    include_str!("../.github/workflows/release.yml")
}

fn ci_workflow() -> &'static str {
    include_str!("../.github/workflows/ci.yml")
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
fn release_concurrency_scopes_recovery_runs_to_the_requested_tag() {
    assert!(release_workflow().contains(
        "concurrency:\n  group: release-${{ inputs.release_tag || github.ref }}\n  cancel-in-progress: false"
    ));
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
        prepare.contains("run: cargo build --workspace --locked"),
        "release preflight must build the private workspace with the locked dependency graph"
    );
    assert!(
        cargo_manifest().contains("publish = false"),
        "the root package must not be planned for crates.io publication"
    );
    assert!(
        cargo_manifest().contains("homeboy-cli = { path = \"crates/homeboy-cli\" }"),
        "the root package must consume the extracted CLI crate as a private path dependency"
    );
    assert!(
        cargo_manifest().contains("homeboy-core = { path = \"crates/homeboy-core\" }"),
        "the root package must consume the extracted core crate as a private path dependency"
    );
    assert!(
        cargo_manifest().contains("homeboy-lab-contract = { path = \"crates/contracts/homeboy-lab-contract\" }"),
        "the root package must consume the extracted Lab contract crate as a private path dependency"
    );
    assert!(
        package_preflight < release_action,
        "package preflight must run before release preparation can create a tag"
    );
}

/// Walk the workspace sources once and count what the Windows guard exists for.
///
/// Reading the tree rather than trusting a remembered number is the point: a
/// job name in a YAML file proves a job runs, not that it guards anything.
fn windows_source_surface() -> (usize, Vec<String>) {
    fn walk(
        dir: &std::path::Path,
        rust: &mut Vec<std::path::PathBuf>,
        manifests: &mut Vec<std::path::PathBuf>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                // `target/` is build output and `.git/` is not source.
                if name == "target" || name == ".git" || name == "node_modules" {
                    continue;
                }
                walk(&path, rust, manifests);
            } else if name.ends_with(".rs") {
                rust.push(path);
            } else if name == "Cargo.toml" {
                manifests.push(path);
            }
        }
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut rust = Vec::new();
    let mut manifests = Vec::new();
    for sub in ["crates", "src"] {
        walk(&root.join(sub), &mut rust, &mut manifests);
    }
    manifests.push(root.join("Cargo.toml"));

    let cfg_sites = rust
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .map(|body| {
            body.matches("cfg(windows)").count()
                + body.matches("cfg(target_os = \"windows\")").count()
        })
        .sum();

    let mut windows_crates: Vec<String> = manifests
        .iter()
        .filter(|path| {
            std::fs::read_to_string(path)
                .map(|body| body.contains("windows-sys"))
                .unwrap_or(false)
        })
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    windows_crates.sort();

    (cfg_sites, windows_crates)
}

/// PR CI keeps a Windows *source* check; the release drops the Windows
/// *artifact*. Both halves are asserted by effect, not by job name.
///
/// #10539: `38492c5db` removed the `gate-windows-build` job and left this test
/// asserting it, panicking `missing gate-windows-build job` and intermittently
/// killing releases. Inverting the assertion fixed that instance — but a test
/// that only says "the removed job is still removed" would keep passing if the
/// surviving guard were narrowed to nothing, or if the code it guards
/// disappeared and the job became pure CI spend. So this asserts the guard has
/// something to guard, and that its scope actually reaches it.
#[test]
fn release_omits_unused_windows_artifacts_while_ci_checks_windows_source() {
    let release = release_workflow();
    let windows_compile = job_section(ci_workflow(), "windows-compile");

    assert!(
        !release.contains("gate-windows-build"),
        "release must not restore the unused native Windows artifact gate"
    );
    assert!(
        !dist_workspace_manifest().contains("x86_64-pc-windows-msvc"),
        "release must not publish an unconsumed Windows artifact"
    );

    // The guard must still be a Windows guard.
    assert!(
        windows_compile.contains("runs-on: windows-latest"),
        "a Windows source check that does not run on Windows checks nothing"
    );
    // `--workspace` is the load-bearing word. Narrowing to `-p <crate>` would
    // leave the job green while the cfg-gated code it exists for goes
    // uncompiled, which is indistinguishable from deleting the job.
    assert!(
        windows_compile.contains("run: cargo check --workspace --locked"),
        "the Windows check must cover the whole workspace with the locked graph"
    );
    assert!(
        !windows_compile.contains("--tests"),
        "the Windows check is deliberately codegen-free and test-target-free; \
         some Unix-only test helpers rely on that boundary"
    );

    // And there must be something for it to guard. If this ever hits zero the
    // honest move is to delete the job, not to keep paying a windows-latest
    // runner on every PR for a check with an empty subject.
    let (cfg_sites, windows_crates) = windows_source_surface();
    assert!(
        cfg_sites > 0,
        "no cfg(windows) sites remain in the workspace: the windows-compile job now guards \
         nothing and should be removed rather than left running"
    );
    assert!(
        !windows_crates.is_empty(),
        "no crate declares a windows-sys dependency: the windows-compile job now guards nothing \
         and should be removed rather than left running. Checked manifests under crates/ and the root."
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

    // Regression guard: recovery releases reach `prepare` through its
    // `always()` gate while the quality-policy job is skipped. If `plan`
    // relied on the implicit `success()`, that skipped ancestor would
    // propagate down the `needs` chain and skip `plan` too — landing the
    // tag with no GitHub Release. `plan` must gate explicitly on
    // `always()` + `prepare.result == 'success'` so the recovery path
    // still publishes.
    assert!(
        plan.contains("always()"),
        "plan must use always() so a skipped quality-policy ancestor does not skip publish"
    );
    assert!(
        plan.contains("needs.prepare.result == 'success'"),
        "plan must gate on prepare's result explicitly instead of the implicit success()"
    );
}

#[test]
fn release_fails_loudly_when_prepared_release_does_not_publish() {
    let plan = job_section(release_workflow(), "plan");
    let verify = job_section(release_workflow(), "verify-published");

    // The guard must run whenever a release was actually prepared, even if
    // the publish chain skipped (so it uses always() + the prepared output).
    assert!(
        verify.contains("always()"),
        "verify-published must use always() so a skipped host does not skip the guard"
    );
    assert!(verify.contains("needs.prepare.outputs.prepared == 'true'"));
    assert!(verify.contains("needs.prepare.outputs['release-tag'] != ''"));
    assert!(verify.contains("- plan"));
    assert!(verify.contains("contents: read"));
    assert!(verify.contains("GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}"));
    assert!(plan.contains("expected-assets: ${{ steps.plan.outputs.expected-assets }}"));
    assert!(plan.contains(".releases[].artifacts[]? | split(\"/\") | last"));
    assert!(plan.contains("cargo-dist planned no release assets"));
    assert!(verify.contains("EXPECTED_ASSETS: ${{ needs.plan.outputs.expected-assets }}"));
    assert!(verify.contains("gh api \"repos/${GITHUB_REPOSITORY}/releases/tags/${RELEASE_TAG}\""));
    assert!(verify.contains(".state == \"uploaded\" and .size > 0"));
    assert!(verify.contains("missing planned assets"));

    // It must fail the run when host did not succeed.
    assert!(verify.contains("needs.host.result"));
    assert!(
        verify.contains("!= \"success\""),
        "verify-published must fail unless host succeeded"
    );
    assert!(
        verify.contains("exit 1"),
        "verify-published must exit non-zero to fail the run"
    );

    // A failed guard must be treated as a release failure by the bookkeeping
    // jobs so the broken commit is recorded and not silently retried.
    let record_failure = job_section(release_workflow(), "record-failure");
    let clear_failure = job_section(release_workflow(), "clear-failure");
    assert!(record_failure.contains("- verify-published"));
    assert!(clear_failure.contains("- verify-published"));
}

#[test]
fn release_artifact_builds_survive_recovery_skips_but_fail_closed_on_required_builds() {
    let workflow = release_workflow();
    let local = job_section(workflow, "build-local-artifacts");
    let global = job_section(workflow, "build-global-artifacts");
    let host = job_section(workflow, "host");

    for section in [local, global, host] {
        assert!(section.contains("always()"));
        assert!(section.contains("needs.prepare.result == 'success'"));
        assert!(section.contains("needs.plan.result == 'success'"));
        assert!(section.contains("needs.prepare.outputs.prepared == 'true'"));
    }

    assert!(local.contains("artifacts_matrix.include != null"));
    assert!(global.contains(
        "artifacts_matrix.include == null || needs.build-local-artifacts.result == 'success'"
    ));
    assert!(host.contains("needs.build-global-artifacts.result == 'success'"));
    assert!(host.contains(
        "artifacts_matrix.include == null || needs.build-local-artifacts.result == 'success'"
    ));
    assert!(!host.contains("needs.build-global-artifacts.result == 'skipped'"));
    assert!(!host.contains("needs.build-local-artifacts.result == 'skipped'"));
}

#[test]
fn release_host_checkout_fetches_full_history_for_finalizer_ancestry_validation() {
    let host = job_section(release_workflow(), "host");
    let checkout = host
        .find("uses: actions/checkout@v4")
        .expect("host must check out the release tag");
    let finalizer = host
        .find("Finish Homeboy release pipeline at tag")
        .expect("host must finalize the release");

    assert!(checkout < finalizer);
    assert!(host[checkout..finalizer].contains("fetch-depth: 0"));
}

#[test]
fn release_shared_binary_uses_the_publication_host_runtime_baseline() {
    let gate_build = job_section(release_workflow(), "gate-build");
    let host = job_section(release_workflow(), "host");

    assert!(gate_build.contains("runs-on: ubuntu-22.04"));
    assert!(host.contains("runs-on: ubuntu-22.04"));
    assert!(!gate_build.contains("runs-on: ubuntu-latest"));
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
    assert!(host.contains("Create remote draft adoption manifest"));
    assert!(host.contains("homeboy.draft-adoption"));
    assert!(host.contains("expected_assets: $expected_assets"));
    assert!(host.contains("Download current Homeboy finalizer"));
    assert!(host.contains("binary-path: ${{ needs.prepare.outputs.recovery-release == 'true' && '.homeboy-bin/homeboy' || '' }}"));
    assert!(host.contains("release-from-artifacts: ${{ needs.prepare.outputs.recovery-release == 'true' && 'draft-adoption' || 'artifacts' }}"));
}

#[test]
fn release_recovery_propagates_dist_opt_in_and_dirty_allowance_through_all_dist_phases() {
    let workflow = release_workflow();
    let prepare = job_section(workflow, "prepare");
    let local_build = job_section(workflow, "build-local-artifacts");

    assert!(prepare.contains("recovery-release: ${{ steps.outputs.outputs.recovery-release }}"));
    assert!(prepare.contains("RECOVERY_RELEASE=\"${{ needs.check.outputs.recovery-release }}\""));
    assert!(prepare.contains("echo \"recovery-release=${RECOVERY_RELEASE}\" >> \"$GITHUB_OUTPUT\""));

    let build_step = local_build
        .find("- name: Build artifacts")
        .expect("local artifact build step must exist");
    let post_build = local_build
        .find("- id: cargo-dist")
        .expect("local artifact post-build step must exist");
    assert!(
        local_build[build_step..post_build].contains("shell: bash"),
        "recovery syntax must use Bash on every matrix runner, including Windows"
    );

    for job in [
        "plan",
        "build-local-artifacts",
        "build-global-artifacts",
        "host",
    ] {
        let section = job_section(workflow, job);
        let opt_in = section
            .find("grep -Fqx '[package.metadata.dist]' Cargo.toml")
            .unwrap_or_else(|| panic!("{job} must use fixed-string exact-line matching"));
        let dist = section
            .find("\n          dist ")
            .unwrap_or_else(|| panic!("{job} must execute cargo-dist"));

        assert!(
            opt_in < dist,
            "{job} must append the dist opt-in before executing cargo-dist"
        );
        assert!(
            section.contains("printf '\\n[package.metadata.dist]\\ndist = true\\n' >> Cargo.toml"),
            "{job} must append the literal root package dist opt-in when it is absent"
        );
        assert!(
            section.contains(
                "if [ \"${{ needs.prepare.outputs.recovery-release }}\" = \"true\" ]; then\n            DIST_ALLOW_DIRTY=\"--allow-dirty\""
            ),
            "{job} must allow a dirty manifest only in recovery mode"
        );
        assert!(
            section.contains("else\n            DIST_ALLOW_DIRTY=\"\""),
            "{job} must preserve a clean fresh-release cargo-dist invocation"
        );
        assert_eq!(
            section.matches("--allow-dirty").count(),
            1,
            "{job} must not pass --allow-dirty outside the recovery branch"
        );
    }

    assert!(cargo_manifest().contains("[package.metadata.dist]\ndist = true"));
}

#[test]
fn release_recovery_restores_only_the_cargo_dist_overlay_before_finalization() {
    let host = job_section(release_workflow(), "host");
    let cargo_dist_upload = host
        .find("dist host --tag=${{ needs.prepare.outputs['release-tag'] }} --steps=upload")
        .expect("host must upload artifacts with cargo-dist");
    let restore = host
        .find("- name: Restore recovery cargo-dist overlay")
        .expect("host must clean up the recovery cargo-dist overlay");
    let finalize = host
        .find("- name: Finish Homeboy release pipeline at tag")
        .expect("host must finalize the Homeboy release pipeline");

    assert!(
        host.contains("if: needs.prepare.outputs.recovery-release == 'true'"),
        "only recovery releases may restore the cargo-dist overlay"
    );
    assert!(
        host.contains("run: git restore --source=HEAD -- Cargo.toml"),
        "cleanup must restore only the intentional Cargo.toml overlay"
    );
    assert!(
        cargo_dist_upload < restore && restore < finalize,
        "recovery overlay cleanup must follow cargo-dist upload and precede Homeboy finalization"
    );
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

/// Isolate a single `steps:` entry so a gate can be asserted on the step it
/// actually guards rather than merely somewhere in the job.
fn release_step_block<'a>(job: &'a str, marker_line: &str) -> &'a str {
    let marker = format!("      - {marker_line}\n");
    let start = job
        .find(&marker)
        .unwrap_or_else(|| panic!("missing step '{marker_line}'"));
    let rest = &job[start + marker.len()..];
    let end = rest
        .find("\n      - ")
        .map_or(rest.len(), |index| index + 1);
    &rest[..end]
}

/// Acceptance criteria 3 and 4 of #10519: recovery must not rebuild every
/// platform before it can adopt a draft that already holds every authoritative
/// asset. Both stranded drafts on this repo are in exactly that state
/// (`v0.321.1` with 13 assets, `v0.320.0` with 15 including Windows), yet each
/// recovery attempt spent ~40 minutes rebuilding them before failing in the
/// publisher.
#[test]
fn release_recovery_skips_the_artifact_rebuild_when_the_draft_is_already_complete() {
    let workflow = release_workflow();
    let plan = job_section(workflow, "plan");

    assert!(
        plan.contains("draft-complete: ${{ steps.draft-probe.outputs.draft-complete }}"),
        "plan must publish the fast-path decision to downstream jobs"
    );

    let probe = release_step_block(
        plan,
        "name: Probe existing draft for a complete asset inventory",
    );
    assert!(
        probe.contains("if: needs.prepare.outputs.recovery-release == 'true'"),
        "the fast path is a recovery-only concern; fresh releases have no draft to adopt"
    );
    assert!(
        probe.contains("draft-complete=${COMPLETE}"),
        "the probe must emit its decision"
    );
    assert!(
        probe.contains("COMPLETE=false"),
        "the probe must default to rebuilding"
    );

    // The fail-safe ordering property. Whether `dist host --steps=create`
    // disturbs an existing draft's assets is undocumented; observing the
    // inventory *after* create means a create that emptied the draft is seen as
    // incomplete and the normal rebuild runs. A pre-create probe could skip the
    // very rebuild that restores those assets.
    let create = plan
        .find("dist host --steps=create")
        .expect("plan must create the GitHub Release with cargo-dist");
    let probe_at = plan
        .find("- name: Probe existing draft for a complete asset inventory")
        .expect("plan must probe the draft");
    assert!(
        create < probe_at,
        "the completeness probe must observe the post-create inventory so an asset-clearing create falls back to the rebuild"
    );

    // GitHub's default shell is `bash -e`; `set -uo pipefail` alone leaves
    // errexit on, so a transient `gh`/`jq` failure in an optimisation probe
    // would fail the whole release.
    assert!(
        probe.contains("set +e"),
        "the probe must not be able to fail a release it only exists to speed up"
    );

    for job in ["build-local-artifacts", "build-global-artifacts"] {
        assert!(
            job_section(workflow, job).contains("needs.plan.outputs.draft-complete != 'true'"),
            "{job} must be skipped when the draft already holds every expected asset"
        );
    }

    let host = job_section(workflow, "host");
    assert!(
        host.contains("needs.plan.outputs.draft-complete == 'true' || (needs.build-global-artifacts.result == 'success'"),
        "host must still run when the artifact matrix was deliberately skipped, or the draft could never be published"
    );

    // Every cargo-dist upload-phase step is gated...
    for step in [
        "name: Install cached dist",
        "name: Fetch artifacts",
        "id: host",
        "name: Upload dist-manifest.json",
        "name: Download GitHub Artifacts",
        "name: Cleanup",
    ] {
        assert!(
            release_step_block(host, step)
                .contains("needs.plan.outputs.draft-complete != 'true'"),
            "host step '{step}' rebuilds or re-uploads artifacts and must be skipped on the fast path"
        );
    }

    // ...and the adoption phase is not, or the fast path would publish nothing.
    let adoption = &host[host
        .find("- name: Create remote draft adoption manifest")
        .expect("host must build the draft adoption manifest")..];
    assert!(
        !adoption.contains("draft-complete != 'true'"),
        "verified draft adoption must run on the fast path — it is the whole point of taking it"
    );
    assert!(
        adoption.contains("release-from-artifacts: ${{ needs.prepare.outputs.recovery-release == 'true' && 'draft-adoption' || 'artifacts' }}"),
        "the fast path must still hand the finalizer the remote-adoption manifest"
    );
}

/// The fast path is an optimisation, never a relaxation. Skipping the rebuild
/// must not skip a single verification: `validate_draft_adoption` still checks
/// name, size, upload state and SHA-256 digest against the published checksum
/// sidecars, and `verify-published` still re-reads the release afterwards.
#[test]
fn release_fast_path_keeps_every_publication_verification() {
    let workflow = release_workflow();
    let verify = job_section(workflow, "verify-published");

    assert!(
        !verify.contains("draft-complete"),
        "post-publication verification must be unconditional, including on the fast path"
    );
    assert!(
        verify.contains("EXPECTED_ASSETS: ${{ needs.plan.outputs.expected-assets }}"),
        "verification must re-check the same expected inventory the fast path matched on"
    );
    assert!(
        verify.contains("HOST_RESULT: ${{ needs.host.result }}"),
        "verification must still require the publishing job to have succeeded"
    );

    // The control binary that performs adoption is still the one built from the
    // dispatching commit, not from the stranded tag (#10519's headline defect,
    // fixed in #10560 — keep it fixed).
    let host = job_section(workflow, "host");
    assert!(
        host.contains("binary-path: ${{ needs.prepare.outputs.recovery-release == 'true' && '.homeboy-bin/homeboy' || '' }}"),
        "recovery must execute the freshly built control binary, never the stranded tag's"
    );
    assert!(
        job_section(workflow, "gate-build").contains("ref: ${{ github.sha }}"),
        "the control binary must be built from the dispatching commit"
    );
    assert!(
        host.contains("ref: ${{ needs.prepare.outputs['release-tag'] }}"),
        "the release target tree must stay pinned to the tag being recovered"
    );
    assert!(
        host.contains("| artifact bytes |"),
        "recovery evidence must record whether the published bytes were rebuilt or pre-existing"
    );
}

/// Recovery is allowed — required — to run a control binary NEWER than the tag
/// it repairs; that is the bootstrap #10519 asks for, and #10560 built it by
/// pinning `gate-build` to `github.sha`. The inverse was never bounded: a
/// control binary from a tree that never contained the tag cannot be shown to
/// carry the publisher fix recovery exists to apply, and would impose a release
/// contract the tag was never planned under.
///
/// The workflow is the only place that relationship can be established, because
/// only it holds both the control commit and the release target's git history.
/// The publisher enforces it, so the manifest must carry it.
#[test]
fn release_recovery_records_control_binary_lineage_against_the_release_target() {
    let workflow = release_workflow();
    let host = job_section(workflow, "host");
    let adoption = release_step_block(host, "name: Create remote draft adoption manifest");

    assert!(
        adoption.contains("CONTROL_SHA: ${{ github.sha }}"),
        "the adoption manifest must record which commit the control binary was built from"
    );
    assert!(
        adoption.contains("git merge-base --is-ancestor"),
        "control lineage must be read out of git history, never assumed"
    );
    assert!(
        adoption.contains("contains_target: $contains_target"),
        "the publisher can only enforce the lineage boundary if the manifest carries it"
    );

    // Fail-safe direction. An already-stranded release must never become
    // unrecoverable because ancestry could not be resolved, so only a
    // definitive "not an ancestor" (git exit code 1) may produce `false`;
    // every other path leaves the answer null, which the publisher treats as
    // unverified-but-permitted.
    assert!(
        adoption.contains("CONTAINS_TARGET=null"),
        "unresolvable ancestry must default to unverified, never to a blocking answer"
    );
    assert!(
        adoption.contains("1) CONTAINS_TARGET=false ;;"),
        "only git's definitive non-ancestor exit code may block a recovery"
    );
}
