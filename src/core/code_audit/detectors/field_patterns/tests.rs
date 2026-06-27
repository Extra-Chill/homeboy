#![cfg(test)]

use super::*;

/// Representative multi-language scan settings for tests, mirroring what an
/// extension/component profile would declare.
fn test_scan() -> ResolvedFieldScan {
    ResolvedFieldScan {
        scan_tokens: vec![
            "rs".to_string(),
            "cls".to_string(),
            "ts".to_string(),
            "js".to_string(),
            "go".to_string(),
        ],
        type_before_name_tokens: vec!["cls".to_string()],
        inline_test_strip_tokens: vec!["rs".to_string()],
        test_file_suffixes: vec![
            "_test.rs".to_string(),
            "_test.cls".to_string(),
            ".test.ts".to_string(),
            ".test.js".to_string(),
        ],
    }
}

/// Build a shared-style codebase snapshot over the multi-language source
/// extensions these tests use, mirroring how the audit pipeline now feeds
/// the detector an already-walked `(path, content)` view.
fn snapshot_of(root: &std::path::Path) -> CodebaseSnapshot {
    use crate::core::engine::codebase_scan::{ExtensionFilter, ScanConfig};
    CodebaseSnapshot::build(
        root,
        &ScanConfig {
            extensions: ExtensionFilter::Only(vec![
                "rs".to_string(),
                "cls".to_string(),
                "ts".to_string(),
                "js".to_string(),
                "go".to_string(),
            ]),
            ..Default::default()
        },
    )
}

#[test]
fn test_run() {
    let dir = tempfile::tempdir().unwrap();
    // Empty profile (no tokens, builtin defaults on) → still empty on empty dir.
    assert!(run(&snapshot_of(dir.path()), &DetectorProfileConfig::default()).is_empty());
}

#[test]
fn run_is_inert_without_scan_tokens() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
            dir.path().join("a.rs"),
            "struct A { verbose: bool, quiet: bool }\nstruct B { verbose: bool, quiet: bool }\nstruct C { verbose: bool, quiet: bool }\n",
        )
        .unwrap();
    let config = DetectorProfileConfig {
        use_builtin_defaults: false,
        ..Default::default()
    };
    assert!(
        run(&snapshot_of(dir.path()), &config).is_empty(),
        "no scan tokens declared and builtin defaults off → inert"
    );
}

#[test]
fn extracts_rust_struct_fields() {
    let content = r#"
pub struct Config {
    pub verbose: bool,
    pub quiet: bool,
    pub output: Option<String>,
}
"#;
    let structs = extract_structs(content, "test.rs", FieldSyntax::NameBeforeType);
    assert_eq!(structs.len(), 1);
    assert_eq!(structs[0].name, "Config");
    assert_eq!(structs[0].fields.len(), 3);
    assert_eq!(structs[0].fields[0].name, "verbose");
    assert_eq!(structs[0].fields[0].field_type, "bool");
    assert_eq!(structs[0].fields[2].name, "output");
    assert_eq!(structs[0].fields[2].field_type, "Option<String>");
}

#[test]
fn extracts_multiple_structs() {
    let content = r#"
struct Alpha {
    x: i32,
    y: i32,
}

struct Beta {
    x: i32,
    y: i32,
    z: i32,
}
"#;
    let structs = extract_structs(content, "test.rs", FieldSyntax::NameBeforeType);
    assert_eq!(structs.len(), 2);
    assert_eq!(structs[0].name, "Alpha");
    assert_eq!(structs[1].name, "Beta");
}

#[test]
fn skips_methods_inside_struct() {
    let content = r#"
struct Foo {
    name: String,
}

impl Foo {
    fn new() -> Self {
        Self { name: String::new() }
    }
}
"#;
    let structs = extract_structs(content, "test.rs", FieldSyntax::NameBeforeType);
    assert_eq!(structs.len(), 1);
    assert_eq!(structs[0].fields.len(), 1);
    assert_eq!(structs[0].fields[0].name, "name");
}

#[test]
fn skips_call_arguments_inside_type_before_name_methods() {
    let content = r#"
class AIStep {
    public static function register(): void {
        self::registerStepType(
            class_name: self::class,
            label: 'AI',
        );
        add_filter('sampleplugin_handlers', [self::class, 'register']);
    }
}
"#;

    let structs = extract_structs(content, "test.cls", FieldSyntax::TypeBeforeName);
    assert!(
        structs.is_empty(),
        "call-site named arguments inside methods should not create field-bearing structs"
    );
}

#[test]
fn extracts_type_before_name_class_properties_without_named_arguments() {
    let content = r#"
class Config {
    public string $label;
    protected ?array $settings;

    public static function register(): void {
        self::registerStepType(
            label: 'Config',
            stepSettings: array(),
        );
    }
}
"#;

    let structs = extract_structs(content, "test.cls", FieldSyntax::TypeBeforeName);
    assert_eq!(structs.len(), 1);
    assert_eq!(structs[0].fields.len(), 2);
    assert_eq!(structs[0].fields[0].name, "label");
    assert_eq!(structs[0].fields[0].field_type, "string");
    assert_eq!(structs[0].fields[1].name, "settings");
    assert_eq!(structs[0].fields[1].field_type, "?array");
}

#[test]
fn does_not_report_repeated_type_before_name_presentation_or_command_call_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();

    for name in &["callback.cls", "authorize.cls", "webhook.cls"] {
        std::fs::write(
            src.join(name),
            format!(
                r#"
class {} {{
    public function render(): void {{
        $styles = array(
            'display' => 'flex',
            'background' => '#fff',
        );
        WP_CLI::log( 'Rendered' );
    }}
}}
"#,
                name.replace(".cls", "").to_uppercase()
            ),
        )
        .unwrap();
    }

    let findings = detect_repeated_field_patterns(&snapshot_of(dir.path()), &test_scan());
    assert!(
        findings.is_empty(),
        "presentation arrays and WP_CLI call sites are not extractable field groups: {:?}",
        findings
    );
}

#[test]
fn skips_field_shaped_syntax_inside_rust_methods() {
    let content = r#"
struct Foo {
    name: String,
}

impl Foo {
    fn new() -> Self {
        Self {
            name: String::new(),
            label: "nested literal",
        }
    }
}
"#;

    let structs = extract_structs(content, "test.rs", FieldSyntax::NameBeforeType);
    assert_eq!(structs.len(), 1);
    assert_eq!(structs[0].fields.len(), 1);
    assert_eq!(structs[0].fields[0].name, "name");
    assert_eq!(structs[0].fields[0].field_type, "String");
}

#[test]
fn skips_field_shaped_syntax_inside_typescript_methods() {
    let content = r#"
class Widget {
    name: string;

    build() {
        return {
            name: 'nested literal',
            label: 'not a class field',
        };
    }
}
"#;

    let structs = extract_structs(content, "test.ts", FieldSyntax::NameBeforeType);
    assert_eq!(structs.len(), 1);
    assert_eq!(structs[0].fields.len(), 1);
    assert_eq!(structs[0].fields[0].name, "name");
    assert_eq!(structs[0].fields[0].field_type, "string");
}

#[test]
fn does_not_report_repeated_type_before_name_self_registration_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();

    for name in &["ai.cls", "fetch.cls", "publish.cls"] {
        std::fs::write(
            src.join(name),
            format!(
                r#"
class {} {{
    public static function register(): void {{
        self::registerStepType(
            class_name: self::class,
            label: 'Step',
        );
    }}
}}
"#,
                name.replace(".cls", "").to_uppercase()
            ),
        )
        .unwrap();
    }

    let findings = detect_repeated_field_patterns(&snapshot_of(dir.path()), &test_scan());
    assert!(
        findings.is_empty(),
        "self::class registration arguments should not be reported as fields"
    );
}

#[test]
fn does_not_report_repeated_boundary_record_coordinates() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src/core/observation");
    std::fs::create_dir_all(&src).unwrap();

    std::fs::write(
        src.join("records.rs"),
        r#"
struct AnnotationFindingRecord {
    line: Option<u32>,
    fixable: bool,
    annotation_id: String,
}

struct NewFindingRecord {
    line: Option<u32>,
    fixable: bool,
    run_id: String,
}

struct FindingRecord {
    line: Option<u32>,
    fixable: bool,
    id: String,
}
"#,
    )
    .unwrap();

    let findings = detect_repeated_field_patterns(&snapshot_of(dir.path()), &test_scan());
    assert!(
        findings.is_empty(),
        "small scalar coordinate overlaps across boundary records are not extractable: {:?}",
        findings
    );
}

#[test]
fn skips_rust_cfg_test_modules_when_scanning_files() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("detector.rs"),
        r##"
pub struct RealConfig {
    enabled: bool,
}

#[cfg(test)]
mod tests {
    #[test]
    fn fixture_strings_do_not_count_as_real_structs() {
        let _ = r#"
struct Foo {
    std: fs::PathBuf,
    std: io::Result<()>,
}
"#;
    }

    #[test]
    fn fixture_two() {
        let _ = r#"
struct Foo {
    std: fs::PathBuf,
    std: io::Result<()>,
}
"#;
    }

    #[test]
    fn fixture_three() {
        let _ = r#"
struct Foo {
    std: fs::PathBuf,
    std: io::Result<()>,
}
"#;
    }
}
"##,
    )
    .unwrap();

    let findings = detect_repeated_field_patterns(&snapshot_of(dir.path()), &test_scan());
    assert!(
        findings.is_empty(),
        "inline Rust test fixtures should not be scanned as production structs: {:?}",
        findings
    );
}

#[test]
fn parse_field_line_rust() {
    let field = parse_field_line("    pub verbose: bool,", FieldSyntax::NameBeforeType);
    assert!(field.is_some());
    let f = field.unwrap();
    assert_eq!(f.name, "verbose");
    assert_eq!(f.field_type, "bool");
}

#[test]
fn parse_field_line_with_option() {
    let field = parse_field_line("    output: Option<PathBuf>,", FieldSyntax::NameBeforeType);
    assert!(field.is_some());
    let f = field.unwrap();
    assert_eq!(f.name, "output");
    assert_eq!(f.field_type, "Option<PathBuf>");
}

#[test]
fn parse_field_line_skips_comments() {
    assert!(parse_field_line("    // a comment", FieldSyntax::NameBeforeType).is_none());
    assert!(parse_field_line("    #[derive(Debug)]", FieldSyntax::NameBeforeType).is_none());
    assert!(parse_field_line("", FieldSyntax::NameBeforeType).is_none());
}

#[test]
fn parse_field_line_skips_functions() {
    assert!(parse_field_line("    fn new() -> Self {", FieldSyntax::NameBeforeType).is_none());
    assert!(parse_field_line("    pub function run() {", FieldSyntax::NameBeforeType).is_none());
}

#[test]
fn extract_type_name_rust() {
    assert_eq!(
        extract_type_name("pub struct Foo {"),
        Some("Foo".to_string())
    );
    assert_eq!(
        extract_type_name("pub(crate) struct Bar<T> {"),
        Some("Bar".to_string())
    );
    assert_eq!(extract_type_name("struct Baz"), Some("Baz".to_string()));
}

#[test]
fn extract_type_name_skips_keywords_inside_string_literals() {
    assert_eq!(extract_type_name("let content = \"struct Foo {\";"), None);
    assert_eq!(extract_type_name("format!(\"class Widget {\")"), None);
}

#[test]
fn extract_type_name_other_langs() {
    assert_eq!(
        extract_type_name("class MyClass {"),
        Some("MyClass".to_string())
    );
    assert_eq!(
        extract_type_name("interface IFoo {"),
        Some("IFoo".to_string())
    );
    assert_eq!(
        extract_type_name("export interface Props {"),
        Some("Props".to_string())
    );
    assert_eq!(
        extract_type_name("export default class Widget {"),
        Some("Widget".to_string())
    );
}

#[test]
fn extract_type_name_skips_comments() {
    assert_eq!(extract_type_name("// struct Comment"), None);
    assert_eq!(extract_type_name("# class PythonStyle"), None);
}

#[test]
fn detects_repeated_pattern() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();

    // Three files with the same field group.
    for name in &["alpha.rs", "beta.rs", "gamma.rs"] {
        std::fs::write(
            src.join(name),
            format!(
                "struct {} {{\n    verbose: bool,\n    quiet: bool,\n}}\n",
                name.replace(".rs", "").to_uppercase()
            ),
        )
        .unwrap();
    }

    let findings = detect_repeated_field_patterns(&snapshot_of(dir.path()), &test_scan());
    assert!(
        !findings.is_empty(),
        "Should detect repeated [verbose, quiet] pattern"
    );
    assert!(findings
        .iter()
        .all(|f| f.kind == AuditFinding::RepeatedFieldPattern));
}

#[test]
fn suppresses_boundary_dto_field_overlap_across_layers() {
    let dir = tempfile::tempdir().unwrap();
    let commands = dir.path().join("src/commands");
    let extension = dir.path().join("src/core/extension/lint");
    let refactor = dir.path().join("src/core/refactor/plan");
    std::fs::create_dir_all(&commands).unwrap();
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::create_dir_all(&refactor).unwrap();

    std::fs::write(
        commands.join("lint.rs"),
        r#"
struct LintArgs {
    category: Option<String>,
    errors_only: bool,
    exclude_sniffs: Option<String>,
    glob: Option<String>,
    sniffs: Option<String>,
    changed_only: bool,
    summary: bool,
}
"#,
    )
    .unwrap();
    std::fs::write(
        extension.join("run.rs"),
        r#"
struct LintRunWorkflowArgs {
    category: Option<String>,
    errors_only: bool,
    exclude_sniffs: Option<String>,
    glob: Option<String>,
    sniffs: Option<String>,
    changed_only: bool,
    summary: bool,
}
"#,
    )
    .unwrap();
    std::fs::write(
        refactor.join("sources.rs"),
        r#"
struct LintSourceOptions {
    category: Option<String>,
    errors_only: bool,
    exclude_sniffs: Option<String>,
    glob: Option<String>,
    sniffs: Option<String>,
}
"#,
    )
    .unwrap();
    std::fs::write(
        commands.join("review.rs"),
        r#"
struct ReviewArgs {
    changed_only: bool,
    summary: bool,
}
"#,
    )
    .unwrap();

    let findings = detect_repeated_field_patterns(&snapshot_of(dir.path()), &test_scan());
    assert!(
            findings.is_empty(),
            "boundary DTO overlap across command/workflow/refactor layers should not suggest extraction: {:?}",
            findings
        );
}

#[test]
fn keeps_boundary_dto_signal_inside_one_layer() {
    let dir = tempfile::tempdir().unwrap();
    let commands = dir.path().join("src/commands");
    std::fs::create_dir_all(&commands).unwrap();

    for name in &["AuditArgs", "LintArgs", "TestArgs"] {
        std::fs::write(
            commands.join(format!("{}.rs", name.to_lowercase())),
            format!("struct {name} {{\n    dry_run: bool,\n    output: Option<String>,\n}}\n"),
        )
        .unwrap();
    }

    let descriptions: Vec<String> =
        detect_repeated_field_patterns(&snapshot_of(dir.path()), &test_scan())
            .into_iter()
            .map(|finding| finding.description)
            .collect();
    assert!(
        descriptions
            .iter()
            .any(|description| description.contains("[dry_run, output]")),
        "boundary DTOs inside one layer can still be useful local extraction signals: {:?}",
        descriptions
    );
}

#[test]
fn suppresses_generic_pairs_across_unrelated_modules() {
    let dir = tempfile::tempdir().unwrap();

    for (module, name, fields) in [
        (
            "ssh",
            "SshConnectOutput",
            "stdout: String,\n    stderr: String,",
        ),
        ("db", "DbResult", "stdout: String,\n    stderr: String,"),
        (
            "fleet",
            "FleetExecProjectResult",
            "stdout: String,\n    stderr: String,",
        ),
        (
            "database",
            "DatabaseConfig",
            "host: String,\n    port: u16,",
        ),
        ("server", "Server", "host: String,\n    port: u16,"),
        ("client", "SshClient", "host: String,\n    port: u16,"),
        ("rename", "RenameSummary", "from: String,\n    to: String,"),
        (
            "variant",
            "VariantSummary",
            "from: String,\n    to: String,",
        ),
        ("file", "FileRename", "from: String,\n    to: String,"),
        (
            "deploy",
            "DeployStatusRow",
            "local_version: String,\n    remote_version: String,",
        ),
        (
            "fleet_status",
            "FleetStatusRow",
            "local_version: String,\n    remote_version: String,",
        ),
        (
            "release",
            "ReleaseStatusRow",
            "local_version: String,\n    remote_version: String,",
        ),
    ] {
        let module_dir = dir.path().join("src").join(module);
        std::fs::create_dir_all(&module_dir).unwrap();
        std::fs::write(
            module_dir.join("types.rs"),
            format!("struct {name} {{\n    {fields}\n}}\n"),
        )
        .unwrap();
    }

    let findings = detect_repeated_field_patterns(&snapshot_of(dir.path()), &test_scan());
    assert!(
        findings.is_empty(),
        "generic DTO field pairs across unrelated modules should not become extraction work: {:?}",
        findings
    );
}

#[test]
fn suppresses_low_value_data_contract_field_overlap() {
    let dir = tempfile::tempdir().unwrap();

    for (path, name, fields) in [
        (
            "src/commands/deploy.rs",
            "DeployOutput",
            "results: Vec<String>,\n    summary: String,",
        ),
        (
            "src/core/deploy/types.rs",
            "DeployOrchestrationResult",
            "results: Vec<String>,\n    summary: String,",
        ),
        (
            "src/core/deploy/result.rs",
            "ProjectDeployResult",
            "results: Vec<String>,\n    summary: String,",
        ),
        (
            "src/commands/status.rs",
            "UpstreamDrift",
            "ahead: usize,\n    behind: usize,",
        ),
        (
            "src/core/context/report.rs",
            "GitSnapshot",
            "ahead: usize,\n    behind: usize,",
        ),
        (
            "src/core/git/operations.rs",
            "RepoSnapshot",
            "ahead: usize,\n    behind: usize,",
        ),
    ] {
        let file = dir.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, format!("struct {name} {{\n    {fields}\n}}\n")).unwrap();
    }

    let findings = detect_repeated_field_patterns(&snapshot_of(dir.path()), &test_scan());
    assert!(
        findings.is_empty(),
        "boundary data contract field overlaps should not suggest extraction: {:?}",
        findings
    );
}

#[test]
fn keeps_generic_pair_signal_with_local_or_suffix_affinity() {
    let dir = tempfile::tempdir().unwrap();
    let network = dir.path().join("src/network");
    std::fs::create_dir_all(&network).unwrap();

    for name in &["PrimaryEndpoint", "ReplicaEndpoint", "FallbackEndpoint"] {
        std::fs::write(
            network.join(format!("{}.rs", name.to_lowercase())),
            format!("struct {name} {{\n    host: String,\n    port: u16,\n}}\n"),
        )
        .unwrap();
    }

    for (module, name) in &[
        ("filesystem", "FileRename"),
        ("workspace", "WorkspaceRename"),
        ("package", "PackageRename"),
    ] {
        let module_dir = dir.path().join("src").join(module);
        std::fs::create_dir_all(&module_dir).unwrap();
        std::fs::write(
            module_dir.join("rename.rs"),
            format!("struct {name} {{\n    from: String,\n    to: String,\n}}\n"),
        )
        .unwrap();
    }

    let descriptions: Vec<String> =
        detect_repeated_field_patterns(&snapshot_of(dir.path()), &test_scan())
            .into_iter()
            .map(|finding| finding.description)
            .collect();

    assert!(
        descriptions
            .iter()
            .any(|description| description.contains("[host, port]")),
        "generic pairs inside one module should still report: {:?}",
        descriptions
    );
    assert!(
        descriptions
            .iter()
            .any(|description| description.contains("[from, to]")),
        "generic pairs on structs with a shared suffix should still report: {:?}",
        descriptions
    );
}

#[test]
fn repeated_pattern_description_orders_fields_deterministically() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();

    for name in &["alpha.rs", "beta.rs", "gamma.rs"] {
        std::fs::write(
            src.join(name),
            format!(
                "struct {} {{\n    zebra: bool,\n    alpha: bool,\n    middle: bool,\n}}\n",
                name.replace(".rs", "").to_uppercase()
            ),
        )
        .unwrap();
    }

    let findings = detect_repeated_field_patterns(&snapshot_of(dir.path()), &test_scan());
    assert!(
        !findings.is_empty(),
        "Should detect repeated [alpha, middle, zebra] pattern"
    );
    assert!(
        findings.iter().all(|f| f
            .description
            .contains("Repeated field group [alpha, middle, zebra]")),
        "field order in descriptions must be stable and lexical: {:?}",
        findings
            .iter()
            .map(|f| f.description.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        findings
            .iter()
            .all(|f| f.suggestion.contains("[alpha, middle, zebra]")),
        "field order in suggestions must match descriptions"
    );
}

#[test]
fn ignores_below_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();

    // Only 2 files (below MIN_OCCURRENCES=3).
    for name in &["alpha.rs", "beta.rs"] {
        std::fs::write(
            src.join(name),
            "struct Foo {\n    x: i32,\n    y: i32,\n}\n",
        )
        .unwrap();
    }

    let findings = detect_repeated_field_patterns(&snapshot_of(dir.path()), &test_scan());
    assert!(
        findings.is_empty(),
        "Two occurrences should be below threshold"
    );
}
