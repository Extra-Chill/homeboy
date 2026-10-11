mod declaration;
mod runtime_state;

// The 14 types the crate-root facade publishes. Previously two globs
// (`declaration::*` + `runtime_state::*`) republished all 37 items here.
pub use declaration::{
    ExposeServiceTunnelSpec, ServiceTunnel, ServiceTunnelAuth, ServiceTunnelAuthMode,
    ServiceTunnelExposure, ServiceTunnelPolicy, ServiceTunnelPreviewPolicy,
    ServiceTunnelPreviewPolicyMode, ServiceTunnelTarget, StartServiceTunnelSpec,
};
pub use runtime_state::{
    ServiceTunnelReadinessCheck, ServiceTunnelReadinessKind, ServiceTunnelStatus,
    ServiceTunnelTunnelBackend,
};
// Reachable through `ServiceTunnelStatus`'s and `ServiceTunnelPolicy`'s public fields.
pub use declaration::{ServiceTunnelNativePreviewAuthPolicy, ServiceTunnelNativePreviewToken};
pub use runtime_state::{
    ServiceTunnelBackendStatus, ServiceTunnelCommandSpec, ServiceTunnelEvidence,
    ServiceTunnelHealthStatus, ServiceTunnelLogPaths, ServiceTunnelPreviewArtifact,
    ServiceTunnelPreviewCleanupMetadata, ServiceTunnelPreviewIdentity, ServiceTunnelPreviewSource,
    ServiceTunnelProcessDescriptor, ServiceTunnelProcessStatus, ServiceTunnelReadinessCheckStatus,
    ServiceTunnelReadinessStatus,
};

pub(crate) use declaration::*;
pub(crate) use declaration::{default_local_host, default_scheme};
pub(crate) use runtime_state::*;
