//! Duplication detection — find identical and near-identical functions across
//! source files, and duplicated code blocks within a single method.
//!
//! Uses method body hashes from fingerprinting to detect exact duplicates,
//! and structural hashes (identifiers/literals normalized to positional tokens)
//! to detect near-duplicates — functions with identical control flow that differ
//! only in variable names, constant references, or string values.
//!
//! Four outputs:
//! - `detect_duplicates()` → flat `Vec<Finding>` for exact duplicates
//! - `detect_duplicate_groups()` → structured `Vec<DuplicateGroup>` for the fixer
//! - `detect_near_duplicates()` → flat `Vec<Finding>` for structural near-duplicates
//! - `detect_intra_method_duplicates()` → duplicated blocks within a single method

use std::collections::{HashMap, HashSet};

use super::conventions::{AuditFinding, Language};
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;
use super::idiomatic::is_trivial_method;
use super::walker::is_test_path;
use crate::core::component::DuplicationDetectorConfig;

mod intra_method;

pub(crate) use intra_method::detect_intra_method_duplicates;
#[cfg(test)]
use intra_method::{has_logic_signal, is_scaffolding_line};

/// Minimum number of locations for a function to count as duplicated.
const MIN_DUPLICATE_LOCATIONS: usize = 2;

/// A group of files containing an identical function.
///
/// The fixer uses this to keep the canonical copy and remove the rest.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DuplicateGroup {
    /// The duplicated function name.
    pub function_name: String,
    /// File chosen to keep the function (canonical location).
    pub canonical_file: String,
    /// Files where the duplicate should be removed and replaced with an import.
    pub remove_from: Vec<String>,
}

/// Build grouped duplication data from fingerprints.
///
/// For each group of identical functions, picks a canonical file (shortest
/// path, then alphabetical) and lists the rest as removal targets.
fn build_groups(fingerprints: &[&FileFingerprint]) -> HashMap<(String, String), Vec<String>> {
    let mut hash_groups: HashMap<(String, String), Vec<String>> = HashMap::new();

    for fp in fingerprints {
        for (method_name, body_hash) in &fp.method_hashes {
            hash_groups
                .entry((method_name.clone(), body_hash.clone()))
                .or_default()
                .push(fp.relative_path.clone());
        }
    }

    hash_groups
}

/// Pick the canonical file from a list of locations.
///
/// Heuristics (in order):
/// 1. Files in a `utils/` directory are preferred (already shared)
/// 2. Shortest path (most general module)
/// 3. Alphabetical (deterministic tiebreaker)
fn pick_canonical(locations: &[String]) -> String {
    let mut sorted = locations.to_vec();
    sorted.sort_by(|a, b| {
        let a_utils = a.contains("/utils/") || a.contains("/utils.");
        let b_utils = b.contains("/utils/") || b.contains("/utils.");
        // utils files first
        b_utils
            .cmp(&a_utils)
            // then shortest path
            .then_with(|| a.len().cmp(&b.len()))
            // then alphabetical
            .then_with(|| a.cmp(b))
    });
    sorted[0].clone()
}

/// Detect duplicate groups with canonical file selection.
///
/// Returns structured data the fixer uses to remove duplicates.
pub(crate) fn detect_duplicate_groups(fingerprints: &[&FileFingerprint]) -> Vec<DuplicateGroup> {
    let hash_groups = build_groups(fingerprints);
    let mut groups = Vec::new();

    for ((method_name, _hash), locations) in &hash_groups {
        if locations.len() < MIN_DUPLICATE_LOCATIONS {
            continue;
        }

        let canonical = pick_canonical(locations);
        let mut remove_from: Vec<String> = locations
            .iter()
            .filter(|f| **f != canonical)
            .cloned()
            .collect();
        remove_from.sort();

        groups.push(DuplicateGroup {
            function_name: method_name.clone(),
            canonical_file: canonical,
            remove_from,
        });
    }

    groups.sort_by(|a, b| a.function_name.cmp(&b.function_name));
    groups
}

/// Detect duplicated functions across all fingerprinted files.
///
/// Groups functions by their body hash. When two or more files contain a
/// function with the same name and the same normalized body hash, a finding
/// is emitted for each location.
/// Detect exact function body duplicates across files.
///
/// `convention_methods` are excluded — identical implementations across convention-
/// following files are expected behavior (e.g. `__construct`, `checkPermission`,
/// interface methods with identical bodies).
pub(crate) fn detect_duplicates(
    fingerprints: &[&FileFingerprint],
    convention_methods: &std::collections::HashSet<String>,
) -> Vec<Finding> {
    let hash_groups = build_groups(fingerprints);
    let mut findings = Vec::new();

    for ((method_name, _hash), locations) in &hash_groups {
        if locations.len() < MIN_DUPLICATE_LOCATIONS {
            continue;
        }

        // Skip convention-expected methods — identical implementations are by design.
        if convention_methods.contains(method_name) {
            continue;
        }

        let test_only_duplicate = locations.iter().all(|file| is_test_path(file));
        let severity = if test_only_duplicate {
            Severity::Info
        } else {
            Severity::Warning
        };
        let suggestion = if test_only_duplicate {
            format!(
                "Function `{}` has identical body in {} test files. Consider a shared test helper if the duplication grows or starts obscuring test intent.",
                method_name,
                locations.len()
            )
        } else {
            format!(
                "Function `{}` has identical body in {} files. \
             Extract to a shared module and import it.",
                method_name,
                locations.len()
            )
        };

        // Emit one finding per file that has the duplicate
        for file in locations {
            let mut also_in_vec: Vec<_> =
                locations.iter().filter(|f| *f != file).cloned().collect();
            also_in_vec.sort();
            let also_in = also_in_vec.join(", ");

            findings.push(Finding {
                convention: "duplication".to_string(),
                severity: severity.clone(),
                file: file.clone(),
                description: format!("Duplicate function `{}` — also in {}", method_name, also_in),
                suggestion: suggestion.clone(),
                kind: AuditFinding::DuplicateFunction,
            });
        }
    }

    // Sort by file path then description for deterministic output
    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.description.cmp(&b.description))
    });
    findings
}

// ============================================================================
// Near-Duplicate Detection (structural similarity)
// ============================================================================

/// Names that are too generic to flag as near-duplicates.
/// These appear in many files with completely unrelated implementations.
const GENERIC_NAMES: &[&str] = &[
    "run", "new", "default", "build", "list", "show", "set", "get", "delete", "remove", "clear",
    "create", "update", "status", "search", "find", "read", "write", "rename", "init", "test",
    "fmt", "from", "into", "clone", "drop", "display", "parse", "validate", "execute", "handle",
    "process", "merge", "resolve", "pin", "plan",
];

/// Minimum body line count — skip trivial functions (1-2 line bodies).
/// Functions like `fn default_true() -> bool { true }` are too small
/// to meaningfully refactor into shared code with a parameter.
///
/// Counted against `count_body_lines`, which returns the count of lines
/// strictly between the opening and closing braces (so a single-line body
/// is 0 and the standard three-line shape is 1).
const MIN_BODY_LINES: usize = 3;

/// Build structural hash groups from fingerprints.
///
/// Groups functions by (name, structural_hash), returning only groups
/// where the exact body hashes differ (otherwise they'd already be caught
/// by the exact-duplicate detector).
fn build_structural_groups(
    fingerprints: &[&FileFingerprint],
) -> HashMap<(String, String), Vec<(String, String)>> {
    // Collect: (fn_name, structural_hash) → [(file, body_hash), ...]
    let mut groups: HashMap<(String, String), Vec<(String, String)>> = HashMap::new();

    for fp in fingerprints {
        for (method_name, struct_hash) in &fp.structural_hashes {
            groups
                .entry((method_name.clone(), struct_hash.clone()))
                .or_default()
                .push((
                    fp.relative_path.clone(),
                    fp.method_hashes
                        .get(method_name)
                        .cloned()
                        .unwrap_or_default(),
                ));
        }
    }

    groups
}

/// Check if a file path looks like a CLI command module.
///
/// Command modules (`src/commands/*.rs`) are expected to have identically-
/// named functions (`run`, `list`, etc.) with completely different bodies.
fn is_command_file(path: &str) -> bool {
    path.contains("/commands/") || path.starts_with("commands/")
}

/// Count the body lines of a function in a file's structural hash data.
///
/// Returns the count of lines **strictly between** the line containing the
/// opening `{` and the line containing the matching `}` — the actual body,
/// not the wrapping span. So:
///
/// - `fn x() -> u32 { 0 }` (single-line body, both braces on the same line)
///   returns **0** — there are no lines strictly between the braces.
/// - The standard three-line shape
///   ```text
///   fn x() -> u32 {
///       0
///   }
///   ```
///   returns **1** — exactly the one body line.
/// - A genuine N-statement body returns ~N.
///
/// Returns 0 if the function is not found or its content is empty. The
/// previous implementation returned the **inclusive line span** from `fn`
/// to the closing brace, which off-by-twoed three-line delegation methods
/// like `pub fn len(&self) -> usize { self.inner.len() }` to a count of 3
/// and slipped them past the `< MIN_BODY_LINES` filter (#1517).
fn count_body_lines(fp: &FileFingerprint, method_name: &str) -> usize {
    let pattern = format!("fn {}", method_name);
    let lines: Vec<&str> = fp.content.lines().collect();
    let mut start = None;

    for (i, line) in lines.iter().enumerate() {
        if line.contains(&pattern) {
            start = Some(i);
            break;
        }
    }

    let Some(start_idx) = start else { return 0 };

    let mut brace_depth = 0i32;
    let mut open_line: Option<usize> = None;
    for (offset, line) in lines[start_idx..].iter().enumerate() {
        let line_idx = start_idx + offset;
        for ch in line.chars() {
            if ch == '{' {
                if open_line.is_none() {
                    open_line = Some(line_idx);
                }
                brace_depth += 1;
            } else if ch == '}' {
                brace_depth -= 1;
                if let Some(open) = open_line {
                    if brace_depth == 0 {
                        return line_idx.saturating_sub(open).saturating_sub(1);
                    }
                }
            }
        }
    }

    0
}

/// Detect structural near-duplicates across all fingerprinted files.
///
/// Groups functions by (name, structural_hash). When two or more files
/// contain a function with the same name and the same structural hash
/// but *different* exact body hashes, it means the functions have
/// identical control flow but differ in identifiers/constants.
///
/// Filters out:
/// - Functions already caught by exact-duplicate detection
/// - Generic names (`run`, `list`, `show`, etc.)
/// - Universally-idiomatic method names (`len`, `is_empty`, `iter`, `new`,
///   `default`, `from`, `into`, `clone`, `fmt`, etc. — see
///   `super::idiomatic::is_trivial_method`)
/// - Command/core delegation pairs (command module ↔ core module)
/// - Trivial functions (fewer than `MIN_BODY_LINES` body lines, where the
///   body line count is *strictly between the braces* — so a single-line
///   body is 0 and the standard three-line shape is 1)
pub(crate) fn detect_near_duplicates(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let structural_groups = build_structural_groups(fingerprints);
    let exact_groups = build_groups(fingerprints);

    // Collect exact-duplicate (name, hash) pairs for exclusion
    let exact_duplicate_names: std::collections::HashSet<String> = exact_groups
        .iter()
        .filter(|(_, locs)| locs.len() >= MIN_DUPLICATE_LOCATIONS)
        .map(|((name, _), _)| name.clone())
        .collect();

    let mut findings = Vec::new();

    for ((method_name, _struct_hash), file_hashes) in &structural_groups {
        // Need at least 2 locations
        if file_hashes.len() < MIN_DUPLICATE_LOCATIONS {
            continue;
        }

        // Skip if already an exact duplicate
        if exact_duplicate_names.contains(method_name) {
            continue;
        }

        // Skip generic names
        if GENERIC_NAMES.contains(&method_name.as_str()) {
            continue;
        }

        // Skip universally-idiomatic method names. `len`, `is_empty`, `iter`,
        // `new`, `default`, `from`, `into`, `clone`, `fmt`, `as_str`,
        // `to_string`, etc. are *expected* to have boilerplate-shaped bodies
        // across unrelated types — every collection wrapper looks the same,
        // and Clippy's `len_without_is_empty` lint actually *requires* you to
        // pair `len` with `is_empty`. Flagging these as duplication is a
        // false positive (#1517). Predicate is shared with `test_coverage`
        // via `super::idiomatic::is_trivial_method`. The near-duplicate pass has
        // no per-component test mapping config, so it uses the builtin agnostic
        // idiomatic name/prefix sets from the conventions home rather than
        // embedding any language literals here.
        if is_trivial_method(
            method_name,
            Language::builtin_trivial_method_names().iter().copied(),
            Language::builtin_trivial_method_prefixes().iter().copied(),
        ) {
            continue;
        }

        // Check that exact hashes actually differ (otherwise exact detection covers it)
        let unique_body_hashes: std::collections::HashSet<&str> =
            file_hashes.iter().map(|(_, h)| h.as_str()).collect();
        if unique_body_hashes.len() < 2 {
            continue;
        }

        let files: Vec<&str> = file_hashes.iter().map(|(f, _)| f.as_str()).collect();

        // Filter: skip if all files are command modules (delegation pattern)
        if files.iter().all(|f| is_command_file(f)) {
            continue;
        }

        // Filter: skip command↔core pairs where one is in commands/ and another in core/
        // These are the delegation pattern — the command calls the core function.
        let has_command = files.iter().any(|f| is_command_file(f));
        let has_non_command = files.iter().any(|f| !is_command_file(f));
        if has_command && has_non_command && files.len() == 2 {
            continue;
        }

        // Filter: skip trivial functions (< MIN_BODY_LINES)
        let body_lines: Vec<usize> = files
            .iter()
            .filter_map(|file_path| {
                fingerprints
                    .iter()
                    .find(|fp| fp.relative_path == *file_path)
                    .map(|fp| count_body_lines(fp, method_name))
            })
            .collect();
        if body_lines.iter().all(|&l| l < MIN_BODY_LINES) {
            continue;
        }

        let suggestion = format!(
            "Function `{}` has identical structure in {} files but different \
             identifiers/constants. Consider extracting shared logic into a \
             parameterized function.",
            method_name,
            files.len()
        );

        for (file, _body_hash) in file_hashes {
            let mut also_in_vec: Vec<&str> = file_hashes
                .iter()
                .filter(|(f, _)| f != file)
                .map(|(f, _)| f.as_str())
                .collect();
            also_in_vec.sort();
            let also_in = also_in_vec.join(", ");

            findings.push(Finding {
                convention: "near-duplication".to_string(),
                severity: Severity::Info,
                file: file.clone(),
                description: format!(
                    "Near-duplicate `{}` — structurally identical to {}",
                    method_name, also_in
                ),
                suggestion: suggestion.clone(),
                kind: AuditFinding::NearDuplicate,
            });
        }
    }

    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.description.cmp(&b.description))
    });
    findings
}

/// Find the body of a method/function in the file lines.
///
/// Returns `(open_brace_line, close_brace_line)` — the line indices of the
/// opening and closing braces. Searches for `function <name>` or `fn <name>`.
fn find_method_body(lines: &[&str], method_name: &str) -> Option<(usize, usize)> {
    let fn_pattern_php = format!("function {}", method_name);
    let fn_pattern_rust = format!("fn {}", method_name);

    let mut start_line = None;
    for (i, line) in lines.iter().enumerate() {
        if line.contains(&fn_pattern_php) || line.contains(&fn_pattern_rust) {
            start_line = Some(i);
            break;
        }
    }

    let start = start_line?;

    // Find opening brace from the function declaration line
    let mut brace_line = None;
    for (offset, line) in lines[start..].iter().enumerate() {
        if line.contains('{') {
            brace_line = Some(start + offset);
            break;
        }
    }

    let open_line = brace_line?;

    // Track brace depth to find closing brace
    let mut depth = 0i32;
    let mut found_open = false;
    for (i, line) in lines[open_line..].iter().enumerate() {
        for ch in line.chars() {
            if ch == '{' {
                depth += 1;
                found_open = true;
            } else if ch == '}' {
                depth -= 1;
            }
        }
        if found_open && depth == 0 {
            return Some((open_line, open_line + i));
        }
    }

    None
}

// ============================================================================
// Parallel Implementation Detection (call-sequence similarity)
// ============================================================================

/// Minimum number of function calls in a method body to consider it for
/// parallel implementation detection. Trivial methods (< 4 calls) are
/// too simple to meaningfully abstract.
const MIN_CALL_COUNT: usize = 4;

/// Minimum Jaccard similarity (|intersection| / |union|) between two
/// call sets to flag as a parallel implementation.
const MIN_JACCARD_SIMILARITY: f64 = 0.5;

/// Minimum longest-common-subsequence ratio to flag as parallel.
/// This captures sequential ordering — two methods that call helpers
/// in the same order score higher than ones that share calls but in
/// a different order.
const MIN_LCS_RATIO: f64 = 0.5;

/// Minimum number of shared (intersecting) calls between two methods
/// to flag as a parallel implementation. This prevents false positives
/// from methods that share only 1-2 trivial calls like `to_string`.
const MIN_SHARED_CALLS: usize = 3;

/// Minimum number of methods a call name must appear in before it can be
/// treated as corpus-common scaffolding for parallel-implementation scoring.
const MIN_COMMON_CALL_METHODS: usize = 8;

/// Minimum share of methods a call name must appear in before it can be
/// treated as corpus-common scaffolding for parallel-implementation scoring.
const COMMON_CALL_METHOD_RATIO: f64 = 0.10;

/// Raised Jaccard floor for two `StraightLine` bodies that share calls.
///
/// Without a loop or recursion two functions that overlap on stdlib
/// helpers (e.g. `fs::copy`, `create_dir_all`) carry weak workflow
/// signal — they are usually small focused helpers that happen to share
/// one stdlib pair. Force them to clear a much higher bar before flagging.
/// Loop/recursion pairs keep the standard `MIN_JACCARD_SIMILARITY`.
const STRAIGHT_LINE_JACCARD_FLOOR: f64 = 0.7;

/// Common plumbing calls that are useful in a method body but too generic to
/// carry signal for workflow-level similarity. Keep these out of the scoring
/// pass so filesystem scans, command wrappers, and terminal renderers do not
/// look like extractable parallel implementations.
const PLUMBING_CALLS: &[&str] = &[
    "args",
    "current_dir",
    "execute",
    "failure",
    "fix_deployed_permissions",
    "from_utf8_lossy",
    "is_dir",
    "is_terminal",
    "max",
    "output",
    "path",
    "quote_path",
    "read_dir",
    "read_to_string",
    "render_map",
    "run_git",
    "stderr",
    "success",
    "to_str",
];

/// Ubiquitous stdlib/trait method calls that appear in almost every function
/// and carry no signal for parallel implementation detection. Two functions
/// both calling `.to_string()` does not mean they implement the same workflow.
const TRIVIAL_CALLS: &[&str] = &[
    "to_string",
    "to_owned",
    "to_lowercase",
    "to_uppercase",
    "clone",
    "default",
    "new",
    "len",
    "is_empty",
    "is_some",
    "is_none",
    "is_ok",
    "is_err",
    "unwrap",
    "unwrap_or",
    "unwrap_or_default",
    "unwrap_or_else",
    "expect",
    "lines",
    "next",
    "ok_or_else",
    "as_str",
    "as_ref",
    "as_deref",
    "into",
    "from",
    "iter",
    "into_iter",
    "collect",
    "map",
    "filter",
    "any",
    "all",
    "find",
    "contains",
    "push",
    "pop",
    "insert",
    "remove",
    "extend",
    "join",
    "split",
    "split_whitespace",
    "trim",
    "starts_with",
    "ends_with",
    "strip_prefix",
    "strip_suffix",
    "replace",
    "display",
    "write",
    "read",
    "flush",
    "ok",
    "err",
    "map_err",
    "and_then",
    "or_else",
    "flatten",
    "take",
    "skip",
    "chain",
    "zip",
    "enumerate",
    "cloned",
    "copied",
    "rev",
    "sort",
    "sort_by",
    "dedup",
    "retain",
    "get",
    "set",
    "entry",
    "or_insert",
    "or_insert_with",
    "keys",
    "values",
    "exists",
    "parent",
    "file_name",
    "extension",
    "with_extension",
];

/// Generic, language-agnostic structural shape of a function body.
///
/// Used as a gate before flagging two methods as parallel implementations:
/// a 22-line straight-line copy helper and a recursive directory walk can
/// share the same call set (`copy`, `create_dir_all`, …) yet have nothing
/// in common at the workflow level. Requiring shape compatibility kills
/// that false positive without leaning on language-specific identifiers.
///
/// Detection is purely lexical so it works for Rust, Python, JS, PHP, Go,
/// etc. — see [`detect_body_shape`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyShape {
    /// Body contains at least one loop construct or iterator-pipeline call.
    Looping,
    /// Body calls its own function name (direct recursion).
    Recursive,
    /// Body contains neither a loop nor a self-call.
    StraightLine,
}

impl BodyShape {
    /// True when two shapes can plausibly implement the same workflow.
    ///
    /// Looping and Recursive are both "iterates over something" and freely
    /// match each other — a recursive directory walk and a `for` loop over
    /// `read_dir` are genuinely interchangeable. StraightLine only matches
    /// itself; pairing a straight-line helper with a loop is the FP shape.
    fn compatible_with(self, other: BodyShape) -> bool {
        use BodyShape::*;
        matches!(
            (self, other),
            (Looping, Looping)
                | (Looping, Recursive)
                | (Recursive, Looping)
                | (Recursive, Recursive)
                | (StraightLine, StraightLine)
        )
    }
}

/// Per-method call sequence extracted from file content.
#[derive(Debug)]
struct MethodCallSequence {
    file: String,
    method: String,
    /// Ordered list of function/method calls made in the body.
    calls: Vec<String>,
    /// Generic structural shape of the body — used as a gate before flagging
    /// two methods as parallel implementations.
    shape: BodyShape,
}

/// Per-method call sequence after detector-level signal filtering.
#[derive(Debug)]
struct ScoredCallSequence {
    file: String,
    method: String,
    /// Ordered signal calls used for LCS scoring.
    signal_calls: Vec<String>,
    /// Unique signal calls used for Jaccard scoring and cheap prefilters.
    signal_set: HashSet<String>,
    shape: BodyShape,
}

/// Generic looping markers — substrings that indicate the body iterates over
/// something. Covers control-flow keywords (`for`, `while`, `loop`,
/// `foreach`) shared by most languages and common iterator-pipeline calls
/// from Rust, JS, Python, PHP, and Go. Match is whitespace/`(`-bounded so
/// substrings like `format!` (containing `for`) do not register.
const LOOPING_MARKERS: &[&str] = &[
    "for ",
    "for(",
    "while ",
    "while(",
    "loop {",
    "loop{",
    "foreach ",
    "foreach(",
    ".iter()",
    ".into_iter()",
    ".iter_mut()",
    ".for_each(",
    ".map(",
    ".filter(",
    ".fold(",
    ".flat_map(",
    ".reduce(",
    ".for_each (",
    "forEach(",
    "range(",
];

/// Detect the body shape of a function body purely from text.
///
/// Generic by construction — uses substrings (`for`, `while`, `loop`,
/// `.map(`, `.filter(`, `forEach(`, `range(`, …) that exist in every
/// mainstream language, plus a self-call probe (`<method_name>(`) for
/// recursion. No AST, no language-specific identifiers.
fn detect_body_shape(body: &str, method_name: &str) -> BodyShape {
    let has_loop = LOOPING_MARKERS.iter().any(|marker| body.contains(marker));
    let has_self_call = contains_self_call(body, method_name);

    match (has_loop, has_self_call) {
        // Recursive wins over Looping for reporting purposes only when there
        // is no loop; if both are present we still want to flag it as Looping
        // (loops are the dominant signal). For the compatibility gate the
        // distinction does not matter — Looping and Recursive are mutually
        // compatible.
        (true, _) => BodyShape::Looping,
        (false, true) => BodyShape::Recursive,
        (false, false) => BodyShape::StraightLine,
    }
}

/// Return true if `body` contains a call to `method_name` (i.e. direct
/// recursion). The check is the function name followed by `(`, with the
/// preceding character either absent or a non-identifier byte so that
/// `do_thing` does not match `redo_thing`.
fn contains_self_call(body: &str, method_name: &str) -> bool {
    if method_name.is_empty() {
        return false;
    }
    let bytes = body.as_bytes();
    let needle = method_name.as_bytes();
    if needle.len() >= bytes.len() {
        return false;
    }

    let mut i = 0;
    while i + needle.len() < bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            let after = bytes[i + needle.len()];
            let before_ok = i == 0 || {
                let b = bytes[i - 1];
                !(b.is_ascii_alphanumeric() || b == b'_')
            };
            if after == b'(' && before_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Extract function call names from a code block.
///
/// Matches patterns like `function_name(`, `self.method(`, `Type::method(`.
/// Returns the called name (without receiver/namespace prefix).
///
/// `extra_trivial` is an extension-supplied set of additional trivial call
/// names that augment the built-in `TRIVIAL_CALLS` floor. Core never inspects
/// these strings — they are merged with the generic floor and used opaquely.
fn extract_calls_from_body(body: &str, extra_trivial: &HashSet<&str>) -> Vec<String> {
    let mut calls = Vec::new();

    for line in body.lines() {
        let trimmed = line.trim();
        // Skip comments
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
            continue;
        }

        // Find all `identifier(` patterns
        let chars: Vec<char> = trimmed.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            // Look for `(`
            if chars[i] == '(' && i > 0 {
                // Walk backwards to find the identifier
                let end = i;
                let mut start = i;
                while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_') {
                    start -= 1;
                }
                if start < end {
                    let name: String = chars[start..end].iter().collect();
                    // Skip language keywords, control flow, and trivial stdlib calls
                    if !is_keyword(&name)
                        && !name.is_empty()
                        && !TRIVIAL_CALLS.contains(&name.as_str())
                        && !extra_trivial.contains(name.as_str())
                    {
                        calls.push(name);
                    }
                }
            }
            i += 1;
        }
    }

    calls
}

fn corpus_common_calls(sequences: &[MethodCallSequence]) -> HashSet<String> {
    if sequences.len() < MIN_COMMON_CALL_METHODS {
        return HashSet::new();
    }

    let mut method_counts: HashMap<&str, usize> = HashMap::new();
    for sequence in sequences {
        let unique_calls: HashSet<&str> = sequence.calls.iter().map(|call| call.as_str()).collect();
        for call in unique_calls {
            *method_counts.entry(call).or_insert(0) += 1;
        }
    }

    let ratio_floor = (sequences.len() as f64 * COMMON_CALL_METHOD_RATIO).ceil() as usize;
    let count_floor = MIN_COMMON_CALL_METHODS.max(ratio_floor);

    method_counts
        .into_iter()
        .filter(|&(_call, count)| count >= count_floor)
        .map(|(call, _count)| call.to_string())
        .collect()
}

fn signal_calls(
    calls: &[String],
    extra_plumbing: &HashSet<&str>,
    common_calls: &HashSet<String>,
) -> Vec<String> {
    calls
        .iter()
        .filter(|call| {
            !PLUMBING_CALLS.contains(&call.as_str())
                && !extra_plumbing.contains(call.as_str())
                && !common_calls.contains(call.as_str())
        })
        .cloned()
        .collect()
}

fn scored_call_sequences(
    sequences: Vec<MethodCallSequence>,
    extra_plumbing: &HashSet<&str>,
    common_calls: &HashSet<String>,
) -> Vec<ScoredCallSequence> {
    sequences
        .into_iter()
        .filter_map(|sequence| {
            let signal_calls = signal_calls(&sequence.calls, extra_plumbing, common_calls);
            if signal_calls.len() < MIN_CALL_COUNT {
                return None;
            }

            let signal_set = signal_calls.iter().cloned().collect();
            Some(ScoredCallSequence {
                file: sequence.file,
                method: sequence.method,
                signal_calls,
                signal_set,
                shape: sequence.shape,
            })
        })
        .collect()
}

/// Check if a name is a language keyword (not a function call).
fn is_keyword(name: &str) -> bool {
    matches!(
        name,
        "if" | "else"
            | "for"
            | "while"
            | "loop"
            | "match"
            | "return"
            | "let"
            | "mut"
            | "const"
            | "fn"
            | "pub"
            | "use"
            | "mod"
            | "struct"
            | "enum"
            | "impl"
            | "trait"
            | "type"
            | "where"
            | "self"
            | "Self"
            | "super"
            | "crate"
            | "as"
            | "in"
            | "ref"
            | "Some"
            | "None"
            | "Ok"
            | "Err"
            | "true"
            | "false"
            | "assert"
            | "assert_eq"
            | "assert_ne"
            | "println"
            | "eprintln"
            | "format"
            | "vec"
            | "todo"
            | "unimplemented"
            | "unreachable"
            | "panic"
            | "dbg"
    )
}

/// Extract per-method call sequences from all fingerprints.
///
/// `extra_trivial` is an extension-supplied set of additional call names to
/// treat as trivial during call-sequence extraction. It is merged with the
/// built-in `TRIVIAL_CALLS` floor — this function never interprets the
/// strings, it only filters them out of the recorded sequence.
fn extract_call_sequences(
    fingerprints: &[&FileFingerprint],
    extra_trivial: &HashSet<&str>,
) -> Vec<MethodCallSequence> {
    let mut sequences = Vec::new();

    for fp in fingerprints {
        if fp.content.is_empty() {
            continue;
        }

        // Skip test files entirely — test code is expected to mirror production
        // call patterns and flagging it as "parallel implementation" is noise.
        if super::walker::is_test_path(&fp.relative_path) {
            continue;
        }

        let lines: Vec<&str> = fp.content.lines().collect();

        for method_name in &fp.methods {
            // Skip generic names — they're expected to have similar call patterns
            if GENERIC_NAMES.contains(&method_name.as_str()) {
                continue;
            }

            // Skip test methods (inline #[cfg(test)] modules)
            if method_name.starts_with("test_") {
                continue;
            }

            let Some((body_start, body_end)) = find_method_body(&lines, method_name) else {
                continue;
            };

            if body_start + 1 >= body_end {
                continue;
            }

            let body: String = lines[body_start + 1..body_end].join("\n");
            let calls = extract_calls_from_body(&body, extra_trivial);
            let shape = detect_body_shape(&body, method_name);

            if calls.len() >= MIN_CALL_COUNT {
                sequences.push(MethodCallSequence {
                    file: fp.relative_path.clone(),
                    method: method_name.clone(),
                    calls,
                    shape,
                });
            }
        }
    }

    sequences
}

/// Compute Jaccard similarity between two sets.
fn jaccard_similarity(a: &HashSet<String>, b: &HashSet<String>, intersection: usize) -> f64 {
    let union = a.len() + b.len() - intersection;

    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

fn shared_signal_call_count(a: &HashSet<String>, b: &HashSet<String>) -> usize {
    if a.len() <= b.len() {
        a.iter().filter(|call| b.contains(*call)).count()
    } else {
        b.iter().filter(|call| a.contains(*call)).count()
    }
}

/// Compute longest common subsequence length between two sequences.
fn lcs_length(a: &[String], b: &[String]) -> usize {
    let (longer, shorter) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut previous = vec![0usize; shorter.len() + 1];
    let mut current = vec![0usize; shorter.len() + 1];

    for longer_call in longer {
        for j in 1..=shorter.len() {
            if longer_call == &shorter[j - 1] {
                current[j] = previous[j - 1] + 1;
            } else {
                current[j] = previous[j].max(current[j - 1]);
            }
        }
        std::mem::swap(&mut previous, &mut current);
        current.fill(0);
    }

    previous[shorter.len()]
}

/// Compute LCS ratio: 2 * LCS / (len(a) + len(b)).
fn lcs_ratio(a: &[String], b: &[String]) -> f64 {
    let total = a.len() + b.len();
    if total == 0 {
        return 0.0;
    }
    2.0 * lcs_length(a, b) as f64 / total as f64
}

/// Detect parallel implementations across files.
///
/// Compares all method pairs (in different files) by their call sequences.
/// When two methods make a similar set of calls in a similar order — but
/// have different names and different exact implementations — they're
/// likely parallel implementations of the same workflow that should be
/// abstracted into a shared parameterized function.
///
/// Filters out:
/// - Methods in the same file
/// - Generic names (run, new, build, etc.)
/// - Methods with fewer than MIN_CALL_COUNT calls
/// - Pairs already caught by exact or near-duplicate detection
/// - Pairs below both similarity thresholds
///
/// Detect parallel implementations — methods with similar call patterns across files.
///
/// `convention_methods` contains method names that are expected by discovered conventions.
/// When both methods in a pair are convention-expected, the pair is skipped — similar call
/// patterns are the expected behavior for convention-following code, not a finding.
///
/// `detector_config` carries extension-supplied trivial/plumbing call name lists
/// that augment the built-in generic floors. Core never interprets these strings;
/// they are merged into the existing filters.
pub(crate) fn detect_parallel_implementations(
    fingerprints: &[&FileFingerprint],
    convention_methods: &std::collections::HashSet<String>,
    detector_config: &DuplicationDetectorConfig,
) -> Vec<Finding> {
    let extra_trivial: HashSet<&str> = detector_config
        .trivial_calls
        .iter()
        .map(|s| s.as_str())
        .collect();
    let extra_plumbing: HashSet<&str> = detector_config
        .plumbing_calls
        .iter()
        .map(|s| s.as_str())
        .collect();

    let sequences = extract_call_sequences(fingerprints, &extra_trivial);
    let common_calls = corpus_common_calls(&sequences);
    let sequences = scored_call_sequences(sequences, &extra_plumbing, &common_calls);

    // Build sets of already-flagged pairs (exact + near duplicates) to avoid double-flagging
    let exact_groups = build_groups(fingerprints);
    let exact_dup_fns: std::collections::HashSet<String> = exact_groups
        .iter()
        .filter(|(_, locs)| locs.len() >= MIN_DUPLICATE_LOCATIONS)
        .map(|((name, _), _)| name.clone())
        .collect();

    let mut findings = Vec::new();
    for i in 0..sequences.len() {
        for j in (i + 1)..sequences.len() {
            let a = &sequences[i];
            let b = &sequences[j];

            // Skip same file
            if a.file == b.file {
                continue;
            }

            // Skip if same function name (already caught by other detectors)
            if a.method == b.method {
                continue;
            }

            // Skip if either function is an exact duplicate
            if exact_dup_fns.contains(&a.method) || exact_dup_fns.contains(&b.method) {
                continue;
            }

            // Skip if either method is convention-expected — its call pattern is shaped
            // by the convention, so similar patterns with other methods are expected.
            if convention_methods.contains(&a.method) || convention_methods.contains(&b.method) {
                continue;
            }

            // Body-shape gate (issue #2334): a parallel-implementation finding
            // must reflect a shared workflow, not just a shared call set. Two
            // bodies with incompatible shapes (e.g. a single-file copy helper
            // vs a recursive directory walk) are not the same workflow even
            // when they share `fs::copy` and `create_dir_all`.
            if !a.shape.compatible_with(b.shape) {
                continue;
            }

            let shared_count = shared_signal_call_count(&a.signal_set, &b.signal_set);
            // This is a conservative prefilter: the old path discarded these
            // pairs after Jaccard/LCS, so skipping LCS here preserves findings.
            if shared_count < MIN_SHARED_CALLS {
                continue;
            }

            // For two StraightLine bodies the shared call set is the only
            // signal we have, so raise the Jaccard floor — a small focused
            // helper that overlaps with another small helper on a couple of
            // stdlib calls is too weak to flag.
            let jaccard_floor = if matches!(
                (a.shape, b.shape),
                (BodyShape::StraightLine, BodyShape::StraightLine)
            ) {
                STRAIGHT_LINE_JACCARD_FLOOR
            } else {
                MIN_JACCARD_SIMILARITY
            };

            let jaccard = jaccard_similarity(&a.signal_set, &b.signal_set, shared_count);
            if jaccard < jaccard_floor {
                continue;
            }

            let lcs = lcs_ratio(&a.signal_calls, &b.signal_calls);
            if lcs < MIN_LCS_RATIO {
                continue;
            }

            let mut shared: Vec<&str> = a
                .signal_set
                .intersection(&b.signal_set)
                .map(|call| call.as_str())
                .collect();
            shared.sort_unstable();
            let shared_preview: String = shared
                .iter()
                .take(5)
                .map(|s| format!("`{}`", s))
                .collect::<Vec<_>>()
                .join(", ");
            let extra = if shared.len() > 5 {
                format!(" (+{} more)", shared.len() - 5)
            } else {
                String::new()
            };

            let suggestion = format!(
                "`{}` and `{}` follow the same call pattern (Jaccard: {:.0}%, sequence: {:.0}%). \
                 Consider extracting the shared workflow into a parameterized function.",
                a.method,
                b.method,
                jaccard * 100.0,
                lcs * 100.0
            );

            // Emit finding for file A
            findings.push(Finding {
                convention: "parallel-implementation".to_string(),
                severity: Severity::Info,
                file: a.file.clone(),
                description: format!(
                    "Parallel implementation: `{}` has similar call pattern to `{}` in {} — shared calls: {}{}",
                    a.method, b.method, b.file, shared_preview, extra
                ),
                suggestion: suggestion.clone(),
                kind: AuditFinding::ParallelImplementation,
            });

            // Emit finding for file B
            findings.push(Finding {
                convention: "parallel-implementation".to_string(),
                severity: Severity::Info,
                file: b.file.clone(),
                description: format!(
                    "Parallel implementation: `{}` has similar call pattern to `{}` in {} — shared calls: {}{}",
                    b.method, a.method, a.file, shared_preview, extra
                ),
                suggestion,
                kind: AuditFinding::ParallelImplementation,
            });
        }
    }

    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.description.cmp(&b.description))
    });
    findings
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests;
