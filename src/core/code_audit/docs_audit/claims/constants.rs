//! constants — extracted from claims.rs.

use regex::Regex;
use std::sync::LazyLock;


pub(crate) static FILE_PATH_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Matches paths that contain at least one directory separator
    // e.g., `src/main.rs`, `/inc/Engine/AI/Tools/BaseTool.php`, `path/to/file.ext`
    // Must have: path separator + file extension
    Regex::new(r"`(/?(?:[\w.-]+/)+[\w.-]+\.[a-zA-Z0-9]+)`").unwrap()
});

pub(crate) static DIR_PATH_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Matches directory paths like `/inc/Engine/` or `src/core/` in backticks
    // Must end with / and contain at least one directory level
    Regex::new(r"`(/?(?:[\w.-]+/)+)`").unwrap()
});

pub(crate) static CODE_BLOCK_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Matches fenced code blocks with language identifier
    Regex::new(r"(?s)```(\w+)\n(.*?)```").unwrap()
});

pub(crate) static CLASS_NAME_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Matches namespaced class references like DataMachine\Services\CacheManager
    // or DataMachine\\Services\\CacheManager (escaped backslashes in markdown)
    // Requires at least two segments (Namespace\Class)
    Regex::new(r"(?:`)?([A-Z][a-zA-Z0-9]*(?:\\{1,2}[A-Z][a-zA-Z0-9]*)+)(?:`)?").unwrap()
});

/// Extensions that indicate domain-like patterns (not file paths)
pub(crate) const DOMAIN_EXTENSIONS: &[&str] = &[
    ".com", ".org", ".io", ".net", ".dev", ".co", ".app", ".ai", ".xyz",
];

/// Placeholder name prefixes that indicate example/template references.
pub(crate) const PLACEHOLDER_PREFIXES: &[&str] = &[
    "My", "Your", "Example", "Sample", "Foo", "Bar", "Test", "Demo", "Dummy", "Fake",
];
