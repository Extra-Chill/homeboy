//! Fails when an ambient wrapper loses its last caller.
//!
//! ## Why this exists
//!
//! #7505 replaces ambient resolution with injected roots. The only safe order
//! is top-down, and it makes the codebase temporarily *worse*: each converted
//! function becomes a pair.
//!
//! ```text
//! pub fn thing(args)             -> thing_in_store(&Store::from_current_environment()?, args)
//! pub fn thing_in_store(s, args) -> ...the actual work...
//! ```
//!
//! The ambient half has to stay while any caller still needs it. The payoff is
//! entirely at the end: when the last caller is rooted, the ambient half is
//! dead and gets deleted. There are 443 such pairs in the tree right now, so
//! that end state deletes several hundred functions.
//!
//! The failure mode this guards is quiet. A PR roots the final caller of some
//! wrapper, the wrapper is now unreachable, and nothing says so — `pub` items
//! do not trip dead-code warnings across crate boundaries. The debt just sits
//! there looking load-bearing, and the next person to read it reasonably
//! assumes it is.
//!
//! So the moment a wrapper is orphaned, this test says so and names it. That
//! turns a migration whose value arrives only at the end into one that pays out
//! continuously.
//!
//! ## What counts as orphaned
//!
//! A function `foo` is orphaned when:
//!
//! * a rooted sibling exists — `foo_in_store`, `foo_in_root`, `foo_in_roots`,
//!   or `foo_at`; and
//! * the name `foo` appears nowhere in the tree except its own definition.
//!
//! The second condition is deliberately blunt. It counts doc links, re-exports,
//! `use` lists, and method-position calls as uses, because the cost of a false
//! "delete this" is someone deleting live code, while the cost of a missed
//! orphan is that it gets caught on the next pass.
//!
//! An earlier version of this scan indexed only `crates/**` and reported 157
//! orphans. Most were method calls it could not see, and five were used from
//! the repo-root `tests/` tree it never opened. The real number was 2. Scan
//! everything, and count generously.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

const ROOTED_SUFFIXES: [&str; 4] = ["_in_store", "_in_roots", "_in_root", "_at"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn tracked_rust_sources() -> Vec<PathBuf> {
    let output = Command::new("git")
        .args(["ls-files", "*.rs"])
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

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Every `fn NAME(` in `source`.
fn definitions(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (offset, _) in source.match_indices("fn ") {
        let before = source[..offset].chars().next_back();
        if before.is_some_and(is_ident_char) {
            continue;
        }
        let rest = &source[offset + 3..];
        let name: String = rest.chars().take_while(|c| is_ident_char(*c)).collect();
        if name.is_empty() {
            continue;
        }
        if rest[name.len()..].trim_start().starts_with('(') {
            found.push(name);
        }
    }
    found
}

/// Count every mention of `name` as a whole word.
fn mentions(source: &str, name: &str) -> usize {
    let mut count = 0;
    for (offset, _) in source.match_indices(name) {
        let before = source[..offset].chars().next_back();
        let after = source[offset + name.len()..].chars().next();
        if before.is_some_and(is_ident_char) || after.is_some_and(is_ident_char) {
            continue;
        }
        count += 1;
    }
    count
}

#[test]
fn no_ambient_wrapper_has_lost_its_last_caller() {
    let sources: Vec<(PathBuf, String)> = tracked_rust_sources()
        .into_iter()
        .filter_map(|path| {
            std::fs::read_to_string(repo_root().join(&path))
                .ok()
                .map(|text| (path, text))
        })
        .collect();

    let mut definition_counts: HashMap<String, usize> = HashMap::new();
    for (_, text) in &sources {
        for name in definitions(text) {
            *definition_counts.entry(name).or_default() += 1;
        }
    }

    // Ambient halves that still have a rooted sibling, found in production code.
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (path, text) in &sources {
        let display = path.display().to_string();
        if display.contains("/tests/") || display.ends_with("tests.rs") {
            continue;
        }
        for name in definitions(text) {
            let Some(base) = ROOTED_SUFFIXES
                .iter()
                .find_map(|suffix| name.strip_suffix(suffix))
            else {
                continue;
            };
            if definition_counts.contains_key(base) {
                pairs.push((base.to_string(), display.clone()));
            }
        }
    }
    pairs.sort();
    pairs.dedup();

    let mut orphaned: Vec<String> = Vec::new();
    for (base, path) in &pairs {
        let total: usize = sources.iter().map(|(_, text)| mentions(text, base)).sum();
        // Its own definition is the only mention left.
        if total <= definition_counts.get(base).copied().unwrap_or(0) {
            orphaned.push(format!("{base}  ({path})"));
        }
    }
    orphaned.sort();
    orphaned.dedup();

    assert!(
        orphaned.is_empty(),
        "\n{} ambient wrapper(s) have no callers left:\n\n{}\n\n\
         Each of these has a rooted sibling and nothing else references it, so \
         rooting its last caller finished the job — delete the wrapper (#7505).\n\n\
         This is the payoff half of the migration. Converting a function adds a \
         pair; the pair collapses only when the last ambient caller is gone, and \
         `pub` items do not warn when they become unreachable across crates. \
         Without this test that debt accumulates silently and reads as \
         load-bearing.\n\n\
         If one of these is genuinely a public API kept for external callers, \
         it is not orphaned — it is exported, and that export is the caller. \
         Say so at the definition.\n",
        orphaned.len(),
        orphaned
            .iter()
            .map(|entry| format!("  - {entry}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
