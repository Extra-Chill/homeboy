"""Tests for import resolution in decompose refactoring."""

import unittest
from .imports import resolve_imports, extract_use_names, fix_import_path


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
        # Both items are being moved — no import needed for helper
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
        # helper is being moved too, so no import should be generated for it
        self.assertFalse(any("helper" in i for i in imports))


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
        # The glob import should be carried forward since
        # KEEP_A_CHANGELOG_SUBSECTIONS is unresolved
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
        # MY_CONST is a local definition, so it's resolved by Phase 2.
        # The glob import should NOT be carried.
        self.assertFalse(any("::*" in i for i in imports))


class TestResolveImportsRealWorldRegression(unittest.TestCase):
    """Regression test for the changelog sections decomposition bug.

    When sections.rs was decomposed, the extracted unreleased.rs called
    find_next_section_start() and find_section_end() without importing them,
    and normalize_heading_label.rs referenced KEEP_A_CHANGELOG_SUBSECTIONS
    which came from a glob import (use super::settings::*).
    """

    def test_changelog_unreleased_extraction(self):
        """Reproduces: unreleased.rs missing imports for parent functions."""
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

        # These functions stay in sections.rs — the extracted code must import them
        self.assertIn("find_next_section_start", import_text,
                       "Extracted code calls find_next_section_start but no import generated")
        self.assertIn("find_section_end", import_text,
                       "Extracted code calls find_section_end but no import generated")

    def test_changelog_normalize_extraction_glob(self):
        """Reproduces: normalize_heading_label.rs missing glob-provided constant."""
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

        # KEEP_A_CHANGELOG_SUBSECTIONS comes from `use super::settings::*;`
        # The glob import must be carried forward
        self.assertIn("settings::*", import_text,
                       "Glob import should be carried for unresolved KEEP_A_CHANGELOG_SUBSECTIONS")


class TestFixImportPath(unittest.TestCase):
    def test_same_parent_keeps_super(self):
        result = fix_import_path(
            "use super::settings::*;",
            "src/core/changelog/sections.rs",
            "src/core/changelog/sections/types.rs",
        )
        # Different parent, super:: should be resolved
        self.assertIn("settings", result)

    def test_different_parent_resolves_super(self):
        result = fix_import_path(
            "use super::settings::CONST;",
            "src/core/changelog/sections.rs",
            "src/core/other/foo.rs",
        )
        self.assertIn("crate::", result)
        self.assertIn("settings", result)


if __name__ == "__main__":
    unittest.main()
