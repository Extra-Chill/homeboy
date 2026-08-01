//! Guards the shipped `core-agnostic-source` term list in `homeboy.json`.
//!
//! The policy is the only instrument that measures ecosystem coupling in core.
//! Its terms are data, so a silent deletion is invisible in code review and
//! shows up only as findings that stop being reported — the same failure mode
//! as #10278's dead baseline, one layer up. These assertions name the term
//! FAMILIES the policy is required to cover (#10279) so removing a whole class
//! is a test failure rather than a quiet loss of coverage.
//!
//! Individual terms inside a family are deliberately not all pinned: the point
//! is that the class stays represented, not that the list is frozen.

use serde_json::Value;

fn homeboy_config() -> Value {
    serde_json::from_str(include_str!("../homeboy.json")).expect("homeboy.json parses")
}

fn core_agnostic_source() -> Value {
    homeboy_config()["audit"]["source_policies"]
        .as_array()
        .expect("source_policies is an array")
        .iter()
        .find(|policy| policy["id"] == "core-agnostic-source")
        .expect("core-agnostic-source policy is configured")
        .clone()
}

fn terms() -> Vec<(String, String)> {
    let policy = core_agnostic_source();
    let default_match = policy["default_match"]
        .as_str()
        .unwrap_or("token")
        .to_string();
    policy["terms"]
        .as_array()
        .expect("terms is an array")
        .iter()
        .map(|term| {
            let value = term["value"]
                .as_str()
                .expect("every term has a value")
                .to_string();
            let mode = term["match_mode"]
                .as_str()
                .unwrap_or(default_match.as_str())
                .to_string();
            (value, mode)
        })
        .collect()
}

fn match_mode_of(configured: &[(String, String)], value: &str) -> Option<String> {
    configured
        .iter()
        .find(|(term, _)| term.as_str() == value)
        .map(|(_, mode)| mode.clone())
}

/// Terms from `required` that the shipped policy no longer configures.
fn missing_terms(required: &[&str]) -> Vec<String> {
    let configured = terms();
    required
        .iter()
        .copied()
        .filter(|value| match_mode_of(&configured, value).is_none())
        .map(str::to_string)
        .collect()
}

#[test]
fn rust_toolchain_terms_are_configured() {
    // `CiFailureCategory { Fmt, Clippy, .. }` in homeboy-core is core reasoning
    // about one specific toolchain's job names.
    let missing = missing_terms(&[
        "rustfmt",
        "clippy",
        "rustc",
        "nextest",
        "target/release",
        "target/debug",
        ".cargo",
    ]);

    assert!(
        missing.is_empty(),
        "core-agnostic-source lost rust-toolchain terms: {missing:?}"
    );
}

#[test]
fn toolchain_output_formats_are_configured_in_regex_mode() {
    // The sneakiest class: core classifies failures by sniffing one compiler's
    // stdout. No token match can see a punctuation-heavy diagnostic prefix, so
    // `error[E` has to be a regex.
    let missing = missing_terms(&[
        "error\\[E",
        "test result:",
        "panicked at",
        "could not compile",
    ]);

    assert!(
        missing.is_empty(),
        "core-agnostic-source lost toolchain-output terms: {missing:?}"
    );

    let mode = match_mode_of(&terms(), "error\\[E").expect("checked above that the term exists");
    assert_eq!(
        mode.as_str(),
        "regex",
        "`error[E` is only matchable as a regex; literal/token mode silently matches nothing useful"
    );
}

#[test]
fn package_manager_and_install_channel_terms_are_configured() {
    let missing = missing_terms(&[
        "nvm",
        "node_modules",
        "pnpm",
        "yarn",
        "brew",
        "homebrew",
        "Cellar",
        "linuxbrew",
    ]);

    assert!(
        missing.is_empty(),
        "core-agnostic-source lost package-manager/install-channel terms: {missing:?}"
    );
}

#[test]
fn org_and_product_identity_terms_are_configured() {
    // An entire absent class before #10279: a hardcoded org, product, or sibling
    // tool name is ecosystem coupling exactly like a hardcoded runtime name.
    let missing = missing_terms(&["kimaki", "Extra-Chill", "extrachill", "Automattic"]);

    assert!(
        missing.is_empty(),
        "core-agnostic-source lost org/product-identity terms: {missing:?}"
    );
}

#[test]
fn interpreter_and_agent_runtime_terms_are_configured() {
    // #11098: the scanner listed `nodejs` but not `node`, and no interpreter or
    // agent-runtime name at all — so it reported clean on core hardcoding
    // `python3` (`homeboy-rig`'s `ServiceKind::HttpStatic`, core's artifact
    // preview, the generic trace runner's interpreter table, and four
    // core-shipped `runtime/*.sh` helpers that hard-fail without it).
    let missing = missing_terms(&[
        "python", "python3", "node", "opencode", "codex", "claude", "gemini",
    ]);

    assert!(
        missing.is_empty(),
        "core-agnostic-source lost interpreter/agent-runtime terms: {missing:?}"
    );
}

#[test]
fn ci_provider_terms_are_configured() {
    let missing = missing_terms(&[".github/workflows", "actions/"]);

    assert!(
        missing.is_empty(),
        "core-agnostic-source lost ci-provider terms: {missing:?}"
    );

    // Env-var-shaped terms match case-sensitively via an inline regex flag; a
    // case-insensitive match would flag every `github_token` identifier and
    // accessor, which is naming rather than provider coupling.
    let mode = match_mode_of(&terms(), "(?-i:GITHUB_TOKEN)")
        .expect("core-agnostic-source must keep the GITHUB_TOKEN term with its case override");
    assert_eq!(
        mode.as_str(),
        "regex",
        "an inline case-sensitivity override is only honored in regex mode"
    );
}

#[test]
fn lint_suppression_attributes_stay_allowlisted() {
    // `clippy` is a required term, but 62 of its 69 tree matches were
    // `#[allow(clippy::…)]` attributes — lint suppression, not ecosystem
    // behavior. Dropping this allowlist entry re-floods the policy with noise
    // that has nothing to do with core boundaries.
    let policy = core_agnostic_source();
    let allowed = policy["allow_line_contains"]
        .as_array()
        .expect("allow_line_contains is an array")
        .iter()
        .filter_map(|value| value.as_str())
        .collect::<Vec<_>>();

    assert!(
        allowed.contains(&"clippy::"),
        "core-agnostic-source must keep `clippy::` allowlisted alongside the `clippy` term"
    );
}

#[test]
fn every_configured_term_declares_a_supported_match_mode() {
    for (value, mode) in terms() {
        assert!(
            matches!(mode.as_str(), "token" | "literal" | "regex"),
            "term `{value}` declares unsupported match mode `{mode}`"
        );
    }
}

#[test]
fn new_term_findings_are_covered_by_the_baseline_policy_section() {
    // #10279 widened the term list against a baseline regenerated by #10278.
    // If a widening ever lands without its baseline rows, the full-profile
    // audit reports the whole class as brand-new debt. The section must at
    // least carry a row for each family this test pins.
    //
    // This pins WIDENING families only. `target/release` used to be pinned here
    // and no longer is: #11096 allowlisted homeboy's own build artifact path
    // (`target/release/homeboy`), which retired all 40 of that term's baseline
    // rows as self-reference and test-tree noise. The term is still required —
    // `rust_toolchain_terms_are_configured` pins it — but its one remaining
    // live finding (`homeboy-upgrade/src/self_status.rs` reasoning about
    // `/target/release/`) is unaccepted debt, not baselined debt. Requiring a
    // baseline row for a narrowed family forces the noise back in.
    let config = homeboy_config();
    let section = config["baselines"]["audit"]["metadata"]["policy_sections"]
        .as_array()
        .expect("policy_sections is an array")
        .iter()
        .find(|section| section["audit_policy"] == "core_boundary_leak:core-agnostic-source")
        .expect("core-agnostic-source has a baseline policy section");
    let rows = section["known_fingerprints"]
        .as_array()
        .expect("known_fingerprints is an array")
        .iter()
        .filter_map(|row| row.as_str())
        .collect::<Vec<_>>();

    for term in [
        "node_modules",
        "homebrew",
        "Extra-Chill",
        // #11098's widening: the agent-runtime and interpreter families the
        // scanner was blind to until this list grew.
        "python3",
        "node",
        "opencode",
    ] {
        assert!(
            rows.iter()
                .any(|row| row.contains(&format!("term `{term}`"))),
            "baseline policy section has no row for the `{term}` term"
        );
    }
}
