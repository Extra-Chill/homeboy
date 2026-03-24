//! release_step_type — extracted from types.rs.

pub enum ReleaseStepType {
    Version,
    GitCommit,
    GitTag,
    GitPush,
    Package,
    Publish(String),
    Cleanup,
    PostRelease,
}
