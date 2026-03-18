use std::collections::HashMap;
use std::path::Path;

use super::fingerprint::FileFingerprint;
use crate::extension::TestMappingConfig;

/// Partition fingerprints into source files and test files based on the config.
pub fn partition_fingerprints<'a>(
    fingerprints: &[&'a FileFingerprint],
    config: &TestMappingConfig,
) -> (Vec<&'a FileFingerprint>, Vec<&'a FileFingerprint>) {
    let mut source = Vec::new();
    let mut test = Vec::new();

    for fp in fingerprints {
        if is_test_file(&fp.relative_path, config) {
            test.push(*fp);
        } else if is_source_file(&fp.relative_path, config) {
            source.push(*fp);
        }
    }

    (source, test)
}

/// Build a class/stem name → source file path index from source fingerprints.
pub fn build_source_name_index<'a>(source_fps: &[&'a FileFingerprint]) -> HashMap<String, &'a str> {
    let mut index = HashMap::new();
    for fp in source_fps {
        if let Some(stem) = extract_file_stem(&fp.relative_path) {
            index.insert(stem.to_lowercase(), fp.relative_path.as_str());
        }
    }
    index
}

/// Discover a source file for a test file by class name, falling back from template matching.
pub fn discover_source_file<'a>(
    test_path: &str,
    config: &TestMappingConfig,
    source_name_index: &HashMap<String, &'a str>,
) -> Option<&'a str> {
    // Tier 1: Template match (existing behavior)
    if let Some(template_path) = test_to_source_path(test_path, config) {
        if let Some(&source_path) =
            source_name_index.get(&extract_file_stem(&template_path)?.to_lowercase())
        {
            if source_path == template_path {
                return Some(source_path);
            }
        }
    }

    // Tier 2: Name-based auto-discovery
    let test_stem = extract_test_stem(test_path)?;
    source_name_index.get(&test_stem.to_lowercase()).copied()
}

/// Extract the file stem (class name) from a path, e.g., "inc/Foo/Bar.php" → "Bar"
fn extract_file_stem(path: &str) -> Option<&str> {
    Path::new(path).file_stem()?.to_str()
}

/// Extract the source class name from a test file path.
/// "tests/Unit/Foo/BarTest.php" → "bar" (lowercased, "Test" suffix stripped)
fn extract_test_stem(path: &str) -> Option<String> {
    let stem = Path::new(path).file_stem()?.to_str()?;
    // Strip common test suffixes
    let base = stem
        .strip_suffix("Test")
        .or_else(|| stem.strip_suffix("_test"))
        .unwrap_or(stem);
    Some(base.to_string())
}

/// Check if a file path is within one of the configured source directories.
pub(crate) fn is_source_file(path: &str, config: &TestMappingConfig) -> bool {
    config.source_dirs.iter().any(|dir| path.starts_with(dir))
}

/// Check if a file path is within one of the configured test directories.
pub(crate) fn is_test_file(path: &str, config: &TestMappingConfig) -> bool {
    config.test_dirs.iter().any(|dir| path.starts_with(dir))
}

/// Convert a source file path to its expected test file path using the template.
///
/// Template variables: `{dir}` (relative dir within source_dir), `{name}` (stem), `{ext}` (extension).
pub fn source_to_test_path(source_path: &str, config: &TestMappingConfig) -> Option<String> {
    let source_dir = config
        .source_dirs
        .iter()
        .find(|dir| source_path.starts_with(dir.as_str()))?;

    let relative = source_path.strip_prefix(source_dir)?;
    let relative = relative.strip_prefix('/').unwrap_or(relative);

    let path = Path::new(relative);
    let name = path.file_stem()?.to_str()?;
    let ext = path.extension()?.to_str()?;
    let dir = path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let test_path = config
        .test_file_pattern
        .replace("{dir}", &dir)
        .replace("{name}", name)
        .replace("{ext}", ext);

    Some(test_path.replace("//", "/"))
}

/// Convert a test file path back to its expected source file path.
pub fn test_to_source_path(test_path: &str, config: &TestMappingConfig) -> Option<String> {
    let pattern = &config.test_file_pattern;
    let test_dir = config.test_dirs.first()?;

    let relative_in_test = if test_path.starts_with(test_dir.as_str()) {
        let stripped = test_path.strip_prefix(test_dir.as_str())?;
        stripped.strip_prefix('/').unwrap_or(stripped)
    } else {
        return None;
    };

    let pattern_after_test_dir = if pattern.starts_with(test_dir.as_str()) {
        let stripped = pattern.strip_prefix(test_dir.as_str())?;
        stripped.strip_prefix('/').unwrap_or(stripped)
    } else {
        pattern.as_str()
    };

    let name_pos = pattern_after_test_dir.find("{name}")?;
    let after_name = &pattern_after_test_dir[name_pos + 6..];

    let test_file_path = Path::new(relative_in_test);
    let test_ext = test_file_path.extension()?.to_str()?;
    let test_stem = test_file_path.file_stem()?.to_str()?;
    let test_dir_part = test_file_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let suffix_before_ext = after_name.strip_suffix(".{ext}").unwrap_or("");
    let source_name = test_stem.strip_suffix(suffix_before_ext)?;
    let source_dir = config.source_dirs.first()?;

    Some(if test_dir_part.is_empty() {
        format!("{}/{}.{}", source_dir, source_name, test_ext)
    } else {
        format!(
            "{}/{}/{}.{}",
            source_dir, test_dir_part, source_name, test_ext
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::TestMappingConfig;

    fn make_config() -> TestMappingConfig {
        TestMappingConfig {
            source_dirs: vec!["src".to_string()],
            test_dirs: vec!["tests".to_string()],
            test_file_pattern: "tests/{dir}/{name}_test.{ext}".to_string(),
            method_prefix: "test_".to_string(),
            critical_patterns: vec![],
            inline_tests: true,
            skip_test_patterns: vec![],
        }
    }

    #[test]
    fn source_to_test_path_basic() {
        let config = make_config();
        assert_eq!(
            source_to_test_path("src/core/audit.rs", &config),
            Some("tests/core/audit_test.rs".to_string())
        );
    }

    #[test]
    fn source_to_test_path_top_level() {
        let config = make_config();
        assert_eq!(
            source_to_test_path("src/main.rs", &config),
            Some("tests/main_test.rs".to_string())
        );
    }

    #[test]
    fn test_to_source_path_basic() {
        let config = make_config();
        assert_eq!(
            test_to_source_path("tests/core/audit_test.rs", &config),
            Some("src/core/audit.rs".to_string())
        );
    }
}
