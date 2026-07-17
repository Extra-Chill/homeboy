//! Extension capability enum + manifest-support descriptor.
//!
//! Pure data + manifest-driven metadata (all dependencies resolve to
//! `ExtensionManifest` in this crate). Execution behavior lives in
//! `homeboy-core`.

use crate::ExtensionManifest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionCapability {
    Lint,
    Test,
    Build,
    Bench,
    Fuzz,
    Trace,
    Deps,
}

/// Static metadata for an [`ExtensionCapability`] variant.
///
/// Centralizing label, manifest-support probe, and script accessor in one
/// descriptor keeps variant additions localized: a new capability only
/// needs one new arm in [`ExtensionCapability::descriptor`] instead of
/// parallel arms scattered across each getter / policy method.
struct ExtensionCapabilityDescriptor {
    label: &'static str,
    has_manifest_support: fn(&ExtensionManifest) -> bool,
    script_path: fn(&ExtensionManifest) -> Option<&str>,
}

impl ExtensionCapability {
    fn descriptor(self) -> ExtensionCapabilityDescriptor {
        match self {
            ExtensionCapability::Lint => ExtensionCapabilityDescriptor {
                label: "lint",
                has_manifest_support: ExtensionManifest::has_lint,
                script_path: ExtensionManifest::lint_script,
            },
            ExtensionCapability::Test => ExtensionCapabilityDescriptor {
                label: "test",
                has_manifest_support: ExtensionManifest::has_test,
                script_path: ExtensionManifest::test_script,
            },
            ExtensionCapability::Build => ExtensionCapabilityDescriptor {
                label: "build",
                has_manifest_support: ExtensionManifest::has_build,
                script_path: ExtensionManifest::build_script,
            },
            ExtensionCapability::Bench => ExtensionCapabilityDescriptor {
                label: "bench",
                has_manifest_support: ExtensionManifest::has_bench,
                script_path: ExtensionManifest::bench_script,
            },
            ExtensionCapability::Fuzz => ExtensionCapabilityDescriptor {
                label: "fuzz",
                has_manifest_support: ExtensionManifest::has_fuzz,
                script_path: ExtensionManifest::fuzz_script,
            },
            ExtensionCapability::Trace => ExtensionCapabilityDescriptor {
                label: "trace",
                has_manifest_support: ExtensionManifest::has_trace,
                script_path: ExtensionManifest::trace_script,
            },
            ExtensionCapability::Deps => ExtensionCapabilityDescriptor {
                label: "deps",
                has_manifest_support: ExtensionManifest::has_deps,
                script_path: ExtensionManifest::deps_script,
            },
        }
    }

    pub fn label(self) -> &'static str {
        self.descriptor().label
    }

    pub fn has_manifest_support(self, manifest: &ExtensionManifest) -> bool {
        (self.descriptor().has_manifest_support)(manifest)
    }

    pub fn script_path(self, manifest: &ExtensionManifest) -> Option<&str> {
        (self.descriptor().script_path)(manifest)
    }

    pub fn requires_script(self) -> bool {
        // Fuzz supports manifest-only workload discovery; `fuzz run` validates
        // its runner script before execution.
        !matches!(self, ExtensionCapability::Build | ExtensionCapability::Fuzz)
    }
}
