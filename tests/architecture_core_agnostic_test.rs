fn source_file(relative_path: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative_path);
    std::fs::read_to_string(path).expect("read source file")
}

#[test]
fn core_source_does_not_depend_on_command_layer() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let core_root = root.join("src/core");
    let forbidden = [
        "use crate::commands",
        "crate::commands::",
        "use homeboy::commands",
        "homeboy::commands::",
        "use crate::cli_surface",
        "crate::cli_surface::",
        "use homeboy::cli_surface",
        "homeboy::cli_surface::",
    ];
    let mut violations = Vec::new();

    scan_core_source_for_command_layer(root, &core_root, &forbidden, &mut violations);

    assert!(
        violations.is_empty(),
        "core source must not depend on the command/CLI layer:\n{}\n\nMove command parsing/execution behind an injected adapter owned by src/commands.",
        violations.join("\n")
    );
}

fn scan_core_source_for_command_layer(
    root: &std::path::Path,
    path: &std::path::Path,
    forbidden: &[&str],
    violations: &mut Vec<String>,
) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).expect("read core source directory") {
            let entry = entry.expect("read core source entry");
            scan_core_source_for_command_layer(root, &entry.path(), forbidden, violations);
        }
        return;
    }

    if path.extension().is_none_or(|extension| extension != "rs") {
        return;
    }

    let content = std::fs::read_to_string(path).expect("read core source file");
    let relative = path
        .strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    let mut skip_rest_as_test_module = false;

    for (index, line) in content.lines().enumerate() {
        if line.trim() == "#[cfg(test)]" {
            skip_rest_as_test_module = true;
            continue;
        }
        if skip_rest_as_test_module {
            continue;
        }

        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("//!") {
            continue;
        }

        for term in forbidden {
            if line.contains(term) {
                violations.push(format!("{relative}:{} contains `{term}`", index + 1));
            }
        }
    }
}

#[test]
fn library_root_does_not_flatten_core_surface() {
    let source = source_file("src/lib.rs");

    assert!(
        !source.contains("pub use core::*"),
        "src/lib.rs must keep core APIs behind homeboy::core instead of flattening the crate root"
    );
}

#[test]
fn server_root_does_not_wildcard_reexport_private_modules() {
    let source = source_file("src/core/server/mod.rs");

    assert!(
        !source.contains("pub use client::*")
            && !source.contains("pub use connection::*")
            && !source.contains("pub use keys::*")
            && !source.contains("pub use session::*"),
        "src/core/server/mod.rs must explicitly name the server APIs it re-exports"
    );
}

#[test]
fn validate_and_format_writes_do_not_select_ecosystem_commands() {
    let files = [
        "src/core/engine/validate_write.rs",
        "src/core/engine/format_write.rs",
    ];
    let forbidden = [
        "Cargo.toml",
        "cargo check",
        "cargo fmt",
        "tsconfig.json",
        "npx tsc",
        "prettier",
        "go vet",
        "gofmt",
        "phpcbf",
        "rustfmt",
    ];

    for file in files {
        let source = source_file(file);
        for term in forbidden {
            assert!(
                !source.contains(term),
                "{file} must not hardcode ecosystem command or marker `{term}`"
            );
        }
    }
}
