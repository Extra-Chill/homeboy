//! Command-wrapper-bypass detection.
//!
//! Finds raw subprocess calls whose literal argument vector is byte-identical
//! to the arg-vector wrapped by an existing thin helper — e.g. hand-rolling
//! `git_output(path, &["rev-parse", "HEAD"])` when `head_sha(path)` already
//! exists and is literally `output_optional(git_root, &["rev-parse", "HEAD"])`.
//!
//! This is the command-invocation analog of the constant-bypass detector: a
//! canonical wrapper exists, but callers re-invoke the primitive raw, so the
//! command spelling (flags, arg order) drifts and cannot be fixed in one place.
//!
//! Auto-discovery, no config: a "thin wrapper" is a function whose body is a
//! single call passing a literal all-string arg-vector. The detector records
//! `argvec -> (helper_name, file)` from those, then flags any other site that
//! passes the same literal arg-vector.
//!
//! It reads only `FileFingerprint::content`, so no fingerprint-model change is
//! required. Arg-vector recognition uses the C-family `&["a", "b"]` slice
//! literal; a follow-up can source the array-literal shape from grammar config
//! for non-Rust languages.

use std::collections::HashMap;

use regex::Regex;

use super::super::conventions::AuditFinding;
use super::super::findings::{Finding, Severity};
use super::super::fingerprint::FileFingerprint;
use super::super::walker::{
    cfg_test_regions, crate_of_path, is_test_path, offset_in_cfg_test_region,
    visibility_is_crate_public,
};

/// Minimum arg-vector length to consider. Single-element commands (`["init"]`,
/// `["fetch"]`) are too generic to attribute to one wrapper.
const MIN_ARGV_LEN: usize = 2;

pub(crate) fn run(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    detect_command_wrapper_bypass(fingerprints)
}

/// A literal all-string slice: `&["a", "b", ...]`. Group 1 is the inner list.
fn argvec_regex() -> &'static Regex {
    static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r#"&\[\s*("(?:[^"\\]|\\.)*"\s*(?:,\s*"(?:[^"\\]|\\.)*"\s*)*),?\s*\]"#)
            .expect("valid arg-vector regex")
    });
    &RE
}

/// A **module-level** `fn NAME` declaration. Anchored to the start of a line
/// (optionally through `pub`/`pub(...)`), so nested functions — test-module
/// helpers inside `#[cfg(test)] mod tests { fn head() … }`, impl methods — are
/// excluded. Canonical command wrappers are module-level; indented one-call
/// helpers are almost always test fixtures and would otherwise seed the map
/// with git-setup arg-vectors. Group 1 is the visibility prefix (`pub` /
/// `pub(...)` / empty); group 2 is the name.
fn fn_decl_regex() -> &'static Regex {
    static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?m)^(pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+([a-z_][a-z0-9_]*)\s*[(<]")
            .expect("valid fn regex")
    });
    &RE
}

/// Parse the inner string elements of an arg-vector match into a canonical key.
fn argvec_elements(inner: &str) -> Vec<String> {
    static ELEM: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r#""((?:[^"\\]|\\.)*)""#).unwrap());
    ELEM.captures_iter(inner)
        .map(|c| c[1].to_string())
        .collect()
}

/// Name tokens that mark a call as a subprocess/exec sink. A wrapper body whose
/// call contains one of these is running a command; one that calls e.g.
/// `array_len` / `discover_named_files` is a data accessor and its arg-vector is
/// a path/key list, not a command. Kept generic (not git-specific) so any
/// exec-wrapping helper qualifies.
const EXEC_SINK_TOKENS: &[&str] = &[
    "git", "run", "output", "command", "cmd", "exec", "spawn", "stdout", "capture", "shell",
    "process", "invoke",
];

/// Whether a wrapper body passes its arg-vector to an exec sink.
fn wraps_exec_sink(body: &str) -> bool {
    static CALL_RE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"([a-z_][a-z0-9_]*)\s*\(").unwrap());
    CALL_RE.captures_iter(body).any(|c| {
        let call = &c[1];
        EXEC_SINK_TOKENS.iter().any(|tok| {
            call == *tok
                || call.starts_with(&format!("{tok}_"))
                || call.ends_with(&format!("_{tok}"))
                || call.contains(tok)
        })
    })
}

struct WrapperDef {
    name: String,
    file: String,
    /// Byte offset of the arg-vector literal in the wrapper body, so we never
    /// flag the wrapper's own definition.
    argv_offset: usize,
    /// Whether the wrapper is crate-`pub` — required to attribute a bypass from
    /// a different crate. A private/`pub(crate)` helper is unreachable
    /// cross-crate, so flagging it there is a false positive.
    is_public: bool,
}

fn detect_command_wrapper_bypass(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    // argvec-key -> canonical wrapper.
    let mut wrappers: HashMap<Vec<String>, WrapperDef> = HashMap::new();

    for fp in fingerprints {
        if is_test_path(&fp.relative_path) {
            continue;
        }
        for (name, is_public, body, body_offset) in thin_wrapper_bodies(&fp.content) {
            // A thin wrapper's body contains exactly one arg-vector literal.
            let matches: Vec<_> = argvec_regex().find_iter(&body).collect();
            if matches.len() != 1 {
                continue;
            }
            // The arg-vector must be passed to a subprocess/exec sink, not a
            // data accessor. `array_len(v, &["aggregate","outcomes"])` reads a
            // JSON path — that literal is not a command. Recognize exec sinks by
            // name token so a JSON-path or config-key arg-vector never seeds the
            // command-wrapper map.
            if !wraps_exec_sink(&body) {
                continue;
            }
            let m = matches[0];
            let elems = argvec_elements(
                argvec_regex()
                    .captures(&body[m.start()..m.end()])
                    .and_then(|c| c.get(1))
                    .map(|g| g.as_str())
                    .unwrap_or(""),
            );
            if elems.len() < MIN_ARGV_LEN {
                continue;
            }
            wrappers.entry(elems).or_insert(WrapperDef {
                name: name.clone(),
                file: fp.relative_path.clone(),
                argv_offset: body_offset + m.start(),
                is_public,
            });
        }
    }

    if wrappers.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for fp in fingerprints {
        if is_test_path(&fp.relative_path) {
            continue;
        }
        // Raw arg-vectors inside inline `#[cfg(test)] mod tests { … }` blocks of
        // a production file are test fixtures (e.g. `git rev-parse HEAD` to read
        // a freshly-built repo), not production command drift. `is_test_path`
        // only excludes whole test files, so compute the file's cfg(test) byte
        // ranges here and skip matches that fall inside them.
        let test_regions = cfg_test_regions(&fp.content);
        for m in argvec_regex().find_iter(&fp.content) {
            if offset_in_cfg_test_region(m.start(), &test_regions) {
                continue;
            }
            let inner = argvec_regex()
                .captures(&fp.content[m.start()..m.end()])
                .and_then(|c| c.get(1))
                .map(|g| g.as_str())
                .unwrap_or("");
            let elems = argvec_elements(inner);
            let Some(def) = wrappers.get(&elems) else {
                continue;
            };
            // Never flag the wrapper's own arg-vector.
            if fp.relative_path == def.file && m.start() == def.argv_offset {
                continue;
            }
            // Only attribute a bypass to a helper the call site can actually
            // reach. A private/`pub(crate)` helper in a DIFFERENT crate is
            // unreachable, so suggesting "call it instead" is a false positive.
            // Same-crate calls are always fine; cross-crate requires `pub`.
            let call_crate = crate_of_path(&fp.relative_path);
            let def_crate = crate_of_path(&def.file);
            let cross_crate = match (call_crate, def_crate) {
                (Some(a), Some(b)) => a != b,
                // Unknown layout on either side — be conservative and treat as
                // same-crate (do not suppress) to preserve prior behavior.
                _ => false,
            };
            if cross_crate && !def.is_public {
                continue;
            }
            let cmd = elems.join(" ");
            findings.push(Finding {
                convention: "command_wrapper_bypass".to_string(),
                severity: Severity::Warning,
                file: fp.relative_path.clone(),
                description: format!(
                    "Raw command `{}` duplicates helper `{}` (defined in {})",
                    cmd, def.name, def.file
                ),
                suggestion: format!(
                    "Call `{}` instead of assembling the raw argument vector; \
                     the helper is the one place the command spelling is defined.",
                    def.name
                ),
                kind: AuditFinding::CommandWrapperBypass,
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

/// Yield `(fn_name, is_public, body_text, body_start_offset)` for functions
/// whose body is small enough to be a thin wrapper (few statements). Uses brace
/// matching from each `fn` declaration; bounded to short bodies so we only
/// consider genuine one-call wrappers, not large functions that happen to
/// contain one arg-vector. `is_public` is true only for crate-`pub` helpers.
fn thin_wrapper_bodies(content: &str) -> Vec<(String, bool, String, usize)> {
    /// Max characters in a wrapper body — a one-call delegation is tiny; this
    /// keeps us from treating a large function's incidental arg-vector as the
    /// canonical definition.
    const MAX_BODY_CHARS: usize = 160;

    let bytes = content.as_bytes();
    let mut out = Vec::new();
    for caps in fn_decl_regex().captures_iter(content) {
        let is_public = caps
            .get(1)
            .map(|m| visibility_is_crate_public(m.as_str()))
            .unwrap_or(false);
        let name = caps[2].to_string();
        let decl_end = caps.get(0).unwrap().end();
        // Find the opening brace of the body after the signature.
        let Some(brace_rel) = content[decl_end..].find('{') else {
            continue;
        };
        let open = decl_end + brace_rel;
        // Match to the closing brace.
        let mut depth = 0i32;
        let mut close = None;
        for (i, &b) in bytes[open..].iter().enumerate() {
            if b == b'{' {
                depth += 1;
            } else if b == b'}' {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
        }
        let Some(close) = close else { continue };
        let body_start = open + 1;
        if close <= body_start {
            continue;
        }
        let body = &content[body_start..close];
        if body.len() > MAX_BODY_CHARS {
            continue;
        }
        out.push((name, is_public, body.to_string(), body_start));
    }
    out
}

#[cfg(test)]
mod tests;
