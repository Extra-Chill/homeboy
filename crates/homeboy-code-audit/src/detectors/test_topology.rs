//! Test topology audit.
//!
//! This detector emits standalone `vacuous_test` findings via `test_quality`.
//!
//! It used to carry a second, script-driven branch: extensions were meant to
//! classify files as source/test through a `scripts.topology` manifest key, and
//! `audit_rules.test_topology` in `homeboy.json` enforced placement policy on
//! the result. That branch was removed in #11124 because it was unreachable in
//! production twice over — no shipped extension has ever declared
//! `scripts.topology`, and no `homeboy.json` has ever set
//! `audit_rules.test_topology.enabled`. The only thing exercising it was its
//! own unit test, which is the definition of a declaration that is decoration.
//!
//! The protocol it defined should not be restored as-written. It spawned one
//! child process *per file per extension*, feeding the whole file body in on
//! stdin, so the first extension to declare the key would have turned a single
//! audit run into thousands of process spawns. A replacement wants a batch
//! protocol — one invocation handed the file list — and it needs to arrive with
//! a producer, not ahead of one.

use std::path::Path;

use super::findings::Finding;

#[path = "../test_quality.rs"]
mod test_quality;

pub(crate) fn run(root: &Path) -> Vec<Finding> {
    let mut findings = test_quality::run(root);
    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let findings = run(dir.path());
        assert!(findings.is_empty());
    }
}
