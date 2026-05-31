use serde::Serialize;

pub const SOURCE_SNAPSHOT_METADATA_ENV: &str = "HOMEBOY_SOURCE_SNAPSHOT_JSON";
pub const LAB_OFFLOAD_METADATA_ENV: &str = "HOMEBOY_LAB_OFFLOAD_JSON";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunContext {
    pub provenance: RunProvenance,
}

impl RunContext {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_provenance(provenance: RunProvenance) -> Self {
        Self { provenance }
    }

    pub fn subprocess_compatibility_from_env() -> Self {
        Self::from_provenance(RunProvenance {
            source_snapshot: env_json(SOURCE_SNAPSHOT_METADATA_ENV),
            lab_offload: env_json(LAB_OFFLOAD_METADATA_ENV),
            artifact_mirror: None,
        })
    }

    pub fn with_missing_from(mut self, fallback: Self) -> Self {
        self.provenance = self.provenance.with_missing_from(fallback.provenance);
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunProvenance {
    pub source_snapshot: Option<serde_json::Value>,
    pub lab_offload: Option<serde_json::Value>,
    pub artifact_mirror: Option<serde_json::Value>,
}

impl RunProvenance {
    pub fn with_source_snapshot(mut self, source_snapshot: impl Serialize) -> Self {
        self.source_snapshot = serde_json::to_value(source_snapshot).ok();
        self
    }

    pub fn with_lab_offload(mut self, lab_offload: impl Serialize) -> Self {
        self.lab_offload = serde_json::to_value(lab_offload).ok();
        self
    }

    pub fn with_artifact_mirror(mut self, artifact_mirror: impl Serialize) -> Self {
        self.artifact_mirror = serde_json::to_value(artifact_mirror).ok();
        self
    }

    fn with_missing_from(mut self, fallback: Self) -> Self {
        if self.source_snapshot.is_none() {
            self.source_snapshot = fallback.source_snapshot;
        }
        if self.lab_offload.is_none() {
            self.lab_offload = fallback.lab_offload;
        }
        if self.artifact_mirror.is_none() {
            self.artifact_mirror = fallback.artifact_mirror;
        }
        self
    }
}

fn env_json(name: &str) -> Option<serde_json::Value> {
    std::env::var(name)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
}
