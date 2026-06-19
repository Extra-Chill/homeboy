//! Stable facade for artifact contracts, links, manifests, and publication helpers.
//!
//! New command/core code should import artifact APIs from this module instead of
//! reaching into individual artifact implementation modules.

pub use super::artifact_address::{
    validated_public_url, ArtifactAddress, ArtifactAddressKind, ArtifactAddressValidation,
    ARTIFACT_ADDRESS_SCHEMA,
};
pub use super::artifact_contract::{
    ArtifactContract, EvidenceContract, ARTIFACT_CONTRACT_SCHEMA, EVIDENCE_CONTRACT_SCHEMA,
};
pub use super::artifact_inputs::ResolvedArtifactInput;
pub use super::artifact_links::{
    annotate_public_artifact_url_validation, cached_validated_viewer_links,
    public_artifact_path_url, public_artifact_url, public_artifact_url_validation_json,
    validate_public_artifact_url, validated_viewer_links, viewer_links,
    PublicArtifactUrlValidation, PUBLIC_ARTIFACT_BASE_URL_ENV,
};
pub use super::artifact_manifest::{
    manifest_for_existing_files, read_manifest_from_root, write_manifest_to_root, ArtifactManifest,
    ArtifactManifestEntry, ArtifactManifestProvenance, ArtifactManifestPublicUrlState,
    ArtifactManifestViewer, ArtifactManifestViewerLink, ArtifactRedactionState,
    ValidatedArtifactManifestEntry, ARTIFACT_MANIFEST_FILE, ARTIFACT_MANIFEST_SCHEMA,
};
pub use super::artifact_origin::{
    inspect, serve, status, status_with_command, ArtifactOriginInspect, ArtifactOriginServeSpec,
    ArtifactOriginStatus,
};
pub use super::artifact_ref::{
    ArtifactReference, METADATA_ONLY_REF_SCHEME, RUNNER_ARTIFACT_REF_SCHEME,
};
pub use super::browser_evidence::{
    validate_bench_results_payload, validate_trace_results_payload, BrowserArtifactMetadata,
    BrowserBottleneckRow, BrowserNetworkRequestRow, BrowserOriginDeclaredService,
    BrowserOriginEvidence, BrowserPerformanceProfileEnvelope, BrowserPhaseMark, BrowserPhaseWindow,
    BrowserRedirectEvidence, BrowserTimingRow, BrowserWindowLocationEvidence, TraceAssertion,
    TraceAssertionStatus, TraceAssertions, TraceEnvelope, TraceEnvelopeStatus, TraceEvent,
    TraceTimeline, BROWSER_EVIDENCE_SCHEMA_VERSION,
};
pub use super::change_artifact::{
    ChangeApplyResult, ChangeApplyStatus, ChangeArtifact, ChangeArtifactDigest,
    ChangeArtifactProvenance, ChangeArtifactSummary, ChangeDelta, ChangeDeltaFile, ChangePatch,
    CHANGE_APPLY_RESULT_SCHEMA, CHANGE_ARTIFACT_SCHEMA,
};
pub use super::publication_artifacts::index_remote_published_artifact_refs_for_run;
pub use super::structured_sidecar::{
    default_path, default_producer, default_schema_version, registry, schema, validate_payload,
    StructuredSidecarSchema, StructuredSidecarShape, REGISTRY,
};

/// Resolve the artifact root used for copied/downloaded run artifacts.
pub fn root() -> super::Result<std::path::PathBuf> {
    super::artifact_root()
}
