//! Monotonic shrink ratchet for module-level `#[allow(dead_code)]`.
//!
//! A `dead_code` allow written above a `mod` declaration switches the lint off
//! for that entire subtree. It is one line, it reads like housekeeping, and it
//! is the single cheapest way to make a large amount of code stop being
//! checked. Before the 2026-08 sweep this repo carried 40 of them covering
//! **599,992 lines — 52% of the tree**. `pub mod commands` alone was 216,724
//! lines behind one attribute.
//!
//! The cost is not the dead code itself. It is that the compiler reports a
//! clean build while more than half the codebase is exempt from the check, so
//! "zero warnings" stops meaning anything. Removing those attributes across
//! homeboy-lab-runner (#12866), homeboy-cli (#12882, #12954) and homeboy-core
//! (#12912) deleted roughly 8,900 lines and, within hours of landing, caught
//! five dead items that arrived from unrelated PRs the same day.
//!
//! Nothing prevents one line putting a subtree back in the dark. This test is
//! that thing.
//!
//! ## Why per-item allows are not counted
//!
//! `#[allow(dead_code, reason = "...")]` on a single function, field or variant
//! is a different act. It names one item, it survives review as a claim about
//! that item, and the next unused member of the same impl still fails the
//! build. Several land deliberately in the sweep commits — a Windows-only
//! predicate, a partition table asserted by one test, a payload read only by
//! test assertions. Those are decisions. A module attribute is an opt-out.
//!
//! ## Changing the ceiling
//!
//! Down, in the same PR that removes the attribute. Never up. Raising
//! `MODULE_SUPPRESSION_CEILING` is the one edit this test exists to make
//! someone argue for in review.

use std::path::{Path, PathBuf};

/// Maximum permitted number of module-level `dead_code` suppressions.
///
/// This is the exact count as of the commit that introduced this ratchet, down
/// from 40 before the sweep. It is a ceiling, not a target: lower it whenever
/// a crate is cleared, and never raise it.
const MODULE_SUPPRESSION_CEILING: usize = 16;

/// One module-level suppression, located precisely enough to act on.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ModuleSuppression {
    file: String,
    line: usize,
    module: String,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                // `target/` is build output and `.git/` is not source.
                if name == "target" || name.starts_with('.') {
                    continue;
                }
                stack.push(path);
            } else if name.ends_with(".rs") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Is this line the start of an attribute that silences `dead_code`?
///
/// Attributes wrap across lines in this repo because they usually carry a
/// `reason = "..."`, so the scan joins from `#[allow(` to the closing `)]`
/// before deciding.
fn attribute_span(lines: &[&str], start: usize) -> Option<(usize, String)> {
    if !lines[start].trim_start().starts_with("#[allow(") {
        return None;
    }
    let mut end = start;
    let mut joined = String::new();
    while end < lines.len() {
        joined.push_str(lines[end].trim());
        joined.push(' ');
        if lines[end].contains(")]") {
            break;
        }
        end += 1;
    }
    joined.contains("dead_code").then_some((end, joined))
}

/// The declaration an attribute applies to, skipping any further attributes,
/// doc comments and blank lines between them.
fn attached_item<'a>(lines: &[&'a str], after: usize) -> Option<&'a str> {
    let mut index = after + 1;
    while index < lines.len() {
        let line = lines[index].trim();
        if line.is_empty() || line.starts_with("#[") || line.starts_with("//") {
            index += 1;
            continue;
        }
        return Some(line);
    }
    None
}

/// `pub mod foo;`, `pub(crate) mod foo;`, `mod foo;` — a declaration, not an
/// inline `mod foo { .. }` block, which is scoped by its own braces and is not
/// the pattern this ratchet is about.
fn declared_module(item: &str) -> Option<String> {
    let rest = item
        .strip_prefix("pub(crate) ")
        .or_else(|| item.strip_prefix("pub(super) "))
        .or_else(|| item.strip_prefix("pub "))
        .unwrap_or(item);
    let name = rest.strip_prefix("mod ")?.strip_suffix(';')?.trim();
    (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .then(|| name.to_string())
}

fn module_suppressions() -> Vec<ModuleSuppression> {
    let root = repo_root();
    let mut found = Vec::new();
    for path in rust_sources(&root) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        let mut index = 0;
        while index < lines.len() {
            let Some((end, _)) = attribute_span(&lines, index) else {
                index += 1;
                continue;
            };
            if let Some(module) = attached_item(&lines, end).and_then(declared_module) {
                found.push(ModuleSuppression {
                    file: relative.clone(),
                    line: index + 1,
                    module,
                });
            }
            index = end + 1;
        }
    }
    found.sort();
    found
}

fn render(found: &[ModuleSuppression]) -> String {
    found
        .iter()
        .map(|item| format!("    {}:{}  mod {}", item.file, item.line, item.module))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn module_level_dead_code_suppression_may_only_shrink() {
    let found = module_suppressions();
    let actual = found.len();
    let ceiling = MODULE_SUPPRESSION_CEILING;
    let over = actual.saturating_sub(ceiling);
    let listing = render(&found);

    assert!(
        actual <= ceiling,
        r#"
module-level dead_code suppression grew.

  ceiling: {ceiling}
  actual:  {actual}   (+{over})

An `#[allow(dead_code)]` above a `mod` declaration turns the lint off for that
entire subtree. It is one line and it silently removes an unbounded amount of
code from the check, which is how this repo reached 40 of them covering 52% of
the tree.

To land this change, do one of:

  1. Delete the dead code instead of hiding the module. This is the default.
  2. Suppress the single item rather than the module:
     #[allow(dead_code, reason = "<why this item specifically>")]
     A per-item allow still fails the build on the NEXT unused item beside it.
  3. Remove {over} existing module suppression(s) in this same PR so the total
     does not rise.
  4. If the ceiling is genuinely wrong, lower nothing and raise
     MODULE_SUPPRESSION_CEILING in tests/dead_code_suppression_ratchet_test.rs
     in this same PR, and say why in the PR body. Expect to be asked.

Current module-level suppressions:
{listing}

See docs/audit/dead-code-suppression-ratchet.md
"#
    );
}

#[test]
fn module_suppression_ceiling_leaves_no_slack() {
    let actual = module_suppressions().len();
    let ceiling = MODULE_SUPPRESSION_CEILING;
    let under = ceiling.saturating_sub(actual);

    assert!(
        actual >= ceiling,
        r#"
module-level dead_code suppression shrank -- good. Now lock it in.

  ceiling: {ceiling}
  actual:  {actual}   (-{under})

Lower MODULE_SUPPRESSION_CEILING in
tests/dead_code_suppression_ratchet_test.rs to {actual} in this same PR.

The gap is not harmless: a ceiling above the real count is room for {under} new
module suppression(s) to be added later without any test noticing. A ratchet
that keeps slack is not a ratchet.

See docs/audit/dead-code-suppression-ratchet.md
"#
    );
}

#[test]
fn the_swept_crates_stay_swept() {
    // homeboy-lab-runner (#12866), homeboy-cli (#12882, #12954) and
    // homeboy-core (#12912) were cleared to zero module suppressions and their
    // dead code deleted. Re-adding one here would put six figures of lines back
    // in the dark in a single line, and the aggregate ceiling above would not
    // notice if another crate happened to lose one in the same PR.
    const SWEPT: [&str; 3] = [
        "crates/homeboy-lab-runner/",
        "crates/homeboy-cli/",
        "crates/homeboy-core/",
    ];

    let offenders: Vec<&ModuleSuppression> = module_suppressions()
        .leak()
        .iter()
        .filter(|item| {
            SWEPT
                .iter()
                .any(|crate_path| item.file.starts_with(crate_path))
        })
        .collect();

    let listing = offenders
        .iter()
        .map(|item| format!("    {}:{}  mod {}", item.file, item.line, item.module))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        offenders.is_empty(),
        r#"
a swept crate regained a module-level dead_code suppression:

{listing}

homeboy-lab-runner, homeboy-cli and homeboy-core were cleared to zero and their
dead code deleted (#12866, #12882, #12912, #12954). Suppress the specific item
with a reason instead, or delete it.

See docs/audit/dead-code-suppression-ratchet.md
"#
    );
}
