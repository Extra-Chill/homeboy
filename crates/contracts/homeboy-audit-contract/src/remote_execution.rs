use serde::{Deserialize, Serialize};

use super::extend_unique;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteExecutionSafetyConfig {
    /// Report convention label for remote execution preflight findings.
    #[serde(
        default = "default_remote_execution_preflight_convention",
        skip_serializing_if = "is_default_remote_execution_preflight_convention"
    )]
    pub convention: String,
    /// Markers that identify remote execution dispatch sites.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dispatch_markers: Vec<String>,
    /// Markers that prove local arguments/paths are translated or rejected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path_translation_markers: Vec<String>,
    /// Markers that identify caller-provided arguments entering remote commands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argument_forward_markers: Vec<String>,
    /// Markers that prove required remote capabilities were declared/checked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capability_preflight_markers: Vec<String>,
    /// Markers that identify component-specific artifact capture requests.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_capture_markers: Vec<String>,
    /// Markers that prove captured artifacts carry a source snapshot/mirror contract.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_snapshot_markers: Vec<String>,
    /// Markers that prove selected extensions/tools are available remotely.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_parity_markers: Vec<String>,
    /// Markers that identify remote dispatch sites accepting extension selectors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_selector_markers: Vec<String>,
    /// Markers that identify remotely reported artifact references.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_report_markers: Vec<String>,
    /// Markers that prove reported artifacts are locally accessible or retrievable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_access_markers: Vec<String>,
}

fn default_remote_execution_preflight_convention() -> String {
    "remote_execution_preflight".to_string()
}

fn is_default_remote_execution_preflight_convention(value: &str) -> bool {
    value == default_remote_execution_preflight_convention()
}

impl Default for RemoteExecutionSafetyConfig {
    fn default() -> Self {
        Self {
            convention: default_remote_execution_preflight_convention(),
            dispatch_markers: Vec::new(),
            path_translation_markers: Vec::new(),
            argument_forward_markers: Vec::new(),
            capability_preflight_markers: Vec::new(),
            artifact_capture_markers: Vec::new(),
            artifact_snapshot_markers: Vec::new(),
            extension_parity_markers: Vec::new(),
            extension_selector_markers: Vec::new(),
            artifact_report_markers: Vec::new(),
            artifact_access_markers: Vec::new(),
        }
    }
}

impl RemoteExecutionSafetyConfig {
    pub fn is_empty(&self) -> bool {
        self.dispatch_markers.is_empty()
            && self.path_translation_markers.is_empty()
            && self.argument_forward_markers.is_empty()
            && self.capability_preflight_markers.is_empty()
            && self.artifact_capture_markers.is_empty()
            && self.artifact_snapshot_markers.is_empty()
            && self.extension_parity_markers.is_empty()
            && self.extension_selector_markers.is_empty()
            && self.artifact_report_markers.is_empty()
            && self.artifact_access_markers.is_empty()
    }

    pub(super) fn merge(&mut self, other: &RemoteExecutionSafetyConfig) {
        if other.convention != default_remote_execution_preflight_convention() {
            self.convention = other.convention.clone();
        }
        extend_unique(&mut self.dispatch_markers, &other.dispatch_markers);
        extend_unique(
            &mut self.path_translation_markers,
            &other.path_translation_markers,
        );
        extend_unique(
            &mut self.argument_forward_markers,
            &other.argument_forward_markers,
        );
        extend_unique(
            &mut self.capability_preflight_markers,
            &other.capability_preflight_markers,
        );
        extend_unique(
            &mut self.artifact_capture_markers,
            &other.artifact_capture_markers,
        );
        extend_unique(
            &mut self.artifact_snapshot_markers,
            &other.artifact_snapshot_markers,
        );
        extend_unique(
            &mut self.extension_parity_markers,
            &other.extension_parity_markers,
        );
        extend_unique(
            &mut self.extension_selector_markers,
            &other.extension_selector_markers,
        );
        extend_unique(
            &mut self.artifact_report_markers,
            &other.artifact_report_markers,
        );
        extend_unique(
            &mut self.artifact_access_markers,
            &other.artifact_access_markers,
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ArtifactPortabilityConfig {
    /// Number of recent observation runs to scan for persisted artifact path portability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_run_window: Option<usize>,
    /// Path prefixes that identify local/runtime-only locations in stored artifacts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub non_portable_path_prefixes: Vec<String>,
    /// Path substrings that identify project-specific local/runtime-only artifact locations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub non_portable_path_contains: Vec<String>,
}

impl ArtifactPortabilityConfig {
    pub fn is_empty(&self) -> bool {
        self.observation_run_window.is_none()
            && self.non_portable_path_prefixes.is_empty()
            && self.non_portable_path_contains.is_empty()
    }

    pub fn with_generic_defaults(&self) -> Self {
        let mut config = self.clone();
        extend_unique(
            &mut config.non_portable_path_prefixes,
            &[
                "/tmp/".to_string(),
                "/private/tmp/".to_string(),
                "/var/folders/".to_string(),
            ],
        );
        config
    }

    pub(super) fn merge(&mut self, other: &ArtifactPortabilityConfig) {
        if other.observation_run_window.is_some() {
            self.observation_run_window = other.observation_run_window;
        }
        extend_unique(
            &mut self.non_portable_path_prefixes,
            &other.non_portable_path_prefixes,
        );
        extend_unique(
            &mut self.non_portable_path_contains,
            &other.non_portable_path_contains,
        );
    }
}
