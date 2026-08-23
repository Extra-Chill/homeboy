//! Pins production `HOME` resolution onto the process-local override.
//!
//! ## Why this is a different problem from rooting the stores
//!
//! `tests/nextest_shard_parallelism_test.rs` guards two thread settings that
//! look identical and are not:
//!
//! * `rust_nextest_shard_threads` — cleared once the observation stores took
//!   explicit roots. Nextest runs a process per test, so the only shared thing
//!   left was the filesystem, and injecting roots removed it.
//! * `rust_cargo_test_threads` — still pinned at `1`. libtest shares one
//!   process across its threads, so this race is *in-process* and, as that file
//!   says, "survives even if every store on disk becomes explicitly rooted."
//!
//! No amount of store rooting clears the second one. This file is the first leg
//! of the work that can.
//!
//! ## The race
//!
//! `HomeGuard::new` repoints the home for a test two ways: it registers a
//! process-local override, and it also calls `set_var("HOME", ..)`. The second
//! is retained deliberately — subprocesses read their own environment after
//! `fork`, and until now so did a good deal of in-process production code.
//!
//! `setenv` is not thread-safe. A concurrent `getenv` on another thread can
//! observe the variable *mid-write* — not merely stale, but absent. That is the
//! documented shape behind "HOME environment variable not set on Unix-like
//! system" failing on a machine where `HOME` is plainly set, and it is why
//! serializing the mutations behind a lock did not fix it: the readers never
//! took that lock.
//!
//! So every direct `HOME` read in production was a reader racing that write,
//! and each one is a reason the Cargo runner cannot go above one thread.
//!
//! ## What this test pins
//!
//! Exactly one production reader of `HOME` exists, and it is the resolver in
//! `homeboy-paths` that consults the override first. Everything else calls
//! `paths::home_root()` and reads the value under a `Mutex`, so a concurrent
//! repoint is ordered rather than torn.
//!
//! Tests and the code-audit detectors are excluded: the detectors match on
//! `HOME` as *data* to find this very class of defect, and test code that
//! captures and restores `HOME` is doing so under a guard.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn tracked_rust_sources() -> Vec<PathBuf> {
    let output = Command::new("git")
        .args(["ls-files", "crates/*.rs"])
        .current_dir(repo_root())
        .output()
        .expect("git ls-files runs");
    assert!(output.status.success(), "git ls-files failed");

    String::from_utf8(output.stdout)
        .expect("git output is utf-8")
        .lines()
        .map(PathBuf::from)
        .collect()
}

/// Test code may capture and restore `HOME`; it does so under a guard.
fn is_test_surface(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.contains("/tests/")
        || text.ends_with("tests.rs")
        || text.ends_with("_test.rs")
        || text.ends_with("test_support.rs")
        // The audit detectors carry `HOME` as data: they exist to find exactly
        // the defect this file pins, so their own literals are not readers.
        || text.contains("homeboy-code-audit")
}

fn production_body(path: &Path) -> String {
    let source = std::fs::read_to_string(repo_root().join(path))
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    source
        .split_once("\n#[cfg(test)]")
        .map(|(production, _)| production.to_string())
        .unwrap_or(source)
}

#[test]
fn only_the_paths_resolver_reads_home_directly() {
    let mut readers = Vec::new();

    for path in tracked_rust_sources() {
        if is_test_surface(&path) {
            continue;
        }
        let body = production_body(&path);
        for (offset, _) in body.match_indices("env::var") {
            let tail = &body[offset..];
            if !tail.starts_with("env::var(\"HOME\")") && !tail.starts_with("env::var_os(\"HOME\")")
            {
                continue;
            }
            // File, not line: the invariant is *which* code reads the
            // variable, and pinning line numbers would make every unrelated
            // edit above a reader look like a policy change.
            let _ = offset;
            readers.push(path.display().to_string());
        }
    }

    readers.sort();
    readers.dedup();

    let expected = vec![
        // The resolver itself. Consults the override, then falls back.
        "crates/homeboy-core/src/engine/invocation/runtime.rs".to_string(),
        // Deliberate, and documented at the call site. #11266 routed this
        // through `paths` and broke
        // `promotion_gate_binds_a_socket_in_the_short_invocation_tmpdir_for_a_long_run_id`
        // in every run after. The invocation runtime root is separately pinned
        // by `HOMEBOY_INVOCATION_RUNTIME_DIR_ENV` for every hermetic test, so
        // routing it through the override buys no isolation and re-adds the
        // blast radius that regression came from. Socket paths under this root
        // must also stay inside the `sockaddr_un` budget, which makes its shape
        // load-bearing in a way the config root's is not.
        "crates/homeboy-paths/src/lib.rs".to_string(),
    ];

    assert_eq!(
        readers, expected,
        "\nProduction `HOME` readers changed.\n\n\
         Two are allowed, and the second is an exception with a named \
         regression behind it — read the comment at that call site before \
         touching it. Every other reader races `HomeGuard`'s \
         `set_var(\"HOME\", ..)`: `setenv` is not thread-safe, so a concurrent \
         `getenv` can observe the variable mid-write rather than merely \
         stale.\n\n\
         Each such reader is a reason `rust_cargo_test_threads` cannot leave 1 \
         (#7505). Call `paths::home_root()` instead.\n\n\
         Found: {readers:#?}"
    );
}

/// The override-aware accessor stays available to every crate that needed it.
#[test]
fn the_override_aware_accessor_is_public() {
    let source = production_body(Path::new("crates/homeboy-paths/src/lib.rs"));

    assert!(
        source.contains("pub fn home_root() -> Result<PathBuf>"),
        "`paths::home_root()` is gone or no longer public. It is the only \
         sanctioned way for production code to resolve the home directory; \
         without it the readers have nowhere to go but `HOME` (#7505)."
    );
}
