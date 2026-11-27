//! Tests for the rename engine.

use super::*;

#[test]
fn pluralize_applies_english_suffix_rules() {
    for (singular, plural) in [
        ("widget", "widgets"),
        ("ability", "abilities"),
        ("query", "queries"),
        ("class", "classes"),
        ("key", "keys"),
        ("day", "days"),
    ] {
        assert_eq!(pluralize(singular), plural, "pluralize({singular:?})");
    }
}

#[test]
fn normal_rename_matches_branded_display_string_without_literal() {
    let dir = std::env::temp_dir().join("homeboy_refactor_branded_display_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    std::fs::write(
        dir.join("test.md"),
        "# Figma to WordPress\n\nFigma to WordPress workshop\n",
    )
    .unwrap();

    let spec = RenameSpec::new(
        "Figma to WordPress",
        "Figma to WordPress Studio",
        RenameScope::All,
    );
    assert!(!spec.literal);

    let mut result = generate_renames(&spec, &dir);
    assert_eq!(result.edits.len(), 1);
    assert!(result.references.iter().any(|reference| {
        reference.matched == "Figma to WordPress"
            && reference.replacement == "Figma to WordPress Studio"
    }));

    apply_renames(&mut result, &dir).unwrap();
    let content = std::fs::read_to_string(dir.join("test.md")).unwrap();
    assert_eq!(
        content,
        "# Figma to WordPress Studio\n\nFigma to WordPress Studio workshop\n"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn generate_renames_detects_file_renames() {
    let dir = std::env::temp_dir().join("homeboy_refactor_file_rename_test");
    let sub = dir.join("widget");
    let _ = std::fs::create_dir_all(&sub);

    std::fs::write(sub.join("widget.rs"), "fn widget_init() {}\n").unwrap();

    let spec = RenameSpec::new("widget", "gadget", RenameScope::All);
    let result = generate_renames(&spec, &dir);

    assert!(!result.file_renames.is_empty());
    // Should want to rename widget/widget.rs → gadget/gadget.rs
    let rename = result
        .file_renames
        .iter()
        .find(|r| r.from.contains("widget.rs"))
        .unwrap();
    assert!(rename.to.contains("gadget.rs"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn path_renames_include_skipped_content_files_in_renamed_directory() {
    let dir = std::env::temp_dir().join("homeboy_refactor_skipped_path_move_test");
    let _ = std::fs::remove_dir_all(&dir);
    let old_dir = dir.join("plugins/host/studio-native/build/assets/preview");
    std::fs::create_dir_all(&old_dir).unwrap();

    std::fs::write(old_dir.join("studio-native-preview.png"), b"png bytes").unwrap();
    std::fs::write(old_dir.join("preview.js.map"), b"{\"version\":3}").unwrap();
    std::fs::write(
        dir.join("plugins/host/studio-native/.npmrc"),
        b"legacy-peer-deps=true\n",
    )
    .unwrap();
    std::fs::write(
        old_dir.join("studio-native-preview.js"),
        "studioNative();\n",
    )
    .unwrap();

    let spec = RenameSpec::literal("studio-native", "wp-build", RenameScope::All);
    let mut result = generate_renames(&spec, &dir);

    assert!(result.file_renames.iter().any(|rename| {
        rename.from == "plugins/host/studio-native/build/assets/preview/studio-native-preview.png"
            && rename.to == "plugins/host/wp-build/build/assets/preview/wp-build-preview.png"
    }));
    assert!(result.file_renames.iter().any(|rename| {
        rename.from == "plugins/host/studio-native/build/assets/preview/preview.js.map"
            && rename.to == "plugins/host/wp-build/build/assets/preview/preview.js.map"
    }));
    assert!(result.file_renames.iter().any(|rename| {
        rename.from == "plugins/host/studio-native/.npmrc"
            && rename.to == "plugins/host/wp-build/.npmrc"
    }));

    apply_renames(&mut result, &dir).unwrap();

    assert!(dir
        .join("plugins/host/wp-build/build/assets/preview/wp-build-preview.png")
        .exists());
    assert!(dir
        .join("plugins/host/wp-build/build/assets/preview/preview.js.map")
        .exists());
    assert!(dir.join("plugins/host/wp-build/.npmrc").exists());
    assert!(!dir
        .join("plugins/host/studio-native/build/assets/preview/studio-native-preview.png")
        .exists());
    assert!(!dir
        .join("plugins/host/studio-native/build/assets/preview/preview.js.map")
        .exists());
    assert!(!dir.join("plugins/host/studio-native/.npmrc").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn repo_config_dotfiles_are_content_edited() {
    let dir = std::env::temp_dir().join("homeboy_refactor_repo_config_dotfile_edit_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    std::fs::write(
        dir.join(".gitignore"),
        "plugins/host/studio-native/build/\nplugins/host/studio-native/assets/\n",
    )
    .unwrap();

    let spec = RenameSpec::literal("studio-native", "wp-build", RenameScope::All);
    let mut result = generate_renames(&spec, &dir);

    let edit = result
        .edits
        .iter()
        .find(|edit| edit.file == ".gitignore")
        .expect(".gitignore should be content-edit eligible");
    assert!(edit.new_content.contains("plugins/host/wp-build/build/"));
    assert!(!edit.new_content.contains("studio-native"));

    apply_renames(&mut result, &dir).unwrap();

    let content = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
    assert_eq!(
        content,
        "plugins/host/wp-build/build/\nplugins/host/wp-build/assets/\n"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn path_renames_prune_emptied_source_directories() {
    let dir = std::env::temp_dir().join("homeboy_refactor_prune_empty_source_dirs_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("plugins/host/studio-native/assets")).unwrap();
    std::fs::create_dir_all(dir.join("plugins/host/studio-native/build/js")).unwrap();

    std::fs::write(
        dir.join("plugins/host/studio-native/assets/studio-native-logo.svg"),
        "<svg></svg>\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("plugins/host/studio-native/build/js/app.js"),
        "console.log('ok');\n",
    )
    .unwrap();

    let spec = RenameSpec::literal("studio-native", "wp-build", RenameScope::All);
    let mut result = generate_renames(&spec, &dir);

    apply_renames(&mut result, &dir).unwrap();

    assert!(dir
        .join("plugins/host/wp-build/assets/wp-build-logo.svg")
        .exists());
    assert!(dir.join("plugins/host/wp-build/build/js/app.js").exists());
    assert!(!dir.join("plugins/host/studio-native/assets").exists());
    assert!(!dir.join("plugins/host/studio-native/build/js").exists());
    assert!(!dir.join("plugins/host/studio-native/build").exists());
    assert!(!dir.join("plugins/host/studio-native").exists());
    assert!(dir.join("plugins/host").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn path_renames_preserve_intermediate_segments_under_renamed_ancestor() {
    let dir = std::env::temp_dir().join("homeboy_refactor_nested_path_segment_test");
    let _ = std::fs::remove_dir_all(&dir);
    let dist = dir.join("plugins/host/studio-native/modules/agentic-ui-block/dist");
    std::fs::create_dir_all(&dist).unwrap();

    std::fs::write(
        dist.join("studio-native-agentic-ui-block.js"),
        "studioNativeAgenticUiBlock();\n",
    )
    .unwrap();

    let spec = RenameSpec::literal("studio-native", "wp-build", RenameScope::All);
    let mut result = generate_renames(&spec, &dir);

    let rename = result
        .file_renames
        .iter()
        .find(|rename| rename.from.ends_with("studio-native-agentic-ui-block.js"))
        .unwrap();
    assert_eq!(
        rename.from,
        "plugins/host/studio-native/modules/agentic-ui-block/dist/studio-native-agentic-ui-block.js"
    );
    assert_eq!(
        rename.to,
        "plugins/host/wp-build/modules/agentic-ui-block/dist/wp-build-agentic-ui-block.js"
    );

    apply_renames(&mut result, &dir).unwrap();

    assert!(dir
        .join("plugins/host/wp-build/modules/agentic-ui-block/dist/wp-build-agentic-ui-block.js")
        .exists());
    assert!(!dir
        .join("plugins/host/wp-build/modules/agentic-ui-block/wp-build-agentic-ui-block.js")
        .exists());
    assert!(!dir
        .join("plugins/host/studio-native/modules/agentic-ui-block/dist/studio-native-agentic-ui-block.js")
        .exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn apply_renames_writes_to_disk() {
    let dir = std::env::temp_dir().join("homeboy_refactor_apply_test");
    let _ = std::fs::create_dir_all(&dir);

    std::fs::write(dir.join("test.rs"), "pub mod widget;\n").unwrap();

    let spec = RenameSpec::new("widget", "gadget", RenameScope::All);
    let mut result = generate_renames(&spec, &dir);

    apply_renames(&mut result, &dir).unwrap();
    assert!(result.applied);

    let content = std::fs::read_to_string(dir.join("test.rs")).unwrap();
    assert_eq!(content, "pub mod gadget;\n");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn snake_case_rename_in_file() {
    let dir = std::env::temp_dir().join("homeboy_refactor_snake_test");
    let _ = std::fs::create_dir_all(&dir);

    std::fs::write(
        dir.join("test.rs"),
        "fn load_widget() {}\nfn is_widget_linked() -> bool { true }\nconst WIDGET_DIR: &str = \"widgets\";\n",
    )
    .unwrap();

    let spec = RenameSpec::new("widget", "gadget", RenameScope::All);
    let result = generate_renames(&spec, &dir);

    assert!(!result.edits.is_empty());
    let content = &result.edits[0].new_content;
    assert!(
        content.contains("load_gadget"),
        "Expected 'load_gadget' in:\n{}",
        content
    );
    assert!(
        content.contains("is_gadget_linked"),
        "Expected 'is_gadget_linked' in:\n{}",
        content
    );
    assert!(
        content.contains("GADGET_DIR"),
        "Expected 'GADGET_DIR' in:\n{}",
        content
    );
    assert!(
        content.contains("\"gadgets\""),
        "Expected '\"gadgets\"' in:\n{}",
        content
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn extract_field_identifier_works() {
    assert_eq!(
        extract_field_identifier("pub name: String,"),
        Some("name".to_string())
    );
    assert_eq!(
        extract_field_identifier("pub(crate) id: u32,"),
        Some("id".to_string())
    );
    assert_eq!(
        extract_field_identifier("count: usize,"),
        Some("count".to_string())
    );
    assert_eq!(
        extract_field_identifier("let value = 42;"),
        Some("value".to_string())
    );
    assert_eq!(
        extract_field_identifier("fn init("),
        Some("init".to_string())
    );
    assert_eq!(extract_field_identifier("if (enabled) {"), None);
    assert_eq!(extract_field_identifier("// a comment"), None);
    assert_eq!(extract_field_identifier("#[serde(skip)]"), None);
    assert_eq!(extract_field_identifier(""), None);
}

#[test]
fn detect_duplicate_identifiers_catches_collision() {
    let content = "struct Foo {\n    pub name: String,\n    pub name: u32,\n}\n";
    let mut warnings = Vec::new();
    detect_duplicate_identifiers("test.rs", content, &mut warnings);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].kind, "duplicate_identifier");
    assert!(warnings[0].message.contains("name"));
}

#[test]
fn detect_duplicate_identifiers_ignores_distinct_scopes_and_keywords() {
    let cases = [
        (
            "test.rs",
            "struct Foo {\n    pub name: String,\n    pub age: u32,\n}\n",
        ),
        (
            "workflow.yml",
            "function build() {\n  tasks.set(smoke.id, {\nid: smoke.id,\npromptFile: smoke.prompt,\n  });\n  tasks.set(scenario.id, {\nid: scenario.id,\npromptFile: scenario.prompt,\n  });\n}\n",
        ),
        (
            "test.js",
            "const tasks = [\n    {\n        promptFile: 'a.md',\n    },\n    {\n        promptFile: 'b.md',\n    },\n];\n",
        ),
        (
            "test.ts",
            "function run(items) {\n    if (items.length === 0) {\n        return;\n    }\n    if (items.length > 1) {\n        return;\n    }\n}\n",
        ),
    ];
    for (file, content) in cases {
        let mut warnings = Vec::new();
        detect_duplicate_identifiers(file, content, &mut warnings);
        assert!(warnings.is_empty(), "{file}: unexpected {warnings:?}");
    }
}

#[test]
fn collision_detection_file_rename_target_exists() {
    let dir = std::env::temp_dir().join("homeboy_collision_file_test");
    let _ = std::fs::create_dir_all(&dir);

    // Create both source and target files
    std::fs::write(dir.join("old.rs"), "fn old() {}\n").unwrap();
    std::fs::write(dir.join("new.rs"), "fn new() {}\n").unwrap();

    let file_renames = vec![FileRename {
        from: "old.rs".to_string(),
        to: "new.rs".to_string(),
    }];

    let warnings = detect_collisions(&[], &file_renames, &dir);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].kind, "file_collision");
    assert!(warnings[0].message.contains("new.rs"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn collision_detection_in_generate_renames() {
    let dir = std::env::temp_dir().join("homeboy_collision_gen_test");
    let _ = std::fs::create_dir_all(&dir);

    // This simulates the exact #284 bug: struct has both `widgets` and `gadgets` fields,
    // and renaming widget → gadget would create two `gadgets` fields.
    std::fs::write(
        dir.join("test.rs"),
        "struct Config {\n    pub widgets: Vec<String>,\n    pub gadgets: Vec<u32>,\n}\n",
    )
    .unwrap();

    let spec = RenameSpec::new("widget", "gadget", RenameScope::All);
    let result = generate_renames(&spec, &dir);

    assert!(
        !result.warnings.is_empty(),
        "Should detect duplicate 'gadgets' field"
    );
    assert!(result
        .warnings
        .iter()
        .any(|w| w.kind == "duplicate_identifier"));
    assert!(result
        .warnings
        .iter()
        .any(|w| w.message.contains("gadgets")));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn root_build_and_artifacts_dirs_are_skipped_but_nested_build_is_scanned() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for sub in ["build", "artifacts", "scripts/build", "src"] {
        std::fs::create_dir_all(root.join(sub)).unwrap();
    }
    std::fs::write(root.join("build/output.rs"), "let widget = true;\n").unwrap();
    std::fs::write(root.join("artifacts/output.rs"), "let widget = true;\n").unwrap();
    std::fs::write(
        root.join("scripts/build/setup.sh"),
        "WIDGET_PATH=\"$HOME\"\n",
    )
    .unwrap();
    std::fs::write(root.join("src/lib.rs"), "fn widget() {}\n").unwrap();

    let spec = RenameSpec::new("widget", "gadget", RenameScope::All);
    let mut files: Vec<String> = generate_renames(&spec, root)
        .edits
        .into_iter()
        .map(|edit| edit.file)
        .collect();
    files.sort();
    assert_eq!(files, vec!["scripts/build/setup.sh", "src/lib.rs"]);
}

// ====================================================================
// Literal mode tests
// ====================================================================

#[test]
fn literal_mode_finds_references_in_file() {
    let dir = std::env::temp_dir().join("homeboy_refactor_literal_refs_test");
    let _ = std::fs::create_dir_all(&dir);

    std::fs::write(
        dir.join("plugin.php"),
        "// Plugin: sampleplugin-events\ndefine('SAMPLEPLUGIN_EVENTS_VERSION', '1.0');\nfunction sampleplugin_events_init() {}\n",
    )
    .unwrap();

    // Literal mode: only exact match, no case variants
    let spec = RenameSpec::literal(
        "sampleplugin-events",
        "sample-plugin-events",
        RenameScope::All,
    );
    let refs = find_references(&spec, &dir);

    // Should find only the hyphenated form, not SAMPLEPLUGIN_EVENTS or sampleplugin_events
    assert_eq!(
        refs.len(),
        1,
        "Should find exactly 1 literal match, got: {:?}",
        refs.iter().map(|r| &r.matched).collect::<Vec<_>>()
    );
    assert_eq!(refs[0].matched, "sampleplugin-events");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn literal_mode_generates_and_applies_edits() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("plugin.php"),
        "Text Domain: sampleplugin-events\nSlug: sampleplugin-events\n",
    )
    .unwrap();

    let spec = RenameSpec::literal(
        "sampleplugin-events",
        "sample-plugin-events",
        RenameScope::All,
    );
    let mut result = generate_renames(&spec, root);

    assert_eq!(result.edits.len(), 1);
    assert_eq!(result.edits[0].replacements, 2);

    apply_renames(&mut result, root).unwrap();
    assert!(result.applied);
    assert_eq!(
        std::fs::read_to_string(root.join("plugin.php")).unwrap(),
        "Text Domain: sample-plugin-events\nSlug: sample-plugin-events\n"
    );
}

#[test]
fn literal_mode_renames_files() {
    let dir = std::env::temp_dir().join("homeboy_refactor_literal_file_rename_test");
    let _ = std::fs::create_dir_all(&dir);

    std::fs::write(dir.join("sampleplugin-events.php"), "// main file\n").unwrap();

    let spec = RenameSpec::literal(
        "sampleplugin-events",
        "sample-plugin-events",
        RenameScope::All,
    );
    let result = generate_renames(&spec, &dir);

    assert!(
        !result.file_renames.is_empty(),
        "Should rename sampleplugin-events.php"
    );
    let rename = &result.file_renames[0];
    assert_eq!(rename.from, "sampleplugin-events.php");
    assert_eq!(rename.to, "sample-plugin-events.php");

    let _ = std::fs::remove_dir_all(&dir);
}

// ====================================================================
// Word splitting tests
// ====================================================================

#[test]
fn split_words_normalizes_every_naming_convention() {
    let cases: &[(&str, &[&str])] = &[
        ("wp-agent", &["wp", "agent"]),
        ("sample-plugin-agent", &["sample", "plugin", "agent"]),
        ("sample_plugin_agent", &["sample", "plugin", "agent"]),
        ("SAMPLE_PLUGIN_AGENT", &["sample", "plugin", "agent"]),
        ("SamplePluginAgent", &["sample", "plugin", "agent"]),
        ("samplePluginAgent", &["sample", "plugin", "agent"]),
        ("Sample Plugin Agent", &["sample", "plugin", "agent"]),
        // consecutive uppercase is an acronym followed by a word
        ("WPAgent", &["wp", "agent"]),
        ("XMLParser", &["xml", "parser"]),
        ("HTTPClient", &["http", "client"]),
        // all-uppercase stays one word
        ("HTTP", &["http"]),
        ("Widget", &["widget"]),
        ("WIDGET", &["widget"]),
    ];
    for (term, expected) in cases {
        assert_eq!(
            split_words(term),
            expected.to_vec(),
            "split_words({term:?})"
        );
    }
}

// ====================================================================
// Cross-separator variant generation tests
// ====================================================================

#[test]
fn cross_separator_variants_from_kebab() {
    let spec = RenameSpec::new("wp-agent", "sample-plugin-agent", RenameScope::All);
    let from_values: Vec<&str> = spec.variants.iter().map(|v| v.from.as_str()).collect();
    let to_values: Vec<&str> = spec.variants.iter().map(|v| v.to.as_str()).collect();

    // Singular forms — all naming conventions
    assert!(from_values.contains(&"wp-agent"), "Missing kebab from");
    assert!(
        from_values.contains(&"Wp-Agent"),
        "Missing Title-Kebab-Case from"
    );
    assert!(from_values.contains(&"wp_agent"), "Missing snake from");
    assert!(
        from_values.contains(&"WP_AGENT"),
        "Missing UPPER_SNAKE from"
    );
    assert!(
        from_values.contains(&"Wp_Agent"),
        "Missing Title_Snake_Case from"
    );
    assert!(from_values.contains(&"WpAgent"), "Missing PascalCase from");
    assert!(from_values.contains(&"wpAgent"), "Missing camelCase from");
    assert!(from_values.contains(&"Wp Agent"), "Missing display from");

    assert!(
        to_values.contains(&"sample-plugin-agent"),
        "Missing kebab to"
    );
    assert!(
        to_values.contains(&"Sample-Plugin-Agent"),
        "Missing Title-Kebab-Case to"
    );
    assert!(
        to_values.contains(&"sample_plugin_agent"),
        "Missing snake to"
    );
    assert!(
        to_values.contains(&"SAMPLE_PLUGIN_AGENT"),
        "Missing UPPER_SNAKE to"
    );
    assert!(
        to_values.contains(&"Sample_Plugin_Agent"),
        "Missing Title_Snake_Case to"
    );
    assert!(
        to_values.contains(&"SamplePluginAgent"),
        "Missing PascalCase to"
    );
    assert!(
        to_values.contains(&"samplePluginAgent"),
        "Missing camelCase to"
    );
    assert!(
        to_values.contains(&"Sample Plugin Agent"),
        "Missing display to"
    );

    // Plural forms
    assert!(
        from_values.contains(&"wp-agents"),
        "Missing plural kebab from"
    );
    assert!(
        from_values.contains(&"Wp-Agents"),
        "Missing plural Title-Kebab-Case from"
    );
    assert!(
        from_values.contains(&"wp_agents"),
        "Missing plural snake from"
    );
    assert!(
        from_values.contains(&"WP_AGENTS"),
        "Missing plural UPPER_SNAKE from"
    );
    assert!(
        from_values.contains(&"Wp_Agents"),
        "Missing plural Title_Snake_Case from"
    );
    assert!(
        from_values.contains(&"WpAgents"),
        "Missing plural PascalCase from"
    );
}

#[test]
fn cross_separator_variants_are_independent_of_input_convention() {
    let from_set = |from: &str, to: &str| -> std::collections::BTreeSet<String> {
        RenameSpec::new(from, to, RenameScope::All)
            .variants
            .iter()
            .map(|v| v.from.clone())
            .collect()
    };
    let kebab = from_set("wp-agent", "sample-plugin-agent");
    for (from, to) in [
        ("WpAgent", "SamplePluginAgent"),
        ("wp_agent", "sample_plugin_agent"),
    ] {
        let set = from_set(from, to);
        for expected in [
            "wp-agent", "Wp-Agent", "wp_agent", "WP_AGENT", "WpAgent", "wpAgent",
        ] {
            assert!(kebab.contains(expected), "kebab input missing {expected}");
            assert!(set.contains(expected), "{from} input missing {expected}");
        }
    }
}

#[test]
fn single_word_variants_cover_case_and_plural_without_duplicates() {
    let spec = RenameSpec::new("widget", "gadget", RenameScope::All);
    let pairs: Vec<(&str, &str)> = spec
        .variants
        .iter()
        .map(|v| (v.from.as_str(), v.to.as_str()))
        .collect();
    for expected in [
        ("widget", "gadget"),
        ("Widget", "Gadget"),
        ("WIDGET", "GADGET"),
        ("widgets", "gadgets"),
        ("Widgets", "Gadgets"),
        ("WIDGETS", "GADGETS"),
    ] {
        assert!(
            pairs.contains(&expected),
            "missing {expected:?} in {pairs:?}"
        );
    }

    let mut seen = std::collections::HashSet::new();
    for v in &spec.variants {
        assert!(seen.insert(&v.from), "Duplicate variant 'from': {}", v.from);
    }
}

#[test]
fn cross_separator_end_to_end_rename() {
    // The real use case: rename wp-agent → sample-plugin-agent across all conventions
    let dir = std::env::temp_dir().join("homeboy_cross_sep_e2e_test");
    let _ = std::fs::create_dir_all(&dir);

    std::fs::write(
        dir.join("plugin.php"),
        concat!(
            "// Plugin: wp-agent\n",
            "namespace WpAgent;\n",
            "define('WP_AGENT_VERSION', '1.0');\n",
            "function wp_agent_init() {}\n",
            "// slug: wp-agents\n",
        ),
    )
    .unwrap();

    let spec = RenameSpec::new("wp-agent", "sample-plugin-agent", RenameScope::All);
    let result = generate_renames(&spec, &dir);

    assert!(!result.edits.is_empty(), "Should have edits");
    let content = &result.edits[0].new_content;

    assert!(
        content.contains("sample-plugin-agent"),
        "Should rename kebab: {}",
        content
    );
    assert!(
        content.contains("SamplePluginAgent"),
        "Should rename PascalCase: {}",
        content
    );
    assert!(
        content.contains("SAMPLE_PLUGIN_AGENT_VERSION"),
        "Should rename UPPER_SNAKE: {}",
        content
    );
    assert!(
        content.contains("sample_plugin_agent_init"),
        "Should rename snake_case: {}",
        content
    );
    assert!(
        content.contains("sample-plugin-agents"),
        "Should rename plural kebab: {}",
        content
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn title_case_variants_rename_and_explicit_variants_take_precedence() {
    let class = "final class Studio_Native_Codebox_Client {}\n";
    let doc = "// Studio-Native-shaped site_spec\n";
    let explicit = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(from, to)| (from.to_string(), to.to_string()))
            .collect()
    };
    // (content, explicit variants, expected output)
    let cases: Vec<(&str, Vec<(String, String)>, &str)> = vec![
        (class, vec![], "final class Wp_Build_Codebox_Client {}\n"),
        (doc, vec![], "// Wp-Build-shaped site_spec\n"),
        (
            doc,
            explicit(&[("Studio-Native", "WordPress-Build")]),
            "// WordPress-Build-shaped site_spec\n",
        ),
        (
            class,
            explicit(&[("Studio_Native", "WPBuild")]),
            "final class WPBuild_Codebox_Client {}\n",
        ),
        (
            "fn studio_native_gate() {}\n",
            explicit(&[("studio_native", "wpbuild")]),
            "fn wpbuild_gate() {}\n",
        ),
    ];

    for (content, variants, expected) in cases {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plugin.php"), content).unwrap();
        let spec = RenameSpec::new("studio-native", "wp-build", RenameScope::All)
            .with_explicit_variants(variants.clone());
        let result = generate_renames(&spec, dir.path());
        assert_eq!(result.edits.len(), 1, "{content:?} with {variants:?}");
        assert_eq!(
            result.edits[0].new_content, expected,
            "{content:?} with {variants:?}"
        );
    }
}

#[test]
fn explicit_variants_handle_project_specific_acronyms() {
    let dir = std::env::temp_dir().join("homeboy_explicit_variant_acronym_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    std::fs::write(
        dir.join("plugin.php"),
        concat!(
            "// Display: Old API Client\n",
            "final class Old_API_Client {}\n"
        ),
    )
    .unwrap();

    let spec = RenameSpec::new("old-client", "new-client", RenameScope::All)
        .with_explicit_variants(vec![
            ("Old API Client".to_string(), "New API Client".to_string()),
            ("Old_API_Client".to_string(), "New_API_Client".to_string()),
        ]);

    let result = generate_renames(&spec, &dir);
    assert_eq!(result.edits.len(), 1);
    let content = &result.edits[0].new_content;

    assert!(content.contains("New API Client"), "{}", content);
    assert!(content.contains("New_API_Client"), "{}", content);
    assert!(!content.contains("Old API Client"), "{}", content);
    assert!(!content.contains("Old_API_Client"), "{}", content);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn include_glob_limits_edits_to_targeted_files() {
    let dir = std::env::temp_dir().join("homeboy_refactor_target_include_test");
    let _ = std::fs::create_dir_all(dir.join("src"));
    let _ = std::fs::create_dir_all(dir.join("tests"));

    std::fs::write(dir.join("src/lib.rs"), "fn mark_item_processed() {}\n").unwrap();
    std::fs::write(
        dir.join("tests/lib_test.rs"),
        "fn test_mark_item_processed() {}\n",
    )
    .unwrap();

    let spec = RenameSpec::new(
        "mark_item_processed",
        "add_processed_item",
        RenameScope::All,
    );
    let targeting = RenameTargeting {
        include_globs: vec!["tests/**/*.rs".to_string()],
        ..RenameTargeting::default()
    };

    let result = generate_renames_with_targeting(&spec, &dir, &targeting);

    assert_eq!(result.edits.len(), 1, "Should only edit tests files");
    assert_eq!(result.edits[0].file, "tests/lib_test.rs");
    assert!(result.edits[0].new_content.contains("add_processed_item"));
    assert!(!result.edits[0].new_content.contains("mark_item_processed"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exclude_glob_omits_matching_files() {
    let dir = std::env::temp_dir().join("homeboy_refactor_target_exclude_test");
    let _ = std::fs::create_dir_all(dir.join("src"));
    let _ = std::fs::create_dir_all(dir.join("tests"));

    std::fs::write(dir.join("src/lib.rs"), "fn mark_item_processed() {}\n").unwrap();
    std::fs::write(
        dir.join("tests/lib_test.rs"),
        "fn test_mark_item_processed() {}\n",
    )
    .unwrap();

    let spec = RenameSpec::new(
        "mark_item_processed",
        "add_processed_item",
        RenameScope::All,
    );
    let targeting = RenameTargeting {
        exclude_globs: vec!["src/**/*.rs".to_string()],
        ..RenameTargeting::default()
    };

    let result = generate_renames_with_targeting(&spec, &dir, &targeting);

    assert_eq!(result.edits.len(), 1, "Should skip excluded src files");
    assert_eq!(result.edits[0].file, "tests/lib_test.rs");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_file_renames_suppresses_path_renames() {
    let dir = std::env::temp_dir().join("homeboy_refactor_no_file_rename_test");
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("mark_item_processed_test.rs"), "fn ok() {}\n").unwrap();

    let spec = RenameSpec::new(
        "mark_item_processed",
        "add_processed_item",
        RenameScope::All,
    );
    let targeting = RenameTargeting {
        rename_files: false,
        ..RenameTargeting::default()
    };

    let result = generate_renames_with_targeting(&spec, &dir, &targeting);
    assert!(
        result.file_renames.is_empty(),
        "File renames should be disabled when rename_files=false"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn context_key_filters_to_string_keys_only() {
    let dir = std::env::temp_dir().join("homeboy_context_key_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let content = r#"
$handler_config = [];
$input['handler_config'] = 'value';
function foo($handler_config) {}
echo "handler_config";
$obj->handler_config = true;
"#;
    std::fs::write(dir.join("test.php"), content).unwrap();

    let mut spec = RenameSpec::literal("handler_config", "handler_configs", RenameScope::All);
    spec.rename_context = RenameContext::Key;

    let refs = find_references_with_targeting(&spec, &dir, &RenameTargeting::default());

    // Should match: 'handler_config' (string key), "handler_config" (string),
    // ->handler_config (property access)
    // Should NOT match: $handler_config (variable), $handler_config (parameter)
    let matched_lines: Vec<usize> = refs.iter().map(|r| r.line).collect();
    assert!(
        !matched_lines.contains(&2), // $handler_config = [] — variable
        "Should not match variable assignment, got matches at lines: {:?}",
        matched_lines
    );
    assert!(
        matched_lines.contains(&3), // $input['handler_config'] — string key
        "Should match string key, got matches at lines: {:?}",
        matched_lines
    );
    assert!(
        !matched_lines.contains(&4), // function foo($handler_config) — parameter
        "Should not match function parameter, got matches at lines: {:?}",
        matched_lines
    );
    assert!(
        matched_lines.contains(&5), // "handler_config" — string
        "Should match string literal, got matches at lines: {:?}",
        matched_lines
    );
    assert!(
        matched_lines.contains(&6), // ->handler_config — property access
        "Should match property access, got matches at lines: {:?}",
        matched_lines
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn context_variable_filters_to_variables_only() {
    let dir = std::env::temp_dir().join("homeboy_context_var_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let content = r#"
$handler_config = [];
$input['handler_config'] = 'value';
$obj->handler_config = true;
"#;
    std::fs::write(dir.join("test.php"), content).unwrap();

    let mut spec = RenameSpec::literal("handler_config", "handler_configs", RenameScope::All);
    spec.rename_context = RenameContext::Variable;

    let refs = find_references_with_targeting(&spec, &dir, &RenameTargeting::default());

    let matched_lines: Vec<usize> = refs.iter().map(|r| r.line).collect();
    assert!(
        matched_lines.contains(&2), // $handler_config — variable
        "Should match PHP variable, got matches at lines: {:?}",
        matched_lines
    );
    assert!(
        !matched_lines.contains(&3), // 'handler_config' — string key, not variable
        "Should not match string key, got matches at lines: {:?}",
        matched_lines
    );
    assert!(
        !matched_lines.contains(&4), // ->handler_config — property access, not variable
        "Should not match property access, got matches at lines: {:?}",
        matched_lines
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn failed_apply_rolls_back_every_edit_and_move() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/foo_bar.rs"), "pub fn foo_bar() {}\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), "mod foo_bar;\n").unwrap();
    std::fs::write(root.join("src/a_foo_bar.rs"), "// foo_bar\n").unwrap();

    let spec = RenameSpec::new("foo_bar", "baz_qux", RenameScope::All);
    let mut result = generate_renames(&spec, root);
    assert_eq!(result.file_renames.len(), 2);

    // A destination appears between planning and applying: the second move
    // fails after content edits and the first move have already landed.
    std::fs::write(root.join("src/baz_qux.rs"), "occupied\n").unwrap();

    let error = apply_renames(&mut result, root).unwrap_err();
    let detail = error.details["error"].as_str().unwrap_or_default();
    assert!(
        detail.contains("Destination already exists: src/baz_qux.rs")
            && detail.contains("all changes were rolled back"),
        "unexpected error detail: {detail}"
    );
    assert!(!result.applied);
    assert_eq!(
        std::fs::read_to_string(root.join("src/foo_bar.rs")).unwrap(),
        "pub fn foo_bar() {}\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
        "mod foo_bar;\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("src/a_foo_bar.rs")).unwrap(),
        "// foo_bar\n"
    );
    assert!(!root.join("src/a_baz_qux.rs").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("src/baz_qux.rs")).unwrap(),
        "occupied\n"
    );
}
