use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TransientWorkspacePolicy {
    markers: Vec<String>,
}

impl Default for TransientWorkspacePolicy {
    fn default() -> Self {
        Self {
            markers: vec!["tmp".to_string(), "Temp".to_string(), "T".to_string()],
        }
    }
}

impl TransientWorkspacePolicy {
    pub(crate) fn current() -> Self {
        let mut policy = Self::default();
        if let Ok(raw) = std::env::var("HOMEBOY_TRANSIENT_WORKSPACE_MARKERS") {
            for marker in raw
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                if !policy.markers.iter().any(|existing| existing == marker) {
                    policy.markers.push(marker.to_string());
                }
            }
        }
        policy
    }

    pub(crate) fn is_transient_path(&self, path: &Path) -> bool {
        crate::core::paths::path_component_strings(path)
            .iter()
            .any(|segment| self.markers.iter().any(|marker| marker == segment))
    }

    pub(crate) fn stable_root_before_marker(&self, path: &Path) -> Option<PathBuf> {
        let mut root = PathBuf::new();
        for segment in crate::core::paths::path_component_strings(path) {
            if self.markers.iter().any(|marker| marker == &segment) {
                return if root.as_os_str().is_empty() {
                    None
                } else {
                    Some(root)
                };
            }
            root.push(segment);
        }
        None
    }
}
