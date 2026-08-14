//! Extension-declared inputs for the test inventory contract.
//!
//! The inventory contract (`homeboy/test-inventory/v1`) is what makes sharded
//! test execution possible: an extension enumerates its tests without running
//! them, Homeboy binds that enumeration to a fingerprint of the workspace and
//! the runner, and each shard replays an immutable slice of it.
//!
//! Before this config existed, every input to that binding was derived from
//! Cargo: the workspace root came from `cargo metadata`, the workspace
//! fingerprint hashed only `Cargo.toml`/`Cargo.lock`/`*.rs`, and the runner
//! fingerprint shelled out to `cargo --version`. That made the whole mechanism
//! structurally Rust-only rather than merely unimplemented elsewhere — a
//! non-Rust extension could emit a perfectly valid inventory document and
//! Homeboy would never bind, validate, or accept it. (#12394)
//!
//! Declaring `test.inventory` opts an extension into the contract on its own
//! terms. Omitting it preserves the Cargo-derived behaviour exactly, so Rust is
//! unaffected and needs no manifest change.

use serde::{Deserialize, Serialize};

/// How Homeboy resolves the inventory workspace root, fingerprints it, and
/// identifies the runner that produced an inventory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestInventoryConfig {
    /// Marker file names identifying the workspace root, searched upward from
    /// the component source path. The first ancestor containing any marker
    /// wins. When empty, the component source path is the root.
    ///
    /// This mirrors what `cargo metadata` does for Rust without requiring a
    /// toolchain-specific subprocess.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub root_markers: Vec<String>,

    /// Exact file names whose contents feed the workspace fingerprint.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fingerprint_names: Vec<String>,

    /// File extensions (without the dot) whose contents feed the workspace
    /// fingerprint.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fingerprint_extensions: Vec<String>,

    /// Directory names never descended into while fingerprinting. Build output
    /// and VCS metadata belong here: including them makes the fingerprint
    /// unstable across otherwise identical checkouts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fingerprint_skip_dirs: Vec<String>,

    /// Runner identities this extension may report in an inventory's `runner`
    /// field, each with the argv that reports its version. An inventory naming
    /// a runner absent from this list is rejected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runners: Vec<TestInventoryRunner>,
}

impl TestInventoryConfig {
    /// A fingerprint that selects no files is not a fingerprint of anything: it
    /// hashes the empty string for every workspace, so any two checkouts of any
    /// two components compare equal. Reject that rather than bind an inventory
    /// to a constant.
    pub fn selects_files(&self) -> bool {
        !self.fingerprint_names.is_empty() || !self.fingerprint_extensions.is_empty()
    }

    /// Look up the version argv for a runner identity.
    pub fn runner(&self, id: &str) -> Option<&TestInventoryRunner> {
        self.runners.iter().find(|runner| runner.id == id)
    }
}

/// One runner identity and how to report its version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestInventoryRunner {
    /// Value the extension writes to the inventory's `runner` field.
    pub id: String,
    /// Argv whose stdout identifies the runner build. The first element is the
    /// executable. Its output is hashed, never parsed, so any stable version
    /// string works.
    pub version_command: Vec<String>,
}

impl TestInventoryRunner {
    /// A runner with no executable cannot be fingerprinted.
    pub fn is_executable(&self) -> bool {
        self.version_command
            .first()
            .is_some_and(|program| !program.trim().is_empty())
    }
}
