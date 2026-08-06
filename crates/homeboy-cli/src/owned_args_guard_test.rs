//! Source guard: a raw argv flag scan must route through the separator
//! primitive.
//!
//! Two sites scanned argv for a flag Homeboy owns without stopping at the bare
//! separator, so a forwarded remote argument was read as Homeboy's own: a
//! remote short help flag downgraded a mutating `ssh` invocation to `ReadOnly`,
//! and the startup help fast path claimed an invocation clap went on to parse
//! successfully (#11577). They were written months apart by different changes,
//! which is the tell — the shape reads correct, and the failure is a silent
//! misclassification rather than a crash.
//!
//! [`super::homeboy_owned_args`] fixed both. This guard is what stops a third:
//! it walks this crate's sources, finds every function that takes a full-argv
//! slice and compares an element against a flag literal, and requires each one
//! to either route through the primitive or appear in the inventory below with
//! a stated reason. Adding a scan is then a deliberate act with a written
//! justification instead of an unnoticed regression (#11755).
//!
//! Following the `core-agnostic-source` precedent, this reads source text, so
//! it is deliberately shape-based rather than semantic. Lines that begin with a
//! comment marker are skipped, so prose describing the shape is not a finding.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Every function in this crate that reads a flag literal out of a full-argv
/// slice without going through [`super::homeboy_owned_args`], with the reason
/// it is accepted.
///
/// This inventory is the artifact #11755 asked for. The three statuses below
/// are load-bearing:
///
/// * *stops at the separator itself* — correct, but a fourth hand-rolled copy
///   of the same rule. These are the unification candidates.
/// * *cannot be reached through a separator* — correct by construction.
/// * *scans past the separator* — the #11577 shape, unaudited. Each is a
///   latent misread of a forwarded argument, tracked rather than fixed here
///   because the correct behavior differs per call site.
const ACKNOWLEDGED_RAW_ARGV_FLAG_SCANS: &[(&str, &str)] = &[
    (
        "cli_runtime.rs::explicit_runner_from_args",
        "stops at the separator itself: returns None on the first bare separator",
    ),
    (
        "cli_runtime.rs::is_runs_artifact_get_runner_option",
        "scans past the separator: a forwarded runner flag can satisfy this predicate",
    ),
    (
        "cli_runtime.rs::is_runs_list_runner_option",
        "scans past the separator: a forwarded runner flag can satisfy this predicate",
    ),
    (
        "cli_runtime.rs::is_top_level_version_request",
        "cannot be reached through a separator: an exact two-element slice pattern",
    ),
    (
        "commands/infra/route.rs::explicit_run_id",
        "scans past the separator: a forwarded run-id flag is read as Homeboy's own",
    ),
    (
        "commands/infra/route.rs::has_lab_changed_files_json",
        "scans past the separator: a forwarded flag of the same name satisfies this",
    ),
    (
        "commands/infra/route.rs::inline_portable_settings_profiles",
        "stops at the separator itself: copies the tail verbatim once it is reached",
    ),
    (
        "commands/infra/route.rs::is_runs_list_runner_option",
        "scans past the separator: a forwarded runner flag can satisfy this predicate",
    ),
    (
        "commands/infra/route.rs::portable_deferred_args",
        "scans past the separator: strips placement flags from forwarded arguments too",
    ),
    (
        "commands/infra/route.rs::retry_handoff_prefix",
        "scans past the separator: strips path flags from forwarded arguments too",
    ),
    (
        "commands/infra/route.rs::source_path_for_generic_detached_lab_handoff",
        "stops at the separator itself: breaks out on the first bare separator",
    ),
    (
        "commands/infra/route.rs::strip_component_target_args",
        "stops at the separator itself: flips to verbatim passthrough on reaching it",
    ),
    (
        "commands/infra/route/local_detach.rs::detached_cook_child_args",
        "scans past the separator: filters a detach flag out of forwarded arguments too",
    ),
    (
        "commands/infra/route/local_detach.rs::stdin_prompt_index",
        "scans past the separator: a forwarded prompt flag is read as Homeboy's own",
    ),
    (
        "commands/utils/execution_provenance.rs::redact_execution_argv",
        "intentionally covers forwarded arguments: a secret is no less secret past the separator",
    ),
    (
        "commands/utils/resource_policy/messages.rs::append_local_placement",
        "scans past the separator: a forwarded placement flag suppresses the suggestion",
    ),
    (
        "commands/utils/resource_policy/mod.rs::rerun_command",
        "scans past the separator: a forwarded runner flag suppresses the suggestion",
    ),
];

/// Sites that already route through the primitive. Pinned so a refactor that
/// quietly drops the call is a failure here rather than a silent regression.
const SANCTIONED_ARGV_FLAG_SCANS: &[&str] = &[
    "cli_runtime.rs::startup_fast_path",
    "command_capability.rs::classify",
];

/// The comparison shapes that read a Homeboy-owned flag out of an argument.
///
/// The separator is deliberately not one of them: a bare separator carries no
/// flag name, so the trailing character must be alphabetic for a line to count.
const FLAG_LITERAL_COMPARISONS: &[&str] = &["== \"-", "starts_with(\"-"];

/// Parameter spellings that carry a full argv slice. A narrower parameter (an
/// already-split tail, a recorded command's own arguments) is a different
/// question and is out of scope.
const ARGV_PARAMETERS: &[&str] = &["args: &[String]", "argv: &[String]"];

fn crate_source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn is_test_source(relative_path: &str) -> bool {
    relative_path.contains("/tests/")
        || relative_path.ends_with("_test.rs")
        || relative_path.ends_with("_tests.rs")
        || relative_path.ends_with("/tests.rs")
        || relative_path == "tests.rs"
}

fn collect_rust_sources(directory: &Path, root: &Path, collected: &mut Vec<(String, String)>) {
    let entries = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()));

    for entry in entries {
        let entry = entry.expect("readable directory entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rust_sources(&path, root, collected);
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let relative_path = path
            .strip_prefix(root)
            .expect("collected path is under the source root")
            .to_string_lossy()
            .replace('\\', "/");
        if is_test_source(&relative_path) {
            continue;
        }
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        collected.push((relative_path, content));
    }
}

/// The name of the function this line declares, if it declares one.
fn declared_fn_name(line: &str) -> Option<&str> {
    let mut search = 0;
    while let Some(offset) = line[search..].find("fn ") {
        let start = search + offset;
        let preceded_by_identifier = line[..start]
            .chars()
            .next_back()
            .is_some_and(|character| character.is_alphanumeric() || character == '_');
        if !preceded_by_identifier {
            let rest = &line[start + 3..];
            let end = rest
                .find(|character: char| !(character.is_alphanumeric() || character == '_'))
                .unwrap_or(rest.len());
            if end > 0 {
                return Some(&rest[..end]);
            }
        }
        search = start + 3;
    }
    None
}

/// Whether this line compares an argument against a named flag literal.
fn compares_against_a_flag_literal(line: &str) -> bool {
    FLAG_LITERAL_COMPARISONS.iter().any(|shape| {
        line.match_indices(*shape).any(|(index, _)| {
            line[index + shape.len()..]
                .trim_start_matches('-')
                .chars()
                .next()
                .is_some_and(char::is_alphabetic)
        })
    })
}

fn is_comment_line(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// Every argv flag scan in this crate, mapped to whether it routes through the
/// sanctioned primitive.
///
/// A scan is attributed to the nearest preceding function declaration, the same
/// way the shipped source-policy detector resolves a finding's enclosing
/// context. That is an approximation, and a deliberate one: it needs no parser,
/// and over-attribution only ever widens what the guard asks a human to look at.
fn raw_argv_flag_scans() -> BTreeMap<String, bool> {
    let root = crate_source_root();
    let mut sources = Vec::new();
    collect_rust_sources(&root, &root, &mut sources);
    assert!(
        !sources.is_empty(),
        "no rust sources found under {}",
        root.display()
    );

    let mut scans = BTreeMap::new();
    for (relative_path, content) in &sources {
        let lines: Vec<&str> = content.lines().collect();
        let declarations: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| !is_comment_line(line))
            .filter(|(_, line)| declared_fn_name(line).is_some())
            .map(|(index, _)| index)
            .collect();

        for (index, line) in lines.iter().enumerate() {
            if is_comment_line(line) || !compares_against_a_flag_literal(line) {
                continue;
            }
            let Some(position) = declarations
                .iter()
                .rposition(|declaration| *declaration <= index)
            else {
                continue;
            };
            let start = declarations[position];
            let end = declarations
                .get(position + 1)
                .copied()
                .unwrap_or(lines.len());
            let region = &lines[start..end];

            let signature_end = region
                .iter()
                .position(|line| line.contains('{'))
                .map_or(region.len(), |offset| offset + 1);
            let signature = region[..signature_end].join("\n");
            if !ARGV_PARAMETERS
                .iter()
                .any(|parameter| signature.contains(parameter))
            {
                continue;
            }

            let name = declared_fn_name(lines[start]).expect("declaration line names a function");
            let routes_through_primitive = region.join("\n").contains("homeboy_owned_args");
            scans.insert(format!("{relative_path}::{name}"), routes_through_primitive);
        }
    }
    scans
}

#[test]
fn a_new_raw_argv_flag_scan_is_a_failure() {
    let scans = raw_argv_flag_scans();
    let unsanctioned: Vec<String> = scans
        .iter()
        .filter(|(site, routes_through_primitive)| {
            !*routes_through_primitive
                && !ACKNOWLEDGED_RAW_ARGV_FLAG_SCANS
                    .iter()
                    .any(|(acknowledged, _)| *acknowledged == site.as_str())
        })
        .map(|(site, _)| site.clone())
        .collect();

    assert!(
        unsanctioned.is_empty(),
        "these functions read a flag literal out of raw argv without stopping at the bare \
         separator, so a forwarded argument can be read as Homeboy's own (#11577): {unsanctioned:?}. \
         Call `homeboy_owned_args` first, or add the site to \
         ACKNOWLEDGED_RAW_ARGV_FLAG_SCANS with the reason it is safe."
    );
}

#[test]
fn an_acknowledged_scan_that_no_longer_exists_is_a_failure() {
    // A stale entry is how an inventory stops describing the tree it guards.
    // The row must still name a real scan, and that scan must still be raw:
    // a site that has since adopted the primitive belongs in the sanctioned
    // list, not in the acknowledged one.
    let scans = raw_argv_flag_scans();
    let stale: Vec<&str> = ACKNOWLEDGED_RAW_ARGV_FLAG_SCANS
        .iter()
        .filter(|(site, _)| scans.get(*site) != Some(&false))
        .map(|(site, _)| *site)
        .collect();

    assert!(
        stale.is_empty(),
        "ACKNOWLEDGED_RAW_ARGV_FLAG_SCANS names scans that no longer exist as raw argv \
         scans: {stale:?}. Remove the stale rows."
    );
}

#[test]
fn every_acknowledged_scan_states_a_reason() {
    for (site, reason) in ACKNOWLEDGED_RAW_ARGV_FLAG_SCANS {
        assert!(
            reason.len() > 20,
            "{site} is acknowledged without a usable reason"
        );
    }
}

#[test]
fn the_sanctioned_sites_still_route_through_the_primitive() {
    let scans = raw_argv_flag_scans();
    for site in SANCTIONED_ARGV_FLAG_SCANS {
        assert_eq!(
            scans.get(*site),
            Some(&true),
            "{site} no longer routes its argv flag scan through homeboy_owned_args"
        );
    }
}

#[test]
fn the_guard_recognizes_the_shape_it_is_written_to_catch() {
    // The two #11577 sites, as they were written.
    assert!(compares_against_a_flag_literal(
        r#"if args.iter().any(|arg| arg == "--help" || arg == "-h") {"#
    ));
    assert!(compares_against_a_flag_literal(
        r#"index > list_index && (arg == "--runner" || arg.starts_with("--runner="))"#
    ));

    // The separator carries no flag name, so the primitive's own scan for it
    // is not a finding — otherwise the guard would flag its own foundation.
    assert!(!compares_against_a_flag_literal(
        r#"args.iter().position(|arg| arg == "--")"#
    ));
    // A subcommand token is not a flag.
    assert!(!compares_against_a_flag_literal(
        r#"args.iter().position(|arg| arg == "runs")"#
    ));
    // A negative number is not a flag either.
    assert!(!compares_against_a_flag_literal(r#"value == "-1""#));
}

#[test]
fn function_attribution_names_the_enclosing_declaration() {
    assert_eq!(
        declared_fn_name("pub fn homeboy_owned_args(args: &[String]) -> &[String] {"),
        Some("homeboy_owned_args")
    );
    assert_eq!(
        declared_fn_name("    fn is_top_level_version_request(args: &[String]) -> bool {"),
        Some("is_top_level_version_request")
    );
    assert_eq!(
        declared_fn_name("    let owned = separator_index(argv);"),
        None
    );
    // A word ending in `fn` must not be read as a declaration keyword.
    assert_eq!(declared_fn_name("    let asfn = 1;"), None);
}

#[test]
fn test_sources_are_out_of_scope() {
    assert!(is_test_source("commands/runner/tests/status.rs"));
    assert!(is_test_source("commands/bench/parse_tests.rs"));
    assert!(is_test_source("owned_args_guard_test.rs"));
    assert!(!is_test_source("cli_runtime.rs"));
    assert!(!is_test_source("commands/infra/route.rs"));
}
