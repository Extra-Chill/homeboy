//! Stable facade for artifact contracts, links, manifests, and publication helpers.
//!
//! New command/core code should import artifact APIs from this module instead of
//! reaching into individual artifact implementation modules.

pub use super::artifact_dom_boxes::{
    capture as capture_dom_boxes, DomBoxCaptureSpec, DomBoxReport,
};
pub use super::artifact_links::{
    cached_validated_viewer_links, public_artifact_path_url, public_artifact_url,
    PUBLIC_ARTIFACT_BASE_URL_ENV,
};
pub use super::artifact_manifest::{
    ArtifactManifest, ARTIFACT_MANIFEST_SCHEMA, RUNTIME_AGENT_ARTIFACT_PATHS_SCHEMA,
    RUNTIME_AGENT_FINAL_OUTPUT_ARTIFACT_PATH, RUNTIME_AGENT_PATCH_DIFF_ARTIFACT_FILE,
    RUNTIME_AGENT_PATCH_PATCH_ARTIFACT_FILE, RUNTIME_AGENT_RESULT_ARTIFACT_FILE,
    RUNTIME_AGENT_TRANSCRIPT_ARTIFACT_FILE, RUNTIME_AGENT_TRANSCRIPT_ARTIFACT_PATH,
    RUN_ARTIFACT_EVENTS_FILE, RUN_ARTIFACT_FANOUT_RUN_FILE, RUN_ARTIFACT_LOOP_POLICY_FILE,
    RUN_ARTIFACT_LOOP_RESULT_FILE, RUN_ARTIFACT_OUTCOME_FILE, RUN_ARTIFACT_RESULTS_FILE,
    RUN_ARTIFACT_STATUS_FILE,
};
pub use super::artifact_origin::{
    inspect, serve, serve_listener, status, status_with_command, ArtifactOriginInspect,
    ArtifactOriginServeSpec, ArtifactOriginStatus,
};
pub use super::artifact_postprocess::{
    record_artifact_postprocess_outputs, run_artifact_postprocess_plan_for_persisted_root,
    run_artifact_postprocess_steps, validate_artifact_postprocess_plan, ArtifactPostprocessAction,
    ArtifactPostprocessContext, ArtifactPostprocessOutput, ArtifactPostprocessPlan,
    ArtifactPostprocessPlanDescription, ArtifactPostprocessProducedArtifact,
    ArtifactPostprocessResult, ArtifactPostprocessReviewerRef, ArtifactPostprocessRoot,
    ARTIFACT_POSTPROCESS_PLAN_SCHEMA, ARTIFACT_POSTPROCESS_RESULT_SCHEMA,
    ARTIFACT_POSTPROCESS_SCHEMA,
};
pub use super::artifact_preview::{html_preview_entrypoints, ArtifactPreviewEntrypoint};
pub use super::matrix_artifact_summary::{
    generic_matrix_summary_from_artifacts, is_matrix_summary_artifact,
    render_matrix_artifact_summary_markdown, summarize_matrix_artifacts, GenericMatrixSummary,
    MatrixArtifactSummary,
};
pub use super::publication_artifacts::index_remote_published_artifact_refs_for_run;

/// Resolve the artifact root used for copied/downloaded run artifacts.
pub fn root() -> super::Result<std::path::PathBuf> {
    super::artifact_root()
}
