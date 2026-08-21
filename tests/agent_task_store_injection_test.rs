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
//!
//! # The second assertion: same-kind shadowing
//!
//! The mixed-store check above is deliberately narrow — it only fires when the
//! ambient store is a *different kind* from the injected one. #12595 named the
//! blind spot that leaves, and #12618 proved it with a live bug:
//! `record_aggregate_in_store`, a function an earlier slice had already declared
//! rooted and merged, called the ambient `update_cook_candidate_after_completion`.
//! A completed run committed its aggregate into the injected store and wrote the
//! Cook substantive-candidate pointer into the ambient one. Nothing failed —
//! every record and aggregate read back from the injected store was correct at
//! that point. It surfaced only because a later slice happened to walk that code.
//!
//! `rooted_siblings_never_reach_ambient_store_state` closes that. It asserts the
//! other half of the #7505 convention: an ambient wrapper `foo` may resolve a
//! root, but its `foo_in_store` sibling must *use the store it was handed* and
//! reach no process-global state of its own.

use std::collections::BTreeSet;
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
    // Empty. The former entry, `run_cook_with_boundaries_reported`, is gone: the
    // `run_cook_with_boundaries_observed*` wrapper chain this row asked us to
    // thread a lifecycle store through was collapsed into a single `run_cook`
    // taking `CookContext`, which carries both stores as fields. The mixed
    // resolution had nowhere left to hide once the chain stopped existing.
];

/// Every scanned source as `(repo-relative path, contents)`.
///
/// Both assertions in this file walk the same tree, and both are worthless if
/// the walk finds nothing, so the emptiness guard lives here with the walk.
fn scanned_sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sources_root = root.join("crates/homeboy-agents/src");
    let mut paths = Vec::new();
    collect_rust_sources(&sources_root, &mut paths);
    assert!(
        !paths.is_empty(),
        "no Rust sources found under {}; this scanner has lost its target and \
         would pass vacuously",
        sources_root.display()
    );

    paths
        .into_iter()
        .map(|path| {
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            (relative, source)
        })
        .collect()
}

#[test]
fn homeboy_agents_never_pairs_an_injected_store_with_an_ambient_one() {
    let mut findings = Vec::new();
    for (relative, source) in scanned_sources() {
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

// ---------------------------------------------------------------------------
// Same-kind shadowing: an `*_in_store` sibling must use the store it was handed.
// ---------------------------------------------------------------------------

/// The suffix that, by #7505 convention, promises "this body touches only the
/// store it was handed".
///
/// The suffix is also the scope. Two things stay outside this check because of
/// it, both deliberately:
///
/// * `substantive_candidate_in_aggregate` reaches an ambient artifact-projection
///   path helper and #12618 decided not to fix it: the rooted opener sets
///   `maintain_artifacts = false` while the ambient one first reconciles
///   unfinished publications, so swapping them changes what an existing ambient
///   caller sees. It takes an already-read aggregate, not a store, and its name
///   makes no rooting promise — so it is out of scope structurally rather than
///   by a row in a list that would have to be re-argued every slice. It degrades
///   to "no candidate recorded", never to a cross-home write.
/// * the `provider_registry!` registries — `with_acceptance_verifier` and its
///   siblings — are process-global on purpose. They are configured trust
///   material and subprocess contracts, not lifecycle roots (#12618). They are
///   invisible here because they name no root resolver and have no `_in_store`
///   sibling, so nothing had to be written to exclude them.
const IN_STORE_SUFFIX: &str = "_in_store";

/// One way a body can reach process-global durable state.
struct AmbientReach {
    /// Matched as a whole path token, never as a bare substring: if the literal
    /// begins or ends in an identifier character, that end must not abut another
    /// one. That is what keeps `store::` from matching `lifecycle_store::`, and
    /// `paths::homeboy_data` from matching `paths::homeboy_data_store`.
    needle: &'static str,
    /// What the reach lands in, phrased to complete "reaches ...".
    lands_in: &'static str,
}

/// The reaches that mark a free function as an *ambient wrapper* — the legal
/// half of the convention, which resolves a root and delegates to its
/// `*_in_store` sibling. A body naming one of these has manufactured a root.
const ROOT_RESOLVERS: &[AmbientReach] = &[
    AmbientReach {
        needle: "AgentTaskLifecycleStore::from_current_environment(",
        lands_in: "the lifecycle store the process environment points at",
    },
    AmbientReach {
        needle: "CookRecipeStore::from_current_data_root(",
        lands_in: "the recipe store under the process data root",
    },
    AmbientReach {
        needle: "::from_environment(",
        lands_in: "a store resolved from the process environment",
    },
    AmbientReach {
        needle: "default_store(",
        lands_in: "the ambient default store",
    },
    AmbientReach {
        // `agent_task_lifecycle/mod.rs` does `use lifecycle_store as store;`.
        // Every `store::` free function is a shim that resolves `default_store()`
        // for itself, so naming the path at all is naming the ambient root.
        needle: "store::",
        lands_in: "the ambient root a `store::` free shim resolves for itself",
    },
];

/// The reaches that are not store constructors — so they do not mark a function
/// as an ambient wrapper — but still resolve a lifecycle root behind the
/// caller's back. Scanned together with `ROOT_RESOLVERS`.
///
/// The `paths::` entries are enumerated rather than matched by prefix, because
/// `paths::` is not uniformly a root:
///
/// * `paths::controller_runtimes_store()` and `paths::cargo_targets_store()` are
///   content-addressed executable caches shared across homes, process-global by
///   design and not durable lifecycle state (#12608, #12614).
/// * `paths::homeboy()`/`homeboy_json()` are config, not observed state.
/// * `paths::sanitize_path_segment()`, `paths::local_path_is_contained()` and
///   `paths::expand_tilde_path()` are pure string and path arithmetic.
/// * `paths::runtime_promotion_dir()` and `paths::runner_session_file()` are
///   machine-global runtime coordination, in the same class as the runtime pins.
///
/// Listing the roots explicitly is what keeps those out structurally instead of
/// through an allowlist that would have to be argued about again every slice.
const ADDITIONAL_ROOT_REACHES: &[AmbientReach] = &[
    AmbientReach {
        // Both openers take no roots. `open_initialized` additionally resolves
        // `paths::artifact_root()`, which is how #12618 found
        // `reconcile_terminal_artifact_projection` registering controller-owned
        // bytes under one home while writing a complete projection status into
        // another. `AgentTaskLifecycleStore` has its own opener that binds both
        // the data root and the separately carried artifacts root.
        needle: "ObservationStore::open_readonly(",
        lands_in: "the observation store at the ambient data root",
    },
    AmbientReach {
        needle: "ObservationStore::open_initialized(",
        lands_in: "the observation store at the ambient data root and artifact root",
    },
    AmbientReach {
        needle: "paths::homeboy_data",
        lands_in: "the ambient Homeboy data root",
    },
    AmbientReach {
        needle: "paths::homeboy_data_store",
        lands_in: "a named store below the ambient Homeboy data root",
    },
    AmbientReach {
        needle: "paths::observation_db",
        lands_in: "the ambient observation database",
    },
    AmbientReach {
        needle: "paths::artifact_root",
        lands_in: "the ambient artifact-projection root",
    },
    AmbientReach {
        needle: "paths::controller_scratch_store",
        lands_in: "the ambient controller scratch store",
    },
];

/// `*_in_store` functions that reach ambient store state and are not fixed here.
///
/// This is a *different* list from `KNOWN_MIXED_STORE_FUNCTIONS` on purpose.
/// That one records cross-kind splits, where the fix is to thread a second store
/// through a wrapper chain. This one records same-kind shadowing, where the fix
/// is to use the store already in hand — usually a one-line reroute, which is
/// why every row here needs a reason that is not "not done yet".
///
/// Entries may only be removed. A stale entry — one naming a function that no
/// longer reaches ambient state, or no longer exists — fails this test.
const KNOWN_AMBIENT_REACHING_ROOTED_FUNCTIONS: &[(&str, &str)] = &[
    // Takes `Option<&AgentTaskLifecycleStore>` and matches on it twice: the
    // `Some` arm reads through the injected store, the `None` arm falls back to
    // `exact_record` and `store::read_aggregate`. The `None` arm is not
    // shadowing — there is no store in hand to shadow — and #12618's audit
    // confirmed it is not the hazard class. It is listed rather than excluded
    // structurally because "the signature is `Option<&Store>`" is a loophole any
    // future function could adopt to opt out of this check; a named row cannot
    // be adopted by accident.
    (
        "crates/homeboy-agents/src/agent_task_lifecycle/lifecycle_ops.rs",
        "substantive_candidate_in_store",
    ),
];

#[test]
fn rooted_siblings_never_reach_ambient_store_state() {
    let sources = scanned_sources();
    let functions: Vec<RootedFunction> = sources
        .iter()
        .flat_map(|(file, source)| functions_in(file, source))
        .collect();

    // A free function is an ambient wrapper when an `*_in_store` sibling exists
    // for it *and* its own body demonstrably resolves a root. Deriving the set
    // from the tree rather than listing it means a new wrapper pair is covered
    // the moment it is written, and a name that merely looks like a wrapper is
    // not assumed to be one.
    let wrappers = ambient_wrappers(&functions);
    assert!(
        !wrappers.is_empty(),
        "no ambient wrapper / `_in_store` sibling pairs found; the indirect half \
         of this check has lost its target and would pass vacuously"
    );

    let findings = shadow_findings(&functions, &wrappers);

    let unexpected: Vec<&ShadowFinding> = findings
        .iter()
        .filter(|finding| {
            !KNOWN_AMBIENT_REACHING_ROOTED_FUNCTIONS
                .contains(&(finding.file.as_str(), finding.function.as_str()))
        })
        .collect();
    assert!(unexpected.is_empty(), "{}", shadow_report(&unexpected));

    let stale: Vec<String> = KNOWN_AMBIENT_REACHING_ROOTED_FUNCTIONS
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
        "KNOWN_AMBIENT_REACHING_ROOTED_FUNCTIONS names functions that no longer \
         reach ambient store state:\n{}\n\nThat is the good outcome — delete \
         these rows. The list is only allowed to shrink, and a row that \
         suppresses nothing would silently re-permit the shape it names.",
        stale.join("\n")
    );
}

/// The free functions that are the ambient half of a wrapper pair.
///
/// A name qualifies when an `*_in_store` sibling exists for it *and* its own
/// body demonstrably resolves a root. Deriving the set from the tree rather than
/// listing it means a new pair is covered the moment it is written, and a name
/// that merely looks like a wrapper is never assumed to be one.
///
/// Only the direct, one-hop form is recognised. A wrapper that resolves its root
/// two hops away escapes, which is a missed finding and never a misattributed
/// one — the same trade `function_regions` already makes.
fn ambient_wrappers(functions: &[RootedFunction]) -> BTreeSet<&str> {
    let rooted_bases: BTreeSet<&str> = functions
        .iter()
        .filter_map(|function| function.name.strip_suffix(IN_STORE_SUFFIX))
        .collect();
    functions
        .iter()
        .filter(|function| rooted_bases.contains(function.name.as_str()))
        .filter(|function| {
            ROOT_RESOLVERS
                .iter()
                .any(|reach| contains_token(&function.body, reach.needle))
        })
        .map(|function| function.name.as_str())
        .collect()
}

fn shadow_findings(functions: &[RootedFunction], wrappers: &BTreeSet<&str>) -> Vec<ShadowFinding> {
    let mut findings = Vec::new();
    for function in functions
        .iter()
        .filter(|function| function.name.ends_with(IN_STORE_SUFFIX))
    {
        let mut reaches: Vec<String> = ROOT_RESOLVERS
            .iter()
            .chain(ADDITIONAL_ROOT_REACHES.iter())
            .filter(|reach| contains_token(&function.body, reach.needle))
            .map(|reach| format!("`{}` — reaches {}", reach.needle, reach.lands_in))
            .collect();
        // The indirect form, and the one #12618 caught in already-merged code:
        // calling the ambient half of a wrapper pair. Such a body names no store
        // constructor at all, so every direct needle above walks straight past it.
        reaches.extend(
            wrappers
                .iter()
                .filter(|wrapper| contains_token(&function.body, &format!("{wrapper}(")))
                .map(|wrapper| {
                    format!(
                        "`{wrapper}(` — the ambient half of the \
                         `{wrapper}` / `{wrapper}{IN_STORE_SUFFIX}` pair, which \
                         resolves its own root before delegating"
                    )
                }),
        );
        if reaches.is_empty() {
            continue;
        }
        findings.push(ShadowFinding {
            file: function.file.clone(),
            line: function.line,
            function: function.name.clone(),
            reaches,
        });
    }
    findings
}

struct ShadowFinding {
    file: String,
    line: usize,
    function: String,
    reaches: Vec<String>,
}

fn shadow_report(findings: &[&ShadowFinding]) -> String {
    let mut report = String::from(
        "homeboy-agents has an `_in_store` function that reaches ambient store \
         state instead of using the store it was handed:\n\n",
    );
    for finding in findings {
        let file = &finding.file;
        let line = finding.line;
        let function = &finding.function;
        report.push_str(&format!("  {file}:{line}  {function}\n"));
        for reach in &finding.reaches {
            report.push_str(&format!("\x20   {reach}\n"));
        }
    }
    report.push_str(
        "\nThe `_in_store` suffix is a promise to the caller that this body \
         touches only the store it was passed. A body that breaks that promise \
         splits one logical operation across two homes and cannot fail while \
         doing it: the writes that went to the injected store still read back \
         correct, so every positive assertion the caller can make still passes. \
         That is exactly how record_aggregate_in_store shipped rooted while \
         writing its Cook substantive-candidate pointer into the ambient home \
         (#7505, #12618).\n\n\
         Use the store parameter, or call the `_in_store` sibling of whatever \
         you reached for and pass it along. If the reach is genuinely not a \
         lifecycle root, say so in ADDITIONAL_ROOT_REACHES rather than here.\n",
    );
    report
}

/// A function body paired with where it was found.
struct RootedFunction {
    file: String,
    line: usize,
    name: String,
    /// Body text with `//` line comments stripped. Comments in this crate
    /// explain the very hazards this scans for, and the natural way to write
    /// such a comment is to name the ambient function that is no longer called.
    /// Scanning comments would report that documentation as the defect.
    body: String,
}

fn functions_in(file: &str, source: &str) -> Vec<RootedFunction> {
    let lines: Vec<&str> = source.lines().collect();
    let mut functions = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let Some(name) = function_name(trimmed) else {
            continue;
        };
        let indent = line.len() - trimmed.len();
        let Some((_signature, body)) = function_regions(&lines, index, indent) else {
            continue;
        };
        functions.push(RootedFunction {
            file: file.to_string(),
            line: index + 1,
            name: name.to_string(),
            body: strip_line_comments(&body),
        });
    }
    functions
}

/// Drop `//` line comments. A `//` inside a string literal truncates the line
/// early, which can only lose a match — a missed finding, never a misattributed
/// one, which is the same trade `function_regions` already makes.
fn strip_line_comments(body: &str) -> String {
    body.lines()
        .map(|line| match line.find("//") {
            Some(offset) => &line[..offset],
            None => line,
        })
        .collect::<Vec<&str>>()
        .join("\n")
}

/// Whether `haystack` contains `needle` as a whole free path token.
///
/// Two things bound a match, and both matter:
///
/// * an identifier character at either end of the needle must not abut another
///   identifier character, which is what keeps `store::` from matching
///   `lifecycle_store::` and `paths::homeboy_data` from matching
///   `paths::homeboy_data_store`;
/// * a needle that begins an identifier must not follow a `.`, because that is
///   a method call on a receiver, not a free path. `lifecycle_store.read_record(`
///   is the *correct* rooted form and reads identically to the ambient
///   `read_record(` without this rule — it was 40-odd false positives on the
///   first run of this check.
///
/// A needle that starts or ends in `(`, `:` or similar is already self-bounding
/// on that side and is not constrained there.
fn contains_token(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let bound_left = needle.as_bytes().first().is_some_and(|b| is_ident_byte(*b));
    let bound_right = needle.as_bytes().last().is_some_and(|b| is_ident_byte(*b));
    let mut from = 0;
    while let Some(offset) = haystack[from..].find(needle) {
        let start = from + offset;
        let end = start + needle.len();
        let left_ok = !bound_left
            || start == 0
            || (!is_ident_byte(bytes[start - 1]) && bytes[start - 1] != b'.');
        let right_ok = !bound_right || end == bytes.len() || !is_ident_byte(bytes[end]);
        if left_ok && right_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
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

// ---------------------------------------------------------------------------
// Fixtures: the scanner scanning itself.
//
// A scanner that has never been shown to fail is indistinguishable from a
// scanner that cannot fail. These fixtures are the exact code #12618 found and
// the exact code it merged, so the assertion below is "this check would have
// caught the live bug", not "this check compiles".
// ---------------------------------------------------------------------------

/// `record_aggregate_in_store` as it stood before #12618, alongside the wrapper
/// pair it reached through.
///
/// The point of this fixture is what is *not* in it: no store constructor, no
/// `store::` shim, no `paths::`, no `default_store()`. Every direct needle in
/// every direct needle walks straight past this body. The only thing that gives it
/// away is that `update_cook_candidate_after_completion` has an `_in_store`
/// sibling and resolves a root of its own.
const PRE_FIX_RECORD_AGGREGATE: &str = r#"
pub(crate) fn update_cook_candidate_after_completion(
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
    promotion: Option<Value>,
) -> Result<()> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    update_cook_candidate_after_completion_in_store(&lifecycle_store, record, aggregate, promotion)
}

pub(crate) fn update_cook_candidate_after_completion_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
    promotion: Option<Value>,
) -> Result<()> {
    lifecycle_store.update_cook_index(cook_id, |index| {
        replace_latest_substantive_candidate(index, candidate)
    })?;
    Ok(())
}

pub(crate) fn record_aggregate_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &mut AgentTaskRunRecord,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> Result<AgentTaskRunRecord> {
    let aggregate_path = lifecycle_store.aggregate_path(&record.run_id);
    crate::controller_scratch::finalize_run_at(&lifecycle_store.data_root(), &record.run_id)?;
    lifecycle_store.write_aggregate_and_record(record, aggregate)?;
    record_terminal_artifact_projection_in_store(lifecycle_store, record, aggregate)?;
    update_cook_candidate_after_completion(record, aggregate, None)?;
    Ok(record.clone())
}
"#;

/// The same region as #12618 merged it: one call rerouted to the rooted sibling,
/// with a comment that names the ambient function it stopped calling.
const FIXED_RECORD_AGGREGATE: &str = r#"
pub(crate) fn update_cook_candidate_after_completion(
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
    promotion: Option<Value>,
) -> Result<()> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    update_cook_candidate_after_completion_in_store(&lifecycle_store, record, aggregate, promotion)
}

pub(crate) fn update_cook_candidate_after_completion_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
    promotion: Option<Value>,
) -> Result<()> {
    lifecycle_store.update_cook_index(cook_id, |index| {
        replace_latest_substantive_candidate(index, candidate)
    })?;
    Ok(())
}

pub(crate) fn record_aggregate_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &mut AgentTaskRunRecord,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> Result<AgentTaskRunRecord> {
    let aggregate_path = lifecycle_store.aggregate_path(&record.run_id);
    crate::controller_scratch::finalize_run_at(&lifecycle_store.data_root(), &record.run_id)?;
    lifecycle_store.write_aggregate_and_record(record, aggregate)?;
    record_terminal_artifact_projection_in_store(lifecycle_store, record, aggregate)?;
    // This used to call update_cook_candidate_after_completion(record, aggregate, None)?,
    // which wrote the substantive-candidate pointer into whatever home the
    // environment pointed at (#7505, #12618).
    update_cook_candidate_after_completion_in_store(lifecycle_store, record, aggregate, None)?;
    Ok(record.clone())
}
"#;

#[test]
fn the_ambient_reach_check_catches_the_pre_fix_record_aggregate_in_store() {
    let functions = functions_in("fixture.rs", PRE_FIX_RECORD_AGGREGATE);
    let wrappers = ambient_wrappers(&functions);
    assert!(
        wrappers.contains("update_cook_candidate_after_completion"),
        "the wrapper half of the pair was not recognised, so the finding below \
         would be vacuous; wrappers found: {wrappers:?}"
    );

    let findings = shadow_findings(&functions, &wrappers);
    let flagged: Vec<&str> = findings
        .iter()
        .map(|finding| finding.function.as_str())
        .collect();
    assert_eq!(
        flagged,
        ["record_aggregate_in_store"],
        "expected exactly the pre-fix body to be flagged"
    );
    assert_eq!(
        findings[0].reaches.len(),
        1,
        "expected one reach, got {:?}",
        findings[0].reaches
    );
    assert!(
        findings[0].reaches[0].starts_with("`update_cook_candidate_after_completion(`"),
        "the reach must name the ambient call, not something incidental: {}",
        findings[0].reaches[0]
    );
}

#[test]
fn the_ambient_reach_check_clears_the_merged_record_aggregate_in_store() {
    let functions = functions_in("fixture.rs", FIXED_RECORD_AGGREGATE);
    let wrappers = ambient_wrappers(&functions);
    assert!(
        wrappers.contains("update_cook_candidate_after_completion"),
        "the wrapper half of the pair was not recognised, so a clean result \
         below would prove nothing; wrappers found: {wrappers:?}"
    );

    let findings = shadow_findings(&functions, &wrappers);
    let flagged: Vec<&str> = findings
        .iter()
        .map(|finding| finding.function.as_str())
        .collect();
    assert!(
        findings.is_empty(),
        "the merged shape must be clean — the rerouted call ends in \
         `_in_store(` and the ambient name survives only in a comment; \
         flagged: {flagged:?}"
    );
}

#[test]
fn the_ambient_reach_check_catches_the_direct_forms() {
    let fixture = r#"
fn resolve_run_id_in_store(lifecycle_store: &AgentTaskLifecycleStore, run_id: &str) -> Result<String> {
    let index = store::read_cook_index(run_id)?;
    let root = homeboy_core::paths::homeboy_data()?;
    let observations = ObservationStore::open_initialized()?;
    let recipes = CookRecipeStore::from_current_data_root()?;
    Ok(run_id.to_string())
}
"#;
    let findings = shadow_findings(&functions_in("fixture.rs", fixture), &BTreeSet::new());
    assert_eq!(findings.len(), 1);
    let reaches = findings[0].reaches.join("\n");
    for expected in [
        "`store::`",
        "`paths::homeboy_data`",
        "`ObservationStore::open_initialized(`",
        "`CookRecipeStore::from_current_data_root(`",
    ] {
        assert!(
            reaches.contains(expected),
            "missing {expected} in:\n{reaches}"
        );
    }
}

#[test]
fn the_ambient_reach_check_does_not_see_the_rooted_forms() {
    // Everything here is the correct shape: methods on the injected store, the
    // `_in_store` sibling of every helper, and the two `paths::` helpers that
    // are deliberately process-global.
    let fixture = r#"
fn resolve_run_id_in_store(lifecycle_store: &AgentTaskLifecycleStore, run_id: &str) -> Result<String> {
    let index = lifecycle_store.read_cook_index(run_id)?;
    let record = lifecycle_store.read_record(run_id)?;
    let observations = lifecycle_store.open_observation_store()?;
    let pins = homeboy_core::paths::controller_runtimes_store()?;
    let targets = homeboy_core::paths::cargo_targets_store()?;
    let segment = homeboy_core::paths::sanitize_path_segment(run_id);
    let verified = with_acceptance_verifier(|verifier| verifier.revalidate(run_id))?;
    read_cook_index_in_store(lifecycle_store, run_id)
}
"#;
    let mut wrappers = BTreeSet::new();
    wrappers.insert("read_cook_index");
    wrappers.insert("read_record");
    let findings = shadow_findings(&functions_in("fixture.rs", fixture), &wrappers);
    assert!(
        findings.is_empty(),
        "rooted forms were flagged: {:?}",
        findings.first().map(|finding| &finding.reaches)
    );
}

#[test]
fn path_tokens_are_bounded_at_both_ends() {
    // The left bound: `store::` is a free shim, `lifecycle_store::` is a module.
    assert!(contains_token(
        "let r = store::read_record(id)?;",
        "store::"
    ));
    assert!(!contains_token(
        "let r = lifecycle_store::read_record(id)?;",
        "store::"
    ));
    // The right bound: the data root is a lifecycle root, the runtime pin store
    // that hangs below it is not.
    assert!(contains_token(
        "paths::homeboy_data()?",
        "paths::homeboy_data"
    ));
    assert!(!contains_token(
        "paths::homeboy_data_store(name)?",
        "paths::homeboy_data"
    ));
    // The method bound: this is the single distinction between the correct
    // rooted call and the ambient free call, and nothing else in the text
    // separates them.
    assert!(contains_token("read_record(run_id)?", "read_record("));
    assert!(!contains_token(
        "lifecycle_store.read_record(run_id)?",
        "read_record("
    ));
    // A delegating call to the sibling is not a call to the wrapper.
    assert!(!contains_token(
        "update_cook_candidate_after_completion_in_store(lifecycle_store, record)?",
        "update_cook_candidate_after_completion("
    ));
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
