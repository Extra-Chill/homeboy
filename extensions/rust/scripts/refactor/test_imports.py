"""Tests for import resolution in decompose refactoring."""

import unittest
from .imports import resolve_imports, extract_use_names, fix_import_path
from .module_index import generate_module_index


class TestExtractUseNames(unittest.TestCase):
    def test_simple_import(self):
        self.assertEqual(extract_use_names("use std::path::Path;"), ["Path"])

    def test_grouped_import(self):
        names = extract_use_names("use std::path::{Path, PathBuf};")
        self.assertIn("Path", names)
        self.assertIn("PathBuf", names)

    def test_alias_import(self):
        self.assertEqual(extract_use_names("use foo::Bar as Baz;"), ["Baz"])

    def test_glob_import(self):
        # Glob imports don't produce terminal names
        self.assertEqual(extract_use_names("use super::settings::*;"), [])

    def test_self_group_import_extracts_module_name(self):
        """use foo::{self, Bar} should extract both 'foo' (from self) and 'Bar'."""
        names = extract_use_names("use crate::engine::local_files::{self, FileSystem};")
        self.assertIn("local_files", names)
        self.assertIn("FileSystem", names)

    def test_self_group_import_without_companions(self):
        """use foo::{self} should extract the module name."""
        names = extract_use_names("use crate::engine::local_files::{self};")
        self.assertIn("local_files", names)


class TestResolveImportsPhase2Functions(unittest.TestCase):
    """Phase 2 should detect functions and constants, not just types."""

    def test_detects_function_reference(self):
        source = """use std::path::Path;

pub(super) fn find_next_section_start(lines: &[&str]) -> Option<usize> {
    lines.iter().position(|l| l.starts_with("## "))
}

pub(super) fn find_section_end(lines: &[&str], start: usize) -> usize {
    start + 1
}

pub fn count_entries(content: &str) -> usize {
    0
}
"""
        moved_items = [{
            "name": "count_entries",
            "kind": "fn",
            "source": """pub fn count_entries(content: &str) -> usize {
    let start = find_next_section_start(&lines);
    let end = find_section_end(&lines, start);
    0
}""",
        }]
        result = resolve_imports(
            moved_items, source,
            "src/core/release/changelog/sections.rs",
            "src/core/release/changelog/sections/unreleased.rs",
        )
        imports = result["needed_imports"]
        import_text = "\n".join(imports)
        self.assertIn("find_next_section_start", import_text)
        self.assertIn("find_section_end", import_text)

    def test_detects_constant_reference(self):
        source = """const MAX_SIZE: usize = 100;

pub fn check_size(n: usize) -> bool {
    n < MAX_SIZE
}
"""
        moved_items = [{
            "name": "check_size",
            "kind": "fn",
            "source": "pub fn check_size(n: usize) -> bool { n < MAX_SIZE }",
        }]
        result = resolve_imports(
            moved_items, source,
            "src/core/engine.rs",
            "src/core/engine/check.rs",
        )
        imports = result["needed_imports"]
        import_text = "\n".join(imports)
        self.assertIn("MAX_SIZE", import_text)

    def test_does_not_import_moved_item(self):
        source = """pub fn helper() {}
pub fn main_fn() { helper(); }
"""
        moved_items = [
            {"name": "helper", "kind": "fn", "source": "pub fn helper() {}"},
            {"name": "main_fn", "kind": "fn", "source": "pub fn main_fn() { helper(); }"},
        ]
        result = resolve_imports(
            moved_items, source,
            "src/core/foo.rs",
            "src/core/foo/bar.rs",
        )
        imports = result["needed_imports"]
        self.assertFalse(any("helper" in i for i in imports))

    def test_decompose_uses_super_not_crate(self):
        """When dest is a child of source, Phase 2 should use super:: not crate::."""
        source = """pub fn stay_here() {}
pub fn move_me() { stay_here(); }
"""
        moved_items = [{
            "name": "move_me",
            "kind": "fn",
            "source": "pub fn move_me() { stay_here(); }",
        }]
        result = resolve_imports(
            moved_items, source,
            "src/core/big.rs",
            "src/core/big/helpers.rs",
        )
        imports = result["needed_imports"]
        import_text = "\n".join(imports)
        self.assertIn("use super::stay_here;", import_text)
        self.assertNotIn("crate::", import_text)


class TestResolveImportsPhase3Globs(unittest.TestCase):
    """Phase 3 should carry forward glob imports for unresolved references."""

    def test_carries_glob_for_unresolved_constant(self):
        source = """use super::settings::*;

pub fn validate_content(lines: &[&str]) -> bool {
    KEEP_A_CHANGELOG_SUBSECTIONS.iter().any(|h| lines[0].starts_with(h))
}
"""
        moved_items = [{
            "name": "validate_content",
            "kind": "fn",
            "source": """pub fn validate_content(lines: &[&str]) -> bool {
    KEEP_A_CHANGELOG_SUBSECTIONS.iter().any(|h| lines[0].starts_with(h))
}""",
        }]
        result = resolve_imports(
            moved_items, source,
            "src/core/release/changelog/sections.rs",
            "src/core/release/changelog/sections/normalize.rs",
        )
        imports = result["needed_imports"]
        import_text = "\n".join(imports)
        self.assertIn("settings::*", import_text)

    def test_no_glob_when_all_resolved(self):
        source = """use super::settings::*;

const MY_CONST: usize = 42;

pub fn uses_local() -> usize {
    MY_CONST
}
"""
        moved_items = [{
            "name": "uses_local",
            "kind": "fn",
            "source": "pub fn uses_local() -> usize { MY_CONST }",
        }]
        result = resolve_imports(
            moved_items, source,
            "src/core/foo.rs",
            "src/core/foo/bar.rs",
        )
        imports = result["needed_imports"]
        self.assertFalse(any("::*" in i for i in imports))


class TestTraitImports(unittest.TestCase):
    """Trait imports must be carried when self-imported module methods need them."""

    def test_self_group_carries_trait_for_method_dispatch(self):
        """use foo::{self, Trait} should be carried when foo::method() is used."""
        source = """use crate::engine::local_files::{self, FileSystem};

pub fn read_version(path: &str) -> String {
    let content = local_files::local().read(path).unwrap();
    content
}
"""
        moved_items = [{
            "name": "read_version",
            "kind": "fn",
            "source": """pub fn read_version(path: &str) -> String {
    let content = local_files::local().read(path).unwrap();
    content
}""",
        }]
        result = resolve_imports(
            moved_items, source,
            "src/core/release/version.rs",
            "src/core/release/version/helpers.rs",
        )
        imports = result["needed_imports"]
        import_text = "\n".join(imports)
        # The entire {self, FileSystem} import should be carried
        # because local_files:: is used as a path qualifier
        self.assertIn("local_files", import_text)
        self.assertIn("FileSystem", import_text)

    def test_simple_module_import_carried_for_path_usage(self):
        """use crate::foo should be carried when foo::bar() is used."""
        source = """use crate::engine::text;

pub fn extract(content: &str) -> String {
    text::extract_first(content, "pattern").unwrap()
}
"""
        moved_items = [{
            "name": "extract",
            "kind": "fn",
            "source": """pub fn extract(content: &str) -> String {
    text::extract_first(content, "pattern").unwrap()
}""",
        }]
        result = resolve_imports(
            moved_items, source,
            "src/core/big.rs",
            "src/core/big/helpers.rs",
        )
        imports = result["needed_imports"]
        import_text = "\n".join(imports)
        self.assertIn("text", import_text)


class TestResolveImportsRealWorldRegression(unittest.TestCase):
    """Regression tests for real decomposition failures from PR #797."""

    def test_changelog_unreleased_extraction(self):
        """unreleased.rs missing imports for parent functions."""
        source = """use crate::engine::text;
use super::settings::*;

pub(super) fn find_next_section_start(lines: &[&str], aliases: &[String]) -> Option<usize> {
    lines.iter().position(|line| is_matching_next_section_heading(line, aliases))
}

pub(super) fn find_section_end(lines: &[&str], start: usize) -> usize {
    start + 1
}

pub fn count_unreleased_entries(content: &str, aliases: &[String]) -> usize {
    let lines: Vec<&str> = content.lines().collect();
    let start = match find_next_section_start(&lines, aliases) {
        Some(idx) => idx,
        None => return 0,
    };
    let end = find_section_end(&lines, start);
    lines[start + 1..end].iter().filter(|l| l.trim().starts_with("- ")).count()
}
"""
        moved_items = [{
            "name": "count_unreleased_entries",
            "kind": "fn",
            "source": """pub fn count_unreleased_entries(content: &str, aliases: &[String]) -> usize {
    let lines: Vec<&str> = content.lines().collect();
    let start = match find_next_section_start(&lines, aliases) {
        Some(idx) => idx,
        None => return 0,
    };
    let end = find_section_end(&lines, start);
    lines[start + 1..end].iter().filter(|l| l.trim().starts_with("- ")).count()
}""",
        }]
        result = resolve_imports(
            moved_items, source,
            "src/core/release/changelog/sections.rs",
            "src/core/release/changelog/sections/unreleased.rs",
        )
        imports = result["needed_imports"]
        import_text = "\n".join(imports)
        self.assertIn("find_next_section_start", import_text)
        self.assertIn("find_section_end", import_text)
        # Decompose case: should use super:: not crate::
        self.assertIn("use super::", import_text)

    def test_changelog_normalize_extraction_glob(self):
        """normalize_heading_label.rs missing glob-provided constant."""
        source = """use crate::engine::text;
use super::settings::*;

pub(crate) fn validate_section_content(body_lines: &[&str]) -> SectionContentStatus {
    let mut has_subsection_headers = false;
    for line in body_lines {
        let trimmed = line.trim();
        if KEEP_A_CHANGELOG_SUBSECTIONS.iter().any(|h| trimmed.starts_with(h)) {
            has_subsection_headers = true;
        }
    }
    SectionContentStatus::Empty
}
"""
        moved_items = [{
            "name": "validate_section_content",
            "kind": "fn",
            "source": """pub(crate) fn validate_section_content(body_lines: &[&str]) -> SectionContentStatus {
    let mut has_subsection_headers = false;
    for line in body_lines {
        let trimmed = line.trim();
        if KEEP_A_CHANGELOG_SUBSECTIONS.iter().any(|h| trimmed.starts_with(h)) {
            has_subsection_headers = true;
        }
    }
    SectionContentStatus::Empty
}""",
        }]
        result = resolve_imports(
            moved_items, source,
            "src/core/release/changelog/sections.rs",
            "src/core/release/changelog/sections/normalize_heading_label.rs",
        )
        imports = result["needed_imports"]
        import_text = "\n".join(imports)
        self.assertIn("settings::*", import_text)

    def test_version_local_files_trait(self):
        """version/default_pattern_for_file.rs missing FileSystem trait import."""
        source = """use crate::engine::local_files::{self, FileSystem};
use crate::engine::text;

pub fn read_local_version(local_path: &str, target: &VersionTarget) -> Option<String> {
    let full_path = format!("{}/{}", local_path, target.file);
    let content = local_files::local().read(std::path::Path::new(&full_path)).ok()?;
    text::extract_first(&content, &target.pattern)
}

pub fn get_all_versions(component: &Component) -> Vec<String> {
    let target = component.version_targets.as_ref().unwrap().first().unwrap();
    let full_path = format!("{}/{}", component.local_path, target.file);
    let content = local_files::local().read(std::path::Path::new(&full_path)).unwrap();
    vec![content]
}
"""
        moved_items = [{
            "name": "get_all_versions",
            "kind": "fn",
            "source": """pub fn get_all_versions(component: &Component) -> Vec<String> {
    let target = component.version_targets.as_ref().unwrap().first().unwrap();
    let full_path = format!("{}/{}", component.local_path, target.file);
    let content = local_files::local().read(std::path::Path::new(&full_path)).unwrap();
    vec![content]
}""",
        }]
        result = resolve_imports(
            moved_items, source,
            "src/core/release/version.rs",
            "src/core/release/version/default_pattern_for_file.rs",
        )
        imports = result["needed_imports"]
        import_text = "\n".join(imports)
        # Must carry the {self, FileSystem} import for local_files::local().read()
        self.assertIn("local_files", import_text)
        self.assertIn("FileSystem", import_text)


class TestFixImportPath(unittest.TestCase):
    def test_same_parent_keeps_super(self):
        result = fix_import_path(
            "use super::settings::*;",
            "src/core/changelog/sections.rs",
            "src/core/changelog/other.rs",
        )
        # Same parent — super:: stays
        self.assertEqual(result, "use super::settings::*;")

    def test_decompose_adds_extra_super(self):
        """When dest is child of source, super:: gains one more level."""
        result = fix_import_path(
            "use super::settings::*;",
            "src/core/changelog/sections.rs",
            "src/core/changelog/sections/types.rs",
        )
        # Decompose: super:: from source -> super::super:: from dest
        self.assertIn("super::super::settings::*", result)

    def test_different_parent_resolves_to_crate(self):
        result = fix_import_path(
            "use super::settings::CONST;",
            "src/core/changelog/sections.rs",
            "src/core/other/foo.rs",
        )
        self.assertIn("crate::", result)
        self.assertIn("settings", result)

    def test_crate_path_unchanged(self):
        result = fix_import_path(
            "use crate::engine::text;",
            "src/core/big.rs",
            "src/core/big/helpers.rs",
        )
        # crate:: paths are always valid
        self.assertEqual(result, "use crate::engine::text;")


class TestModuleIndex(unittest.TestCase):
    """Module index generation must put doc comments before mod declarations."""

    def test_doc_comments_precede_mod_declarations(self):
        remaining = """//! This module handles big things.
//!
//! It's very important.

use std::path::Path;

pub fn remaining_fn() {}
"""
        result = generate_module_index(
            [{"name": "helpers", "pub_items": []}, {"name": "types", "pub_items": []}],
            remaining,
        )
        content = result["content"]
        lines = content.split('\n')
        # Find positions
        first_doc = next(i for i, l in enumerate(lines) if l.strip().startswith("//!"))
        first_mod = next(i for i, l in enumerate(lines) if l.strip().startswith("mod "))
        self.assertLess(first_doc, first_mod,
                         "Doc comments must appear before mod declarations")

    def test_no_doc_comments_works_normally(self):
        result = generate_module_index(
            [{"name": "helpers", "pub_items": []}],
            "pub fn remaining() {}",
        )
        content = result["content"]
        self.assertIn("mod helpers;", content)
        self.assertIn("pub use helpers::*;", content)
        self.assertIn("pub fn remaining() {}", content)


if __name__ == "__main__":
    unittest.main()
