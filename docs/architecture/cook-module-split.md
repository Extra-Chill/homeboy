# Cook orchestration structural split

Implements [#18611](https://github.com/Extra-Chill/homeboy/issues/18611),
part of [#18436](https://github.com/Extra-Chill/homeboy/issues/18436).
No logic, public JSON, CLI contracts, or public item names change. No helpers
were deleted. Public and crate-visible entry points remain available through
`cook` and the existing service re-exports. The spine is moved whole.

## Region map

Original `cook.rs`: 10,476 lines. After: 1,901 lines (including retained tests).
Line ranges below refer to the original file; destination counts include imports
and documentation after formatting.

| Original region | Destination | Before | After |
| --- | --- | ---: | ---: |
| L196–736: retry admission, interruptions, progress detail | `cook_admission.rs` | 541 | 554 |
| L1041–1414: gate setup retry, promotion claims, diagnostics | `cook_gate_setup.rs` | 374 | 378 |
| L1833–2486: lifecycle/completion/observation projections | `cook_projections.rs` | 654 | 664 |
| L2487–2903: projection tests | `cook_projection_tests.rs` | 417 | 404 |
| L3081–3946: follow-up dispatch, no-change rotation | `cook_follow_up.rs` | 866 | 874 |
| L3947–4295: timeout/startup reporting | `cook_timeout.rs` | 349 | 352 |
| L4480–5231: runtime driver, durable error reporting | `cook_driver.rs` | 752 | 756 |
| L5270–7902: entire orchestration spine | `cook_spine.rs` | 2,633 | 2,637 |
| L8085–10107: workspace validation, materialization, ancestry | `cook_workspace.rs` | 2,023 | 2,043 |

Projection tests retain their original fully qualified module and test names via
`#[path]` on `cook::run_lifecycle_projection_tests`. Existing `cook_tests.rs` and
its descendants are untouched.

## Visibility-only changes

Private free items shared by the sibling modules now use `pub(super)`;
imports shared through `cook` use the same service-local visibility. Existing
`pub`, `pub(crate)` and `pub(super)` items retain their scope. Additional member
access required by the move: `CookSideEffects::promote`,
`PreArtifactInterruptionPhase::name`, and the test hook fields
`WorkspaceBaseCaptureHook::{workspace,count,fail_after_recipe_persistence}`
are now `pub(super)`. No field or method became public API.

Inventory of formerly private free items exposed only to the enclosing service:

- `cook.rs`: `PROMOTION_CLAIM_LEASE`, `WORKSPACE_BASE_CAPTURE_HOOK`, `WorkspaceBaseCaptureHook`, `cook_recover_command_with_prefix`, `cook_recovery_placement_decision`, `cook_recovery_command_prefix_for_decision`, `report_cook_progress_with_activity`, `admit_cook_runtime_generation`, `run_cook_startup_phase`, `finalization_operation_key`, `finalize_with_operation_claim_in_store`, `TestCookPromoteFn`, `project_initial_finalizing_review_form_contract`, `cook_pr_title_policy`, `project_controller_owned_gate_contract`, `project_attempt_prompt_template`, `COOK_CONTINUE_ROUTE_SCHEMA`, `reserve_cook_materialization_capacity`, `record_cook_disk_pressure_advisory`, `publish_latest_incomplete_draft`.
- `cook_admission.rs`: `promotion_operation_key`, `RETRY_DISPATCH_CLAIM_LEASE`, `RETRY_DISPATCH_ACCEPTANCE_WAIT`, `await_dispatch_acceptance_receipt`, `dispatch_acceptance_poll_interval`, `PRE_ARTIFACT_INTERRUPTION_CLAIM_LEASE`, `MAX_TRANSPORT_RETRIES_PER_ATTEMPT`, `transport_retry_available`, `transient_infrastructure_failure`, `PreProviderRetry`, `pre_provider_retry`, `next_transport_retry_run_id`, `wait_out_transient_failure`, `transport_admission_reset_available`, `retry_dispatch_operation_key`, `pre_artifact_interruption_operation_key`, `PreArtifactInterruptionPhase`, `pre_artifact_interruption_phase`, `runner_reported_success`, `request_requires_substantive_candidate`, `no_change_requires_substantive_candidate`, `pre_artifact_execution_count`, `pre_artifact_interruption_report`, `claim_pre_artifact_interruption_retry`, `claim_pre_artifact_interruption_retry_with_stores`, `FINALIZATION_CLAIM_LEASE`, `COOK_HEARTBEAT_INTERVAL`, `COOK_PROVIDER_TERMINAL_POLL_INTERVAL`, `provider_timeout_heartbeat_detail`.
- `cook_driver.rs`: `noop_cook_progress_observer`, `run_cook_with_runtime`, `finish_cook_terminal_with_report`, `cook_component`, `run_cook_reported`, `durable_cook_error_report`, `durable_cook_error_report_with_store`, `rooted_status`.
- `cook_follow_up.rs`: `remediation_tool_policy_error`, `cook_remediation_same_provider`, `NO_CHANGE_PROVIDER_MESSAGE_LIMIT`, `no_change_stop_reason`, `no_change_remediation`, `budget_authority_matches_scope`, `scoped_follow_up_budget`, `review_budget_authority`, `child_execution_budget`, `dispatch_bound_cook_follow_up`, `adopted_attempt_is_ready_for_cook_continuation`.
- `cook_gate_setup.rs`: `is_gate_setup_failure`, `recover_compatibility_cook_base`, `gate_setup_retry_delay`, `promote_with_operation_claim_in_store`, `deepest_typed_error`, `typed_error_at`, `causal_typed_error`, `plain_cause_error`, `bound_diagnostic_value`, `truncate_diagnostic_text`, `redact_diagnostic_text`.
- `cook_projections.rs`: `cook_success`, `cook_completion_with_stores`, `canonical_candidate_produced`, `canonical_finalization_recovery_run_id_with_stores`, `canonical_finalization_eligible`, `slice_is_empty`, `terminal_fallback_status`, `terminal_stop_reason`, `incomplete_no_candidate_reason`.
- `cook_spine.rs`: `run_cook_spine`.
- `cook_timeout.rs`: `outcome_is_timeout`, `provider_timeout_ms`, `startup_without_output_liveness_diagnostic`, `make_startup_without_output_actionable`, `make_provider_timeout_actionable`, `make_provider_rotation_actionable`.
- `cook_workspace.rs`: `resumable_cook_run_id_in_store`, `validate_cook_candidate_group`, `bind_dispatch_workspace_attestations`, `cook_attempt_needs_execution_with_store`, `cook_run_record_needs_execution`, `validate_cook_workspace`, `validate_cook_workspace_with_adopted_candidate`, `supersede_previous_cook_candidate`, `CookWorkspaceBaseValidation`, `record_cook_workspace_base_convergence`, `pin_initial_cook_workspace_base`, `pin_and_persist_initial_cook_workspace_base`, `reconcile_persisted_task_base_plan`, `apply_persisted_task_base`, `pin_cook_workspace_base_at`, `CookWorkspaceBasePin`, `local_tracking_cook_base`, `native_cook_destination_record`, `preflight_cook_workspace_base_ancestry_with_provider`, `preflight_cook_workspace_base_ancestry`, `materialize_prepared_cook_base`, `deferred_cook_base_preparation`, `tolerate_retryable_cook_base_resolution`, `preflight_cook_workspace_resolved_base_ancestry`, `preflight_initial_cook_workspace_provider`, `admit_explicit_cook_workspace_before_provider`, `cook_has_provider_execution_in_store`, `supersede_by_live_destination_owner`, `cook_workspace_lookup_pending`, `materialize_pending_cook_workspace`, `dependency_tool_preflight_error`, `resolve_cook_component_workspace`, `cook_remote_identity`, `provision_pending_cook_workspace`, `validate_pending_cook_repository_identity`, `TrackedPromotionContinuation`, `cook_owned_unpushed_destination`, `tracked_promotion_continuation_in_store`, `cook_owns_attempt_in_store`, `authenticate_tracked_promotion_continuation`, `record_active_cook_worktree_warning`, `rebind_baseline_continuation_workspace`, `canonicalize_cook_provider_workspace`, `canonicalize_native_cook_workspace`.

## AI assistance

AI assistance implemented the structural extraction, adjusted service-local
visibility and re-exports, and checked compilation and focused tests. Directed
by Chris Huber; the issue was drafted by Claude via Roadie/Pi.
