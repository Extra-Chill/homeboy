//! Pins the absence of the mixed-store function shape in `homeboy-agents` (#7505).
//!
//! The shape this test forbids is a function that accepts one durable store as
//! an explicit parameter and then manufactures a *different* one from
//! process-global state inside its own body:
//!
//! ```text
//! fn something_with_store(recipe_store: &CookRecipeStore, ...) -> Result<...> {
//!     let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
//!     something_with_stores((recipe_store, &lifecycle_store), ...)
//! }
//! ```
//!
//! A caller that believes it injected explicit roots gets its state split
//! silently across two homes: the recipe lands where it asked, the lifecycle
//! records land wherever the environment happens to point. #7505 removed these
//! one call path at a time, which is exactly the kind of decision that erodes
//! back in as soon as the next `_with_store` wrapper looks convenient. Asserting
//! the absence is what stops it, in the manner of
//! `tests/audit_debt_workflow_test.rs`.
//!
//! Two shapes stay legal and are not flagged:
//!
//! * an env-resolving entry point that takes no store at all and resolves
//!   everything it needs in one place (`run_cook_with_boundaries_observed_inner`,
//!   `claim_pre_artifact_interruption_retry`). Nothing was injected, so nothing
//!   is split.
//! * a function that takes a store and delegates to an ambient free function
//!   only under a `matches_current_environment()` equivalence guard
//!   (`cook_promotion::attempt_needs_execution_with_store` and
//!   `retryable_provider_discovery_failure_with_store`). Those bodies name no
//!   store constructor at all, so the scanner never sees a second root.
//!
//! The source walk follows the convention in `tests/release_workflow_test.rs`.

use std::fs;
use std::path::{Path, PathBuf};

/// A durable store that `homeboy-agents` roots explicitly.
struct Store {
    /// The type name as it reads in a parameter position, with or without a
    /// module qualifier: `&CookRecipeStore`, `&super::cook_recipe::CookRecipeStore`,
    /// `(&CookRecipeStore, &AgentTaskLifecycleStore)`. Matching the bare name
    /// covers every spelling. It is safe against matching a *return* type
    /// because no function in this crate returns either store type — both are
    /// produced only as `Result<Self>` from their own constructors.
    type_name: &'static str,
    /// The constructor that reads process-global state instead of a passed root.
    env_constructor: &'static str,
}

const STORES: &[Store] = &[
    Store {
        type_name: "CookRecipeStore",
        env_constructor: "CookRecipeStore::from_current_data_root()",
    },
    Store {
        type_name: "AgentTaskLifecycleStore",
        env_constructor: "AgentTaskLifecycleStore::from_current_environment()",
    },
];

/// Mixed-store functions that #7505 has not reached yet.
///
/// Every entry is debt, not a false positive: each one really does have the
/// shape this test forbids. Each is listed because closing it is its own slice,
/// with its own call-chain to re-root, not because the scanner is wrong.
///
/// Entries may only be removed. A stale entry — one naming a function that no
/// longer has the shape, or no longer exists — fails this test, so the list
/// cannot outlive the debt it records. Adding a row is the edit this test exists
/// to make someone argue for in review.
const KNOWN_MIXED_STORE_FUNCTIONS: &[(&str, &str)] = &[
    // Takes `&CookRecipeStore` and resolves the lifecycle store through a
    // `match` so the *resolution failure* still lands in
    // `durable_cook_error_report_with_store`, where the spine's own failure used
    // to land. Its only caller, `run_cook_with_boundaries_observed_policy_with_store`,
    // holds no lifecycle store, so closing this means threading one through the
    // entire `run_cook_with_boundaries_observed*` wrapper chain.
    (
        "crates/homeboy-agents/src/agent_task_service/cook.rs",
        "run_cook_with_boundaries_reported",
    ),
    // Takes `&super::cook_recipe::CookRecipeStore` and resolves the lifecycle
    // store with `?`. Three live callers, two of them in `cook_adoption.rs`, so
    // this is a cook_promotion/cook_adoption slice.
    (
        "crates/homeboy-agents/src/agent_task_service/cook_promotion.rs",
        "finalize_or_load_cook_pr_with_backend_with_store",
    ),
];

#[test]
fn homeboy_agents_never_pairs_an_injected_store_with_an_ambient_one() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sources_root = root.join("crates/homeboy-agents/src");
    let mut sources = Vec::new();
    collect_rust_sources(&sources_root, &mut sources);
    assert!(
        !sources.is_empty(),
        "no Rust sources found under {}; this scanner has lost its target and \
         would pass vacuously",
        sources_root.display()
    );

    let mut findings = Vec::new();
    for path in &sources {
        let source = fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let relative = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        findings.extend(scan_source(&relative, &source));
    }

    let unexpected: Vec<&Finding> = findings
        .iter()
        .filter(|finding| {
            !KNOWN_MIXED_STORE_FUNCTIONS
                .contains(&(finding.file.as_str(), finding.function.as_str()))
        })
        .collect();
    assert!(unexpected.is_empty(), "{}", hazard_report(&unexpected));

    let stale: Vec<String> = KNOWN_MIXED_STORE_FUNCTIONS
        .iter()
        .filter(|(file, function)| {
            !findings
                .iter()
                .any(|finding| finding.file == *file && finding.function == *function)
        })
        .map(|(file, function)| format!("  {file}  {function}"))
        .collect();
    assert!(
        stale.is_empty(),
        "KNOWN_MIXED_STORE_FUNCTIONS names functions that no longer mix an \
         injected store with an ambient one:\n{}\n\nThat is the good outcome — \
         delete these rows. The list is only allowed to shrink, and a row that \
         suppresses nothing would silently re-permit the shape it names.",
        stale.join("\n")
    );
}

struct Finding {
    file: String,
    line: usize,
    function: String,
    injected: Vec<&'static str>,
    ambient: Vec<&'static str>,
}

fn hazard_report(findings: &[&Finding]) -> String {
    let mut report = String::from(
        "homeboy-agents has a function that mixes an injected store with an \
         ambient one:\n\n",
    );
    for finding in findings {
        let file = &finding.file;
        let line = finding.line;
        let function = &finding.function;
        let injected = finding.injected.join(", ");
        let ambient = finding.ambient.join(", ");
        report.push_str(&format!(
            "  {file}:{line}  {function}\n\
             \x20   injected by the caller: {injected}\n\
             \x20   built from the process environment in its own body: {ambient}\n",
        ));
    }
    report.push_str(
        "\nA function that accepts one store as a parameter and constructs a \
         different one from process-global state splits its caller's durable \
         state across two roots. The caller believes it injected explicit roots, \
         but half of its writes land wherever the environment happens to point, \
         with no error and no evidence (#7505).\n\n\
         Take both stores as parameters and let the caller pair them, or take \
         neither and resolve both in a single env-resolving entry point.\n",
    );
    report
}

fn scan_source(file: &str, source: &str) -> Vec<Finding> {
    let lines: Vec<&str> = source.lines().collect();
    let mut findings = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let Some(function) = function_name(trimmed) else {
            continue;
        };
        let indent = line.len() - trimmed.len();
        let Some((signature, body)) = function_regions(&lines, index, indent) else {
            continue;
        };

        let injected: Vec<&'static str> = STORES
            .iter()
            .filter(|store| signature.contains(store.type_name))
            .map(|store| store.type_name)
            .collect();
        if injected.is_empty() {
            continue;
        }
        // Only a store the function was *not* handed is a split root. Resolving
        // the same kind it already accepts is the guarded-equivalence shape, and
        // resolving everything while accepting nothing is an entry point.
        let ambient: Vec<&'static str> = STORES
            .iter()
            .filter(|store| !injected.contains(&store.type_name))
            .filter(|store| body.contains(store.env_constructor))
            .map(|store| store.type_name)
            .collect();
        if ambient.is_empty() {
            continue;
        }

        findings.push(Finding {
            file: file.to_string(),
            line: index + 1,
            function: function.to_string(),
            injected,
            ambient,
        });
    }
    findings
}

/// The name of the function declared on a line, if the line declares one.
fn function_name(trimmed: &str) -> Option<&str> {
    let mut rest = trimmed;
    if let Some(after) = rest.strip_prefix("pub(") {
        rest = after[after.find(')')? + 1..].trim_start();
    }
    for prefix in [
        "pub ",
        "default ",
        "const ",
        "async ",
        "unsafe ",
        "extern \"C\" ",
    ] {
        if let Some(after) = rest.strip_prefix(prefix) {
            rest = after.trim_start();
        }
    }
    let name = rest
        .strip_prefix("fn ")?
        .trim_start()
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .next()?;
    (!name.is_empty()).then_some(name)
}

/// Split a function into its signature text and its body text.
///
/// rustfmt is what makes this reliable without a Rust parser: the line that
/// opens a body is the first one ending in `{`, and the body's closing brace
/// sits alone at exactly the `fn` keyword's own indentation. Anything that does
/// not match that shape — a one-line body, a trait method declared without one —
/// is skipped rather than guessed at, so a miss is a missed finding and never a
/// misattributed one.
fn function_regions(lines: &[&str], start: usize, indent: usize) -> Option<(String, String)> {
    if lines[start].trim_end().ends_with('}') {
        return None;
    }

    let mut signature_end = None;
    for (offset, line) in lines[start..].iter().enumerate() {
        let line = line.trim_end();
        if line.ends_with('{') {
            signature_end = Some(start + offset);
            break;
        }
        if line.ends_with(';') {
            // Declared without a body: a trait method, or a `fn` pointer type.
            return None;
        }
    }
    let signature_end = signature_end?;

    let closing = format!("{}}}", " ".repeat(indent));
    let body_start = signature_end + 1;
    let body_end = body_start
        + lines[body_start..]
            .iter()
            .position(|line| line.trim_end() == closing)?;

    Some((
        lines[start..=signature_end].join("\n"),
        lines[body_start..body_end].join("\n"),
    ))
}

fn collect_rust_sources(directory: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_rust_sources(&path, out);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}
