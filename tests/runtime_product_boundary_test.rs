#[test]
fn core_boundary_policy_has_explicit_owned_roots_and_product_terms() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("homeboy.json");
    let config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(manifest).expect("homeboy.json should be readable"),
    )
    .expect("homeboy.json should parse");
    let policy = config["audit"]["source_policies"]
        .as_array()
        .expect("source_policies should be an array")
        .iter()
        .find(|policy| policy["convention"] == "core_boundary_leak:core-agnostic-source")
        .expect("core boundary source policy should exist");

    assert_eq!(
        values(policy, "include_path_contains"),
        [
            "src/core",
            "src/commands/component.rs",
            "src/commands/extension.rs",
            "src/commands/lint.rs",
            "src/commands/report.rs",
            "src/commands/resources/mod.rs",
            "src/commands/review/mod.rs",
            "src/commands/test.rs",
        ],
        "core boundary scanning must stay limited to explicit core-owned implementation roots"
    );

    assert_eq!(
        values(policy, "ignore_after_line_equals"),
        ["#[cfg(test)]"],
        "source-policy scanning should avoid test-only examples instead of raw-scanning prose"
    );

    let terms = policy["terms"]
        .as_array()
        .expect("source policy terms should be an array")
        .iter()
        .filter_map(|term| term["value"].as_str())
        .collect::<std::collections::BTreeSet<_>>();

    for term in [
        "wordpress",
        "WooCommerce",
        "wp-content",
        "WP_CLI",
        "codebox",
        "wp-codebox",
        "wpcom-codebox",
    ] {
        assert!(
            terms.contains(term),
            "core boundary source policy must include product/runtime term `{term}`"
        );
    }
}

fn values<'a>(policy: &'a serde_json::Value, key: &str) -> Vec<&'a str> {
    policy[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} should be an array"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("{key} entry should be a string"))
        })
        .collect()
}
