use std::sync::{Mutex, MutexGuard, OnceLock};

use tempfile::TempDir;

pub(crate) struct HomeGuard {
    prior: Option<String>,
    prior_xdg_data_home: Option<String>,
    prior_artifact_root: Option<String>,
    prior_runtime_tmpdir: Option<String>,
    prior_invocation_runtime: Option<String>,
    dir: TempDir,
    /// Held alongside `dir` so the short invocation runtime tempdir is
    /// dropped only after the test completes. Distinct from `dir` so the
    /// invocation root can live on a short path (e.g. `/tmp/hb-XXXX`)
    /// regardless of where `$TMPDIR` lands.
    _inv_dir: Option<TempDir>,
    _guard: MutexGuard<'static, ()>,
}

pub(crate) struct AuditGuard {
    _guard: MutexGuard<'static, ()>,
    _home_guard: MutexGuard<'static, ()>,
}

pub(crate) struct AuditHomeGuard {
    home: HomeGuard,
    _guard: MutexGuard<'static, ()>,
}

impl AuditGuard {
    pub(crate) fn new() -> Self {
        let home_guard = home_lock().lock().unwrap_or_else(|e| e.into_inner());
        let guard = audit_lock().lock().unwrap_or_else(|e| e.into_inner());
        Self {
            _guard: guard,
            _home_guard: home_guard,
        }
    }
}

impl AuditHomeGuard {
    pub(crate) fn new() -> Self {
        let home = HomeGuard::new();
        let guard = audit_lock().lock().unwrap_or_else(|e| e.into_inner());
        Self {
            _guard: guard,
            home,
        }
    }
}

impl HomeGuard {
    pub(crate) fn new() -> Self {
        let guard = home_lock().lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var("HOME").ok();
        let prior_xdg_data_home = std::env::var("XDG_DATA_HOME").ok();
        let prior_artifact_root = std::env::var("HOMEBOY_ARTIFACT_ROOT").ok();
        let prior_runtime_tmpdir = std::env::var("HOMEBOY_RUNTIME_TMPDIR").ok();
        let prior_invocation_runtime =
            std::env::var(crate::core::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV).ok();
        let dir = TempDir::new().expect("home tempdir");
        std::env::set_var("HOME", dir.path());
        std::env::set_var("XDG_DATA_HOME", dir.path().join(".local").join("share"));
        std::env::remove_var("HOMEBOY_ARTIFACT_ROOT");
        std::env::remove_var("HOMEBOY_RUNTIME_TMPDIR");
        crate::core::set_artifact_root_override(None);
        // Pin invocation runtime to a SHORT tempdir, isolated from `$TMPDIR`
        // and from the home tempdir (which itself can already live on a long
        // path on macOS, e.g. `/var/folders/<14>/T/.tmpXXXXXX/...`). Using
        // `/tmp` directly keeps tests within the platform `sockaddr_un`
        // budget regardless of host configuration.
        let inv_dir = short_invocation_tempdir();
        std::env::set_var(
            crate::core::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV,
            inv_dir.path(),
        );
        Self {
            prior,
            prior_xdg_data_home,
            prior_artifact_root,
            prior_runtime_tmpdir,
            prior_invocation_runtime,
            dir,
            _inv_dir: Some(inv_dir),
            _guard: guard,
        }
    }
}

/// Return a short-path tempdir suitable for the invocation runtime root.
///
/// On Unix-like systems we anchor to `/tmp` so the path stays well under
/// `sockaddr_un` even on macOS where `$TMPDIR` typically lives at
/// `/var/folders/<14>/T/.tmpXXXXXX/`. Falls back to the default tempdir
/// otherwise (Windows / odd hosts).
fn short_invocation_tempdir() -> TempDir {
    #[cfg(unix)]
    {
        if std::path::Path::new("/tmp").is_dir() {
            return tempfile::Builder::new()
                .prefix("hb-test-")
                .tempdir_in("/tmp")
                .expect("invocation runtime tempdir under /tmp");
        }
    }
    TempDir::new().expect("invocation runtime tempdir")
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match &self.prior_xdg_data_home {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
        match &self.prior_artifact_root {
            Some(value) => std::env::set_var("HOMEBOY_ARTIFACT_ROOT", value),
            None => std::env::remove_var("HOMEBOY_ARTIFACT_ROOT"),
        }
        match &self.prior_runtime_tmpdir {
            Some(value) => std::env::set_var("HOMEBOY_RUNTIME_TMPDIR", value),
            None => std::env::remove_var("HOMEBOY_RUNTIME_TMPDIR"),
        }
        crate::core::set_artifact_root_override(None);
        match &self.prior_invocation_runtime {
            Some(value) => std::env::set_var(
                crate::core::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV,
                value,
            ),
            None => std::env::remove_var(
                crate::core::engine::invocation::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV,
            ),
        }
    }
}

pub(crate) fn with_isolated_home<R>(body: impl FnOnce(&TempDir) -> R) -> R {
    let home = HomeGuard::new();
    body(&home.dir)
}

pub(crate) fn with_isolated_audit_home<R>(body: impl FnOnce(&TempDir) -> R) -> R {
    let guard = AuditHomeGuard::new();
    body(&guard.home.dir)
}

pub(crate) fn write_source_extension(home: &std::path::Path, id: &str, file_extension: &str) {
    let extension_dir = home.join(".config/homeboy/extensions").join(id);
    std::fs::create_dir_all(&extension_dir).expect("extension dir");
    std::fs::write(
        extension_dir.join(format!("{id}.json")),
        serde_json::json!({
            "name": id,
            "version": "0.0.0",
            "provides": {
                "file_extensions": [file_extension]
            },
            "scripts": {
                "fingerprint": "fingerprint.sh"
            }
        })
        .to_string(),
    )
    .expect("source extension manifest");
    std::fs::write(
        extension_dir.join("fingerprint.sh"),
        "#!/usr/bin/env sh\nexit 1\n",
    )
    .expect("fingerprint script");

    if matches!(file_extension, "rs" | "fixture") {
        std::fs::write(extension_dir.join("grammar.toml"), minimal_source_grammar())
            .expect("source grammar");
    }
}

fn minimal_source_grammar() -> &'static str {
    r#"
[language]
id = "source"
extensions = ["rs", "fixture"]

[comments]
line = ["//"]
block = [["/*", "*/"]]
doc = ["///", "//!"]

[strings]
quotes = ['"']
escape = "\\"

[blocks]
open = "{"
close = "}"

[fingerprint]
keywords = ["fn", "let", "if", "for", "return", "true", "false", "pub", "struct", "impl", "trait", "Self", "Result", "String", "bool", "i32", "usize"]
skip_calls = ["if", "for", "return", "println", "write", "assert"]
contract_method_names = []
contract_type_hints = []
registration_concepts = ["macro_invocation"]
registration_skip_names = ["println", "assert", "write"]
registration_skip_prefixes = ["test"]

[fingerprint.namespace_derivation]
prefix = "crate::"
strip_leading_segments = 1
separator = "::"
include_file_stem_when_root = true

[patterns.function]
regex = '^\s*(pub(?:\(crate\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+(\w+)\s*\(([^)]*)\)'
context = "any"

[patterns.function.captures]
visibility = 1
name = 2
params = 3

[patterns.struct]
regex = '^\s*(pub(?:\(crate\))?\s+)?(struct|enum|trait)\s+(\w+)'
context = "top_level"

[patterns.struct.captures]
visibility = 1
kind = 2
name = 3

[patterns.import]
regex = '^use\s+([\w:]+(?:::\{[^}]+\})?)\s*;'
context = "top_level"

[patterns.import.captures]
path = 1

[patterns.impl_block]
regex = '^\s*impl(?:<[^>]*>)?\s+(?:(\w+)\s+for\s+)?(\w+)'
context = "any"

[patterns.impl_block.captures]
trait_name = 1
type_name = 2

[patterns.test_attribute]
regex = '#\[test\]'
context = "any"

[patterns.cfg_test]
regex = '#\[cfg\(test\)\]'
context = "any"
"#
}

pub(crate) fn home_env_guard() -> MutexGuard<'static, ()> {
    home_lock().lock().unwrap_or_else(|e| e.into_inner())
}

fn home_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn audit_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}
