//! Argument normalization utilities.
//!
//! Transforms CLI arguments before clap parsing to support intuitive syntax
//! that would otherwise require special handling.

/// Normalize version command arguments.
/// Converts `homeboy version <component_id>` to `homeboy version show <component_id>`
/// when the argument is not a recognized subcommand (show, set, bump, edit, merge).
pub(crate) fn normalize_version_show(args: Vec<String>) -> Vec<String> {
    if args.len() < 3 {
        return args;
    }

    let is_version_cmd = args.get(1).map(|s| s == "version").unwrap_or(false);
    if !is_version_cmd {
        return args;
    }

    let known_subcommands = ["show", "--help", "-h", "help"];
    let second_arg = args.get(2).map(|s| s.as_str()).unwrap_or("");

    // If it's already a known subcommand or a flag, pass through unchanged
    if known_subcommands.contains(&second_arg) || second_arg.starts_with('-') {
        return args;
    }

    // Otherwise, assume it's a component_id and insert "show"
    let mut result = Vec::with_capacity(args.len() + 1);
    result.push(args[0].clone()); // homeboy
    result.push(args[1].clone()); // version
    result.push("show".to_string()); // insert "show"
    result.extend(args[2..].iter().cloned()); // component_id and remaining args

    result
}

/// Normalize changelog add --component flag to positional form.
/// Converts `changelog add --component <value>` to `changelog add <value>`.
pub(crate) fn normalize_changelog_component(args: Vec<String>) -> Vec<String> {
    let is_changelog_add = args.len() >= 4
        && args.get(1).map(|s| s == "changelog").unwrap_or(false)
        && args.get(2).map(|s| s == "add").unwrap_or(false);

    if !is_changelog_add {
        return args;
    }

    // Look for --component flag
    let component_pos = args.iter().position(|s| s == "--component");
    let Some(component_pos) = component_pos else {
        return args;
    };

    // Need a value after --component
    let Some(component_value) = args.get(component_pos + 1) else {
        return args;
    };

    // Build normalized args: homeboy changelog add <component> [other args]
    let mut result = vec![
        args[0].clone(),         // homeboy
        args[1].clone(),         // changelog
        args[2].clone(),         // add
        component_value.clone(), // component as positional
    ];

    // Add remaining args, skipping --component and its value
    for (i, arg) in args.iter().enumerate().skip(3) {
        if i == component_pos || i == component_pos + 1 {
            continue;
        }
        result.push(arg.clone());
    }

    result
}

/// Auto-insert '--' separator before unknown flags for trailing_var_arg commands.
///
/// Commands that use trailing_var_arg with allow_hyphen_values need a '--' separator
/// to distinguish known flags from pass-through flags. This function auto-inserts it.
///
/// Syntax-agnostic: If '--' is already present, args pass through unchanged.
/// Both styles work identically:
///   homeboy component set my-plugin --field value
///   homeboy component set my-plugin -- --field value
pub(crate) fn normalize_trailing_flags(args: Vec<String>) -> Vec<String> {
    // Define commands and their known flags
    let commands: &[(&str, &str, &[&str])] = &[
        // (command, subcommand, known_flags)
        (
            "component",
            "set",
            &[
                "--json",
                "--base64",
                "--replace",
                "--version-target",
                "--extension",
                "--help",
                "-h",
            ],
        ),
        (
            "component",
            "edit",
            &[
                "--json",
                "--base64",
                "--replace",
                "--version-target",
                "--extension",
                "--help",
                "-h",
            ],
        ),
        (
            "component",
            "merge",
            &[
                "--json",
                "--base64",
                "--replace",
                "--version-target",
                "--extension",
                "--help",
                "-h",
            ],
        ),
        (
            "server",
            "set",
            &["--json", "--base64", "--replace", "--help", "-h"],
        ),
        (
            "server",
            "edit",
            &["--json", "--base64", "--replace", "--help", "-h"],
        ),
        (
            "server",
            "merge",
            &["--json", "--base64", "--replace", "--help", "-h"],
        ),
        (
            "fleet",
            "set",
            &["--json", "--base64", "--replace", "--help", "-h"],
        ),
        (
            "fleet",
            "edit",
            &["--json", "--base64", "--replace", "--help", "-h"],
        ),
        (
            "fleet",
            "merge",
            &["--json", "--base64", "--replace", "--help", "-h"],
        ),
        (
            "test",
            "",
            &[
                "--skip-lint",
                "--fix",
                "--coverage",
                "--coverage-min",
                "--baseline",
                "--ignore-baseline",
                "--ratchet",
                "--analyze",
                "--drift",
                "--scaffold",
                "--scaffold-file",
                "--write",
                "--since",
                "--changed-since",
                "--setting",
                "--path",
                "--json-summary",
                "--json",
                "--help",
                "-h",
            ],
        ),
        (
            "docs",
            "audit",
            &[
                "--path",
                "--docs-dir",
                "--baseline",
                "--ignore-baseline",
                "--features",
                "--help",
                "-h",
            ],
        ),
        (
            "lint",
            "",
            &[
                "--fix",
                "--baseline",
                "--ignore-baseline",
                "--summary",
                "--file",
                "--glob",
                "--changed-only",
                "--changed-since",
                "--errors-only",
                "--sniffs",
                "--exclude-sniffs",
                "--category",
                "--setting",
                "--path",
                "--json",
                "--help",
                "-h",
            ],
        ),
    ];

    // Find matching command pattern
    let known_flags = commands.iter().find_map(|(cmd, subcmd, flags)| {
        let matches = if subcmd.is_empty() {
            // Single-level command (e.g., "test")
            args.get(1).map(|s| s == *cmd).unwrap_or(false)
        } else {
            // Two-level command (e.g., "component set")
            args.get(1).map(|s| s == *cmd).unwrap_or(false)
                && args.get(2).map(|s| s == *subcmd).unwrap_or(false)
        };
        if matches {
            Some(*flags)
        } else {
            None
        }
    });

    let Some(known_flags) = known_flags else {
        return args;
    };

    let mut result = Vec::new();
    let mut found_separator = false;
    let mut insert_position: Option<usize> = None;

    for (i, arg) in args.iter().enumerate() {
        if arg == "--" {
            found_separator = true;
        }
        // Look for unknown flags
        if !found_separator
            && arg.starts_with("--")
            && !known_flags.contains(&arg.as_str())
            && !known_flags
                .iter()
                .any(|f| arg.starts_with(&format!("{}=", f)))
            && insert_position.is_none()
        {
            insert_position = Some(i);
        }
        result.push(arg.clone());
    }

    if let Some(pos) = insert_position {
        result.insert(pos, "--".to_string());
    }

    result
}

/// Apply all argument normalizations in sequence.
pub fn normalize(args: Vec<String>) -> Vec<String> {
    let args = normalize_version_show(args);
    let args = normalize_changelog_component(args);
    normalize_trailing_flags(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_component_set_inserts_separator() {
        let args = vec![
            "homeboy".into(),
            "component".into(),
            "set".into(),
            "my-plugin".into(),
            "--changelog_target".into(),
            "docs/CHANGELOG.md".into(),
        ];
        let result = normalize_trailing_flags(args);
        assert_eq!(result[4], "--");
        assert_eq!(result[5], "--changelog_target");
    }

    #[test]
    fn test_component_set_preserves_existing_separator() {
        let args = vec![
            "homeboy".into(),
            "component".into(),
            "set".into(),
            "my-plugin".into(),
            "--".into(),
            "--changelog_target".into(),
            "docs/CHANGELOG.md".into(),
        ];
        let result = normalize_trailing_flags(args.clone());
        assert_eq!(result, args);
    }

    #[test]
    fn test_component_set_allows_known_flags() {
        let args = vec![
            "homeboy".into(),
            "component".into(),
            "set".into(),
            "my-plugin".into(),
            "--json".into(),
            "{}".into(),
        ];
        let result = normalize_trailing_flags(args.clone());
        assert_eq!(result, args); // No separator inserted
    }

    #[test]
    fn test_test_command_inserts_separator() {
        let args = vec![
            "homeboy".into(),
            "test".into(),
            "my-component".into(),
            "--verbose".into(),
            "--filter=test_foo".into(),
        ];
        let result = normalize_trailing_flags(args);
        assert_eq!(result[3], "--");
        assert_eq!(result[4], "--verbose");
    }

    #[test]
    fn test_test_command_allows_known_flags() {
        let args = vec![
            "homeboy".into(),
            "test".into(),
            "my-component".into(),
            "--skip-lint".into(),
        ];
        let result = normalize_trailing_flags(args.clone());
        assert_eq!(result, args); // No separator inserted
    }

    #[test]
    fn test_test_command_allows_path_flag() {
        let args = vec![
            "homeboy".into(),
            "test".into(),
            "my-component".into(),
            "--path".into(),
            "/tmp/workspace/my-component".into(),
        ];
        let result = normalize_trailing_flags(args.clone());
        assert_eq!(result, args); // No separator inserted
    }

    #[test]
    fn test_test_command_allows_path_flag_equals_syntax() {
        let args = vec![
            "homeboy".into(),
            "test".into(),
            "my-component".into(),
            "--path=/tmp/workspace/my-component".into(),
        ];
        let result = normalize_trailing_flags(args.clone());
        assert_eq!(result, args); // No separator inserted
    }

    #[test]
    fn test_test_command_allows_fix_flag() {
        let args = vec![
            "homeboy".into(),
            "test".into(),
            "my-component".into(),
            "--fix".into(),
        ];
        let result = normalize_trailing_flags(args.clone());
        assert_eq!(result, args); // No separator inserted
    }

    #[test]
    fn test_test_command_allows_all_known_flags_combined() {
        let args = vec![
            "homeboy".into(),
            "test".into(),
            "my-component".into(),
            "--skip-lint".into(),
            "--fix".into(),
            "--path".into(),
            "/tmp/workspace".into(),
            "--setting".into(),
            "key=value".into(),
        ];
        let result = normalize_trailing_flags(args.clone());
        assert_eq!(result, args); // No separator inserted
    }

    #[test]
    fn test_docs_audit_allows_path_flag() {
        let args = vec![
            "homeboy".into(),
            "docs".into(),
            "audit".into(),
            "homeboy".into(),
            "--path".into(),
            "/tmp/workspace/homeboy".into(),
        ];
        let result = normalize_trailing_flags(args.clone());
        assert_eq!(result, args); // No separator inserted
    }

    #[test]
    fn test_lint_allows_baseline_flags() {
        let args = vec![
            "homeboy".into(),
            "lint".into(),
            "homeboy".into(),
            "--baseline".into(),
        ];
        let result = normalize_trailing_flags(args.clone());
        assert_eq!(result, args);
    }

    #[test]
    fn test_syntax_agnostic_both_styles_equivalent() {
        // Style 1: Without separator (auto-inserted)
        let without_sep = vec![
            "homeboy".into(),
            "component".into(),
            "set".into(),
            "my-plugin".into(),
            "--field".into(),
            "value".into(),
        ];

        // Style 2: With explicit separator (preserved)
        let with_sep = vec![
            "homeboy".into(),
            "component".into(),
            "set".into(),
            "my-plugin".into(),
            "--".into(),
            "--field".into(),
            "value".into(),
        ];

        // Both normalize to the same result
        let result1 = normalize_trailing_flags(without_sep);
        let result2 = normalize_trailing_flags(with_sep.clone());

        assert_eq!(result1, with_sep);
        assert_eq!(result2, with_sep);
    }

    #[test]
    fn test_version_show_normalization() {
        let args = vec!["homeboy".into(), "version".into(), "my-plugin".into()];
        let result = normalize_version_show(args);
        assert_eq!(result, vec!["homeboy", "version", "show", "my-plugin"]);
    }

    #[test]
    fn test_version_show_preserves_subcommand() {
        let args = vec![
            "homeboy".into(),
            "version".into(),
            "show".into(),
            "my-plugin".into(),
        ];
        let result = normalize_version_show(args.clone());
        assert_eq!(result, args);
    }

    #[test]
    fn test_full_normalize_pipeline() {
        // Test that normalize() chains all normalizations correctly
        let args = vec![
            "homeboy".into(),
            "component".into(),
            "set".into(),
            "my-plugin".into(),
            "--extract_command".into(),
            "unzip -o artifact.zip".into(),
        ];
        let result = normalize(args);
        assert_eq!(result[4], "--");
        assert_eq!(result[5], "--extract_command");
    }

    #[test]
    fn test_changelog_component_flag_to_positional() {
        let args = vec![
            "homeboy".into(),
            "changelog".into(),
            "add".into(),
            "--component".into(),
            "my-plugin".into(),
            "-m".into(),
            "message".into(),
        ];
        let result = normalize_changelog_component(args);
        assert_eq!(
            result,
            vec!["homeboy", "changelog", "add", "my-plugin", "-m", "message"]
        );
    }

    #[test]
    fn test_changelog_positional_unchanged() {
        let args = vec![
            "homeboy".into(),
            "changelog".into(),
            "add".into(),
            "my-plugin".into(),
            "-m".into(),
            "message".into(),
        ];
        let result = normalize_changelog_component(args.clone());
        assert_eq!(result, args);
    }
}
