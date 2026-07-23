use std::collections::{HashMap, HashSet};
use std::path::Path;

use homeboy_code_audit::fingerprint::{self, FileFingerprint};
use homeboy_code_audit::walker;
use homeboy_core::engine::symbol_graph;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRole {
    Regular,
    Index,
    PublicApi,
}

#[derive(Debug, Clone)]
pub struct SymbolSurface {
    pub incoming_callers: Vec<String>,
    pub incoming_importers: Vec<String>,
    pub reexport_files: Vec<String>,
}

impl SymbolSurface {
    pub fn has_external_usage(&self, owner_file: &str) -> bool {
        self.incoming_callers.iter().any(|file| file != owner_file)
            || self
                .incoming_importers
                .iter()
                .any(|file| file != owner_file)
            || self.reexport_files.iter().any(|file| file != owner_file)
    }
}

#[derive(Debug, Clone)]
pub struct ModuleSurface {
    pub file: String,
    pub role: FileRole,
    pub public_api: HashSet<String>,
    pub internal_calls: HashSet<String>,
    pub call_sites: HashSet<String>,
    pub symbols: HashMap<String, SymbolSurface>,
}

impl ModuleSurface {
    pub fn owns_public_symbol(&self, symbol: &str) -> bool {
        self.public_api.contains(symbol)
    }

    pub fn symbol_surface(&self, symbol: &str) -> Option<&SymbolSurface> {
        self.symbols.get(symbol)
    }

    pub fn is_api_barrel(&self) -> bool {
        matches!(self.role, FileRole::Index | FileRole::PublicApi)
    }
}

#[derive(Debug, Default)]
pub struct ModuleSurfaceIndex {
    by_file: HashMap<String, ModuleSurface>,
}

impl ModuleSurfaceIndex {
    pub fn build(root: &Path) -> Self {
        let snapshot = walker::walk_source_files_snapshot(root);
        let mut fingerprints = Vec::new();

        for (file_path, content) in snapshot.iter() {
            let Some(fp) = fingerprint::fingerprint_content(file_path, root, content) else {
                continue;
            };
            fingerprints.push(fp);
        }

        Self::from_fingerprints(root, &fingerprints)
    }

    pub fn from_fingerprints(root: &Path, fingerprints: &[FileFingerprint]) -> Self {
        let symbol_refs = symbol_graph::SymbolReferenceIndex::from_files(
            fingerprints
                .iter()
                .map(|fp| (fp.relative_path.as_str(), fp.content.as_str())),
        );
        let mut by_file = HashMap::new();

        for fp in fingerprints {
            let surface = build_surface_for_fingerprint_with_symbols(root, fp, &symbol_refs);
            by_file.insert(surface.file.clone(), surface);
        }

        Self { by_file }
    }

    pub fn get(&self, file: &str) -> Option<&ModuleSurface> {
        self.by_file.get(file)
    }

    #[cfg(test)]
    pub(crate) fn from_surfaces(surfaces: Vec<ModuleSurface>) -> Self {
        let by_file = surfaces
            .into_iter()
            .map(|surface| (surface.file.clone(), surface))
            .collect();
        Self { by_file }
    }
}

fn build_surface_for_fingerprint_with_symbols(
    root: &Path,
    fp: &FileFingerprint,
    symbol_refs: &symbol_graph::SymbolReferenceIndex,
) -> ModuleSurface {
    let file = fp.relative_path.clone();
    let module_path = symbol_graph::module_path_from_file(&file);
    let role = classify_file_role(&file);
    let public_api: HashSet<String> = fp.public_api.iter().cloned().collect();
    let internal_calls: HashSet<String> = fp.internal_calls.iter().cloned().collect();
    let call_sites: HashSet<String> = fp
        .call_sites
        .iter()
        .map(|site| site.target.clone())
        .collect();

    let mut symbols = HashMap::new();
    for symbol in &public_api {
        let callers = symbol_refs.trace_symbol_callers(symbol, &module_path);
        let mut incoming_callers = Vec::new();
        let mut incoming_importers = Vec::new();
        for caller in callers {
            if caller.has_call_site {
                incoming_callers.push(caller.file.clone());
            }
            if caller.import.is_some() {
                incoming_importers.push(caller.file);
            }
        }

        let reexport_files = find_reexport_files_for_symbol(root, &file, symbol);

        symbols.insert(
            symbol.clone(),
            SymbolSurface {
                incoming_callers,
                incoming_importers,
                reexport_files,
            },
        );
    }

    ModuleSurface {
        file,
        role,
        public_api,
        internal_calls,
        call_sites,
        symbols,
    }
}

fn classify_file_role(file: &str) -> FileRole {
    let path = Path::new(file);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if walker::is_index_file(path) {
        return FileRole::Index;
    }
    if file_name == "public_api.rs" {
        return FileRole::PublicApi;
    }
    FileRole::Regular
}

fn find_reexport_files_for_symbol(root: &Path, file_path: &str, symbol: &str) -> Vec<String> {
    let source_path = Path::new(file_path);
    let mut result = Vec::new();
    let mut current = source_path.parent();

    while let Some(dir) = current {
        for filename in ["mod.rs", "lib.rs"] {
            let check_path = root.join(dir).join(filename);
            if !check_path.exists() {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&check_path) else {
                continue;
            };
            if has_pub_use_of(&content, symbol) {
                result.push(format!("{}/{}", dir.display(), filename));
            }
        }
        current = dir.parent();
    }

    result
}

fn has_pub_use_of(content: &str, symbol: &str) -> bool {
    let word_re = match regex::Regex::new(&format!(r"\b{}\b", regex::escape(symbol))) {
        Ok(re) => re,
        Err(_) => return false,
    };

    let mut in_pub_use_block = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if in_pub_use_block {
            if word_re.is_match(trimmed) {
                return true;
            }
            if trimmed.contains("};") || trimmed == "}" {
                in_pub_use_block = false;
            }
        } else if trimmed.starts_with("pub use") {
            if trimmed.contains("::*") {
                continue;
            }
            if word_re.is_match(trimmed) {
                return true;
            }
            if trimmed.contains('{') && !trimmed.contains('}') {
                in_pub_use_block = true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_public_api_role() {
        assert_eq!(
            classify_file_role("src/core/code_audit/public_api.rs"),
            FileRole::PublicApi
        );
        assert_eq!(
            classify_file_role("src/core/code_audit/mod.rs"),
            FileRole::Index
        );
        assert_eq!(
            classify_file_role("src/core/code_audit/findings.rs"),
            FileRole::Regular
        );
    }

    #[test]
    fn detects_pub_use_block_members() {
        let content = "pub use super::{foo, bar};\n";
        assert!(has_pub_use_of(content, "foo"));
        assert!(has_pub_use_of(content, "bar"));
        assert!(!has_pub_use_of(content, "baz"));
    }

    /// A grammar-source provider that serves a fixed grammar directory for `.rs`.
    ///
    /// `ModuleSurfaceIndex::build` fingerprints files via
    /// `code_audit::load_grammar_for_ext`, which resolves the grammar dir through
    /// the globally-registered `GrammarSourceProvider`. In a unit test no CLI has
    /// registered one, so the default `NoopProvider` returns `None` for every
    /// extension and no file is fingerprinted (#9782). Registering this fixture
    /// makes the walk self-contained instead of depending on an installed
    /// extension in the ambient environment.
    struct FixtureRustGrammar {
        dir: std::path::PathBuf,
    }

    impl homeboy_code_audit::grammar_source_provider::GrammarSourceProvider for FixtureRustGrammar {
        fn grammar_dir(&self, file_extension: &str) -> Option<std::path::PathBuf> {
            (file_extension == "rs").then(|| self.dir.clone())
        }
    }

    /// An audit-manifest provider that declares `.rs` as a handled extension.
    ///
    /// The source walker (`walk_source_files_snapshot`) only includes files whose
    /// extension appears in some installed audit manifest's
    /// `provided_file_extensions`. With the default `NoopProvider` that set is
    /// empty, so `producer.rs`/`consumer.rs` are never walked and the index is
    /// empty. Registering this fixture lets the walk see `.rs` files.
    struct FixtureRustManifest;

    impl homeboy_code_audit::extension_manifests::AuditExtensionManifestProvider
        for FixtureRustManifest
    {
        fn load_all(&self) -> Vec<homeboy_code_audit::extension_manifests::AuditExtensionManifest> {
            vec![
                homeboy_code_audit::extension_manifests::AuditExtensionManifest {
                    id: "fixture-rust".to_string(),
                    provided_file_extensions: vec!["rs".to_string()],
                    ..Default::default()
                },
            ]
        }

        fn load(
            &self,
            _id: &str,
        ) -> Option<homeboy_code_audit::extension_manifests::AuditExtensionManifest> {
            None
        }
    }

    /// Write a minimal Rust `grammar.toml` sufficient for module-surface
    /// analysis: function definitions (drives `public_api`) and `use` imports
    /// (drives cross-file caller/importer linking). Call sites are matched by
    /// content substring in `SymbolReferenceIndex`, so no call-site grammar is
    /// needed. Patterns mirror the shipped Rust grammar.
    fn write_fixture_rust_grammar(dir: &Path) {
        std::fs::write(
            dir.join("grammar.toml"),
            r#"
[language]
id = "rust"
extensions = ["rs"]

[comments]
line = ["//"]

[strings]
quotes = ['"']

[patterns.function]
regex = '^\s*(pub(?:\([^)]+\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+(\w+)\s*\(([^)]*)\)?'
context = "any"
skip_comments = true
skip_strings = true

[patterns.function.captures]
visibility = 1
name = 2
params = 3

[patterns.import]
regex = '^use\s+([\w:]+(?:::\{[^}]+\})?)\s*;'
context = "top_level"
skip_comments = true
skip_strings = true

[patterns.import.captures]
path = 1
"#,
        )
        .unwrap();
    }

    #[test]
    fn build_uses_snapshot_content_for_module_surfaces() {
        // Make the walk self-contained instead of depending on an installed
        // extension: (1) declare `.rs` as a handled extension so the walker
        // includes the fixture files, and (2) serve a `.rs` grammar so they are
        // fingerprinted. Both providers default to a no-op that yields nothing.
        //
        // The providers are process-global (`static Mutex<Option<..>>`), so
        // register them exactly once and keep them installed for the whole test
        // binary. A per-test re-register would race with any concurrently running
        // test that reads the grammar/manifest registry (parallel test threads
        // share this process). The grammar dir is leaked intentionally so the
        // registered provider keeps pointing at a live path for the run.
        // Serialize against other tests that mutate process-global environment
        // state (the source-cache tests toggle `HOMEBOY_OUTPUT_DIR`), which can
        // otherwise perturb snapshot/grammar resolution mid-walk.
        let _env_guard = homeboy_core::test_support::env_lock();

        static FIXTURE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        FIXTURE.get_or_init(|| {
            homeboy_code_audit::extension_manifests::register_audit_extension_manifest_provider(
                Box::new(FixtureRustManifest),
            );
            let grammar_dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
            write_fixture_rust_grammar(grammar_dir.path());
            homeboy_code_audit::grammar_source_provider::register_grammar_source_provider(
                Box::new(FixtureRustGrammar {
                    dir: grammar_dir.path().to_path_buf(),
                }),
            );
        });

        let dir = tempfile::tempdir().unwrap();
        // The fixture lives under `src/example/` so `producer.rs` resolves to the
        // module `example::producer`, matching the consumer's
        // `use crate::example::producer::make_value;` import. (A `src/core/...`
        // path would resolve to `core::example::...`, which — after the
        // code_audit crate extraction — no longer aliases `crate::`.)
        let src = dir.path().join("src/example");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("producer.rs"),
            "pub fn make_value() -> usize { 1 }\n",
        )
        .unwrap();
        std::fs::write(
            src.join("consumer.rs"),
            "use crate::example::producer::make_value;\n\npub fn consume() -> usize { make_value() }\n",
        )
        .unwrap();

        let index = ModuleSurfaceIndex::build(dir.path());
        let producer = index.get("src/example/producer.rs").unwrap();
        let surface = producer.symbol_surface("make_value").unwrap();

        assert!(producer.owns_public_symbol("make_value"));
        assert!(surface
            .incoming_callers
            .contains(&"src/example/consumer.rs".to_string()));
        assert!(surface
            .incoming_importers
            .contains(&"src/example/consumer.rs".to_string()));
    }
}
