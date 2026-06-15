use std::fs;

#[test]
fn manifest_default_run_selects_homeboy_cli_binary() {
    let manifest_path = format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(&manifest_path).expect("Cargo.toml should be readable");
    let value: toml::Value = toml::from_str(&manifest).expect("Cargo.toml should parse as TOML");

    assert_eq!(
        value
            .get("package")
            .and_then(|package| package.get("default-run"))
            .and_then(|default_run| default_run.as_str()),
        Some("homeboy")
    );
}

#[test]
fn manifest_keeps_self_audit_bench_binary_available() {
    let manifest_path = format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(&manifest_path).expect("Cargo.toml should be readable");
    let value: toml::Value = toml::from_str(&manifest).expect("Cargo.toml should parse as TOML");

    let binaries = value
        .get("bin")
        .and_then(|bin| bin.as_array())
        .expect("Cargo.toml should declare binaries");

    assert!(binaries.iter().any(|binary| {
        binary.get("name").and_then(|name| name.as_str()) == Some("bench-audit-self")
    }));
}
