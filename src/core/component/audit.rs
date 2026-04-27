use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuditConfig {
    /// Class/base names whose public methods are invoked by a runtime dispatcher.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_entrypoint_extends: Vec<String>,
    /// Source markers that indicate public methods are runtime-dispatched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_entrypoint_markers: Vec<String>,
    /// Paths whose guards run outside normal production runtime assumptions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lifecycle_path_globs: Vec<String>,
    /// Type suffixes that mark convention outliers as intentional utilities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub utility_suffixes: Vec<String>,
    /// Files exempt from convention outlier checks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub convention_exception_globs: Vec<String>,
}

impl AuditConfig {
    pub fn is_empty(&self) -> bool {
        self.runtime_entrypoint_extends.is_empty()
            && self.runtime_entrypoint_markers.is_empty()
            && self.lifecycle_path_globs.is_empty()
            && self.utility_suffixes.is_empty()
            && self.convention_exception_globs.is_empty()
    }

    pub fn merge(&mut self, other: &AuditConfig) {
        extend_unique(
            &mut self.runtime_entrypoint_extends,
            &other.runtime_entrypoint_extends,
        );
        extend_unique(
            &mut self.runtime_entrypoint_markers,
            &other.runtime_entrypoint_markers,
        );
        extend_unique(&mut self.lifecycle_path_globs, &other.lifecycle_path_globs);
        extend_unique(&mut self.utility_suffixes, &other.utility_suffixes);
        extend_unique(
            &mut self.convention_exception_globs,
            &other.convention_exception_globs,
        );
    }
}

fn extend_unique(target: &mut Vec<String>, source: &[String]) {
    for value in source {
        if !target.contains(value) {
            target.push(value.clone());
        }
    }
}
