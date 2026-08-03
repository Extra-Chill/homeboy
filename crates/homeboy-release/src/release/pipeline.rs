//! Release pipeline public facade.
//!
//! Planning lives in `planner`; release execution lives in `orchestrator`.

pub use super::orchestrator::run;
pub(crate) use super::orchestrator::run_with_plan;

#[cfg(test)]
mod tests {
    #[test]
    fn release_runtime_core_stays_ecosystem_agnostic() {
        // `version.rs` moved to `homeboy-version` (#11144). Its half of this
        // invariant moved with it — see `version_core_stays_ecosystem_agnostic`
        // there — rather than being asserted through a cross-crate
        // `include_str!` path, which would silently stop compiling if the file
        // were ever relocated again.
        let files = [
            ("executor.rs", include_str!("executor.rs")),
            ("pipeline.rs", include_str!("pipeline.rs")),
        ];
        let forbidden_terms = ["Cargo", "cargo", "Rust", "rust"];

        for (file, source) in files {
            let runtime_source = source.split("#[cfg(test)]").next().unwrap_or(source);
            for term in forbidden_terms {
                assert!(
                    !runtime_source.contains(term),
                    "release runtime core must not branch on ecosystem-specific term {term:?} in {file}"
                );
            }
        }
    }
}
