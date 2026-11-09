//! Bounded error cause chains read from a bench runtime output envelope.
//!
//! Contract: Homeboy exports `$HOMEBOY_BENCH_OUTPUT_ENVELOPE_FILE` to every
//! bench runner. An extension whose runtime produces a structured output
//! envelope writes it there (verbatim JSON). When the bench fails, Homeboy
//! reads the envelope's typed `error` object and its nested `cause` chain into
//! a bounded [`BenchErrorChain`] and keeps the envelope as a run artifact.
//!
//! The envelope shape is product-agnostic: any JSON object with an `error`
//! value. `error` may be a string or an object with `message`, optional
//! `name` / `code` / `stack`, and an optional nested `cause` of the same shape.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use homeboy_core::engine::run_dir::RunDir;

/// Env var naming the file a bench runner writes its runtime output envelope to.
pub const OUTPUT_ENVELOPE_ENV: &str = "HOMEBOY_BENCH_OUTPUT_ENVELOPE_FILE";

/// Run-artifact kind used when the envelope is recorded in the observation store.
pub const OUTPUT_ENVELOPE_ARTIFACT_KIND: &str = "bench_output_envelope";

const OUTPUT_ENVELOPE_FILE: &str = "bench-output-envelope.json";
const MAX_CHAIN_BYTES: usize = 4096;
const MAX_MESSAGE_BYTES: usize = 1024;
const MAX_CAUSE_MESSAGE_BYTES: usize = 512;
const MAX_STACK_BYTES: usize = 512;
const MAX_STACK_LINES: usize = 4;
const MAX_CAUSES: usize = 8;

/// Typed error chain reported by a bench runtime, bounded to about 4 KB.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BenchErrorChain {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_head: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causes: Vec<BenchErrorCause>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// One nested cause of a [`BenchErrorChain`], outermost first.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BenchErrorCause {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_head: Option<String>,
}

/// Envelope path for a single runner, or for instance `id` of a concurrent run.
pub fn output_envelope_path(run_dir: &RunDir, instance: Option<u32>) -> PathBuf {
    match instance {
        None => run_dir.step_file(OUTPUT_ENVELOPE_FILE),
        Some(id) => run_dir.step_file(&format!("bench-output-envelope-i{}.json", id)),
    }
}

/// Whether `path` names a single or per-instance output envelope.
pub fn is_output_envelope_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name == OUTPUT_ENVELOPE_FILE
                || (name.starts_with("bench-output-envelope-i") && name.ends_with(".json"))
        })
}

/// Every envelope path a bench run may have written, single runner first.
pub fn output_envelope_paths(run_dir: &RunDir, concurrency: u32) -> Vec<PathBuf> {
    let mut paths = vec![output_envelope_path(run_dir, None)];
    if concurrency > 1 {
        paths.extend((0..concurrency).map(|id| output_envelope_path(run_dir, Some(id))));
    }
    paths
}

/// Remove envelopes left by a previous run in the same run dir.
pub(crate) fn clear_output_envelopes(run_dir: &RunDir, concurrency: u32) {
    for path in output_envelope_paths(run_dir, concurrency) {
        let _ = std::fs::remove_file(path);
    }
}

/// First error chain found in the run's envelopes, if any.
pub(crate) fn read_run_error_chain(run_dir: &RunDir, concurrency: u32) -> Option<BenchErrorChain> {
    output_envelope_paths(run_dir, concurrency)
        .iter()
        .find_map(|path| read_error_chain(path))
}

/// Read a bounded error chain from the envelope at `path`.
pub fn read_error_chain(path: &Path) -> Option<BenchErrorChain> {
    let raw = std::fs::read_to_string(path).ok()?;
    let envelope: Value = serde_json::from_str(&raw).ok()?;
    error_chain_from_envelope(&envelope)
}

/// Build a bounded error chain from an envelope's `error` value.
pub fn error_chain_from_envelope(envelope: &Value) -> Option<BenchErrorChain> {
    let mut nodes = Vec::new();
    let mut current = envelope.get("error");
    while let Some(node) = current {
        if nodes.len() > MAX_CAUSES {
            break;
        }
        match node {
            Value::String(message) => {
                nodes.push(ErrorNode::message(message));
                current = None;
            }
            Value::Object(map) => {
                nodes.push(ErrorNode {
                    message: text(map.get("message")).unwrap_or_default(),
                    name: text(map.get("name")),
                    code: text(map.get("code")),
                    stack: text(map.get("stack")),
                });
                current = map.get("cause");
            }
            _ => current = None,
        }
    }
    let truncated_depth = current.is_some_and(|node| !node.is_null());

    let mut nodes = nodes.into_iter();
    let top = nodes.next()?;
    let mut chain = BenchErrorChain {
        message: bounded(&top.message, MAX_MESSAGE_BYTES),
        name: top.name.map(|name| bounded(&name, 128)),
        code: top.code.map(|code| bounded(&code, 128)),
        stack_head: top.stack.as_deref().map(stack_head),
        causes: nodes
            .map(|node| BenchErrorCause {
                message: bounded(&node.message, MAX_CAUSE_MESSAGE_BYTES),
                name: node.name.map(|name| bounded(&name, 128)),
                code: node.code.map(|code| bounded(&code, 128)),
                stack_head: node.stack.as_deref().map(stack_head),
            })
            .collect(),
        truncated: truncated_depth,
    };
    enforce_total_bound(&mut chain);
    Some(chain)
}

struct ErrorNode {
    message: String,
    name: Option<String>,
    code: Option<String>,
    stack: Option<String>,
}

impl ErrorNode {
    fn message(message: &str) -> Self {
        Self {
            message: message.to_string(),
            name: None,
            code: None,
            stack: None,
        }
    }
}

fn text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn stack_head(stack: &str) -> String {
    let head = stack
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(MAX_STACK_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    bounded(&head, MAX_STACK_BYTES)
}

/// Drop stacks (innermost causes keep theirs longest), then outer causes, until
/// the serialized chain fits [`MAX_CHAIN_BYTES`]. The innermost cause is the
/// root cause, so it is kept last.
fn enforce_total_bound(chain: &mut BenchErrorChain) {
    let fits = |chain: &BenchErrorChain| {
        serde_json::to_vec(chain).map_or(true, |bytes| bytes.len() <= MAX_CHAIN_BYTES)
    };
    if fits(chain) {
        return;
    }
    chain.truncated = true;
    if chain.stack_head.take().is_some() && fits(chain) {
        return;
    }
    for index in 0..chain.causes.len() {
        if chain.causes[index].stack_head.take().is_some() && fits(chain) {
            return;
        }
    }
    while chain.causes.len() > 1 && !fits(chain) {
        chain.causes.remove(0);
    }
}

fn bounded(text: &str, max_bytes: usize) -> String {
    let text = text.trim();
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes.saturating_sub(3);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested_cause_chain_with_stack_head() {
        let envelope = serde_json::json!({
            "success": false,
            "error": {
                "name": "RecipeError",
                "code": "recipe-runtime-create-failed",
                "message": "Runtime creation failed before recipe workflow execution.",
                "cause": {
                    "name": "SyntaxError",
                    "message": "Unexpected identifier 'source'",
                    "stack": "SyntaxError: Unexpected identifier 'source'\n    at a (x.js:1)\n    at b (x.js:2)\n    at c (x.js:3)\n    at d (x.js:4)\n    at e (x.js:5)"
                }
            }
        });

        let chain = error_chain_from_envelope(&envelope).expect("chain");

        assert_eq!(
            chain.message,
            "Runtime creation failed before recipe workflow execution."
        );
        assert_eq!(chain.code.as_deref(), Some("recipe-runtime-create-failed"));
        assert_eq!(chain.causes.len(), 1);
        assert_eq!(chain.causes[0].name.as_deref(), Some("SyntaxError"));
        let stack = chain.causes[0].stack_head.as_deref().expect("stack head");
        assert_eq!(stack.lines().count(), MAX_STACK_LINES);
        assert!(!chain.truncated);
    }

    #[test]
    fn bounds_large_chains_and_keeps_root_cause() {
        let mut error = serde_json::json!({ "message": "root cause", "stack": "x".repeat(4000) });
        for depth in 0..6 {
            error = serde_json::json!({
                "message": format!("layer {} {}", depth, "m".repeat(2000)),
                "stack": "s".repeat(4000),
                "cause": error,
            });
        }

        let chain =
            error_chain_from_envelope(&serde_json::json!({ "error": error })).expect("chain");

        assert!(serde_json::to_vec(&chain).unwrap().len() <= MAX_CHAIN_BYTES);
        assert!(chain.truncated);
        assert_eq!(chain.causes.last().unwrap().message, "root cause");
    }

    #[test]
    fn envelope_without_error_has_no_chain() {
        assert!(error_chain_from_envelope(&serde_json::json!({ "success": true })).is_none());
        assert!(
            error_chain_from_envelope(&serde_json::json!({ "error": "plain failure" }))
                .is_some_and(|chain| chain.message == "plain failure")
        );
    }
}
