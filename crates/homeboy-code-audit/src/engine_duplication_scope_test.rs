//! Wiring invariants for the duplication family's changed-scope seed (#12583).
//!
//! Wired into `engine.rs` via
//! `#[cfg(test)] #[path = ...] mod engine_duplication_scope_test`.
//!
//! WHY THIS EXISTS: #12587 added scope-seeded exact duplicate analysis and
//! proved its EQUIVALENCE against unscoped analysis in `duplication/tests.rs`.
//! Nothing called it, so the
//! whole optimization was dormant and every one of those equivalence tests kept
//! passing. Equivalence tests cannot see a missing call site.
//!
//! So these tests assert the WIRING instead: that `run_duplication_family`
//! actually threads its `scoped_fingerprints` argument into that pass, and
//! that the five passes without a scoped variant still consume the full corpus.
//! Together with #12587's equivalence proofs that is the whole risk surface of
//! the wiring slice.

use super::*;
use crate::conventions::Language;

/// A structural twin body, 3 body lines so it clears the near-duplicate
/// trivial-body floor (`MIN_BODY_LINES`). Same shape as `duplication/tests.rs`.
const TWIN_A: &str = "fn cache_path() -> Option<PathBuf> {\n    let base = paths::homeboy().ok()?;\n    let file = base.join(CACHE_A);\n    Some(file)\n}\n";
const TWIN_B: &str = "fn cache_path() -> Option<PathBuf> {\n    let base = paths::homeboy().ok()?;\n    let file = base.join(CACHE_B);\n    Some(file)\n}\n";

fn fp(
    path: &str,
    content: &str,
    method_hashes: &[(&str, &str)],
    structural_hashes: &[(&str, &str)],
) -> fingerprint::FileFingerprint {
    fingerprint::FileFingerprint {
        relative_path: path.to_string(),
        language: Language::Rust,
        content: content.to_string(),
        method_hashes: method_hashes
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
        structural_hashes: structural_hashes
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
        ..Default::default()
    }
}

/// Four-file corpus with two independent exact-duplicate groups:
///
/// * `compute_alpha` in `alpha_one.rs` + `alpha_two.rs`
/// * `compute_beta` in `beta_one.rs` + `beta_two.rs`
///
/// The two beta files additionally carry a `cache_path` structural twin, which
/// only the (unscoped) near-duplicate pass can see. Scoping to `alpha_one.rs`
/// therefore distinguishes all three behaviors at once: the alpha group must
/// survive WITH its out-of-scope counterpart, the beta group must be dropped, and
/// the near-duplicate pass must still find the beta twins.
fn corpus() -> Vec<fingerprint::FileFingerprint> {
    vec![
        fp(
            "src/core/alpha_one.rs",
            "fn compute_alpha() {}\n",
            &[("compute_alpha", "h_alpha")],
            &[],
        ),
        fp(
            "src/core/alpha_two.rs",
            "fn compute_alpha() {}\n",
            &[("compute_alpha", "h_alpha")],
            &[],
        ),
        fp(
            "src/core/beta_one.rs",
            TWIN_A,
            &[("compute_beta", "h_beta"), ("cache_path", "hash_a")],
            &[("cache_path", "SAME_STRUCT")],
        ),
        fp(
            "src/core/beta_two.rs",
            TWIN_B,
            &[("compute_beta", "h_beta"), ("cache_path", "hash_b")],
            &[("cache_path", "SAME_STRUCT")],
        ),
    ]
}

fn run(
    scoped: &[&fingerprint::FileFingerprint],
    all: &[&fingerprint::FileFingerprint],
) -> DuplicationUnit {
    run_duplication_family(
        &AuditExecutionPlan::full(),
        scoped,
        all,
        &HashSet::new(),
        &AuditConfig::default(),
    )
}

fn files(items: &[findings::Finding]) -> Vec<&str> {
    let mut out: Vec<&str> = items.iter().map(|f| f.file.as_str()).collect();
    out.sort();
    out
}

#[test]
fn changed_scope_seeds_exact_duplicate_analysis_from_the_scoped_subset() {
    let owned = corpus();
    let all: Vec<&fingerprint::FileFingerprint> = owned.iter().collect();
    // The engine builds its subset by filtering `all_fingerprints`, so the
    // `scoped ⊆ all` precondition holds by construction. Mirror that here.
    let scoped: Vec<&fingerprint::FileFingerprint> = all
        .iter()
        .copied()
        .filter(|fp| fp.relative_path == "src/core/alpha_one.rs")
        .collect();

    let unit = run(&scoped, &all);

    // Seeded from the scoped subset: the beta group has no in-scope member, so
    // it is dropped. If the engine still passed the full corpus as the seed,
    // `compute_beta` findings would be here.
    assert_eq!(
        files(&unit.exact),
        vec!["src/core/alpha_one.rs", "src/core/alpha_two.rs"],
        "exact pass must be seeded from the scoped subset, and expanded against the full corpus"
    );
    assert!(
        unit.exact
            .iter()
            .all(|f| f.description.contains("compute_alpha")),
        "the out-of-scope-only group must not be reported: {:?}",
        unit.exact
            .iter()
            .map(|f| f.description.as_str())
            .collect::<Vec<_>>()
    );

    let group_names: Vec<&str> = unit
        .groups
        .iter()
        .map(|g| g.function_name.as_str())
        .collect();
    assert_eq!(
        group_names,
        vec!["compute_alpha"],
        "fixer groups must come from the same scoped analysis"
    );
    // Expansion still walked the full corpus: the counterpart is out of scope.
    assert_eq!(
        unit.groups[0].remove_from,
        vec!["src/core/alpha_two.rs".to_string()],
        "counterpart evidence must come from the full corpus, not the seed"
    );
}

#[test]
fn the_five_passes_without_a_scoped_variant_still_see_the_full_corpus() {
    let owned = corpus();
    let all: Vec<&fingerprint::FileFingerprint> = owned.iter().collect();
    let scoped: Vec<&fingerprint::FileFingerprint> = all
        .iter()
        .copied()
        .filter(|fp| fp.relative_path == "src/core/alpha_one.rs")
        .collect();

    let scoped_run = run(&scoped, &all);
    let unscoped_run = run(&all, &all);

    // `cache_path` lives only in the two out-of-scope beta files. A near-duplicate
    // pass narrowed to the seed would find nothing.
    assert_eq!(
        files(&scoped_run.near_duplicate),
        vec!["src/core/beta_one.rs", "src/core/beta_two.rs"],
        "near_duplicate must stay on the full corpus"
    );
    // And the seed must make no difference to it at all.
    assert_eq!(
        files(&scoped_run.near_duplicate),
        files(&unscoped_run.near_duplicate)
    );
    assert_eq!(
        files(&scoped_run.intra_method),
        files(&unscoped_run.intra_method)
    );
    assert_eq!(
        files(&scoped_run.cross_name),
        files(&unscoped_run.cross_name)
    );
    assert_eq!(files(&scoped_run.skeleton), files(&unscoped_run.skeleton));
    assert_eq!(
        files(&scoped_run.parallel_implementation),
        files(&unscoped_run.parallel_implementation)
    );
}

#[test]
fn unscoped_mode_passes_the_full_corpus_as_its_own_seed() {
    let owned = corpus();
    let all: Vec<&fingerprint::FileFingerprint> = owned.iter().collect();

    // What `audit_internal` does when `scoped_fingerprints` is `None`:
    // `per_file_fingerprints` IS `all_fingerprints`, which is the same `(all, all)`
    // delegation an unscoped exact analysis performs, so nothing is narrowed.
    let unit = run(&all, &all);

    assert_eq!(
        files(&unit.exact),
        vec![
            "src/core/alpha_one.rs",
            "src/core/alpha_two.rs",
            "src/core/beta_one.rs",
            "src/core/beta_two.rs"
        ],
        "unscoped mode must report both groups"
    );

    let mut group_names: Vec<&str> = unit
        .groups
        .iter()
        .map(|g| g.function_name.as_str())
        .collect();
    group_names.sort();
    assert_eq!(group_names, vec!["compute_alpha", "compute_beta"]);
}

#[test]
fn span_order_is_pass_order_regardless_of_the_seed() {
    let owned = corpus();
    let all: Vec<&fingerprint::FileFingerprint> = owned.iter().collect();
    let scoped: Vec<&fingerprint::FileFingerprint> = all
        .iter()
        .copied()
        .filter(|fp| fp.relative_path == "src/core/alpha_one.rs")
        .collect();

    let expected = vec![
        "detector.duplication.exact",
        "detector.duplication.intra_method",
        "detector.duplication.near_duplicate",
        "detector.duplication.cross_name_duplicate",
        "detector.duplication.skeleton_duplicate",
        "detector.duplication.parallel_implementation",
    ];

    for unit in [run(&scoped, &all), run(&all, &all)] {
        let ids: Vec<&str> = unit.spans.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids, expected,
            "threading the seed must not reorder the timing spans"
        );
    }
}
