"""Tests for import resolution and visibility in decompose refactoring."""

import unittest
from .imports import resolve_imports, extract_use_names, fix_import_path
from .module_index import generate_module_index
from .visibility import adjust_visibility


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

    def test_decompose_prefers_specific_child_module_over_parent_barrel(self):
        """If ownership of a same-file symbol is obvious, import from the sibling child module."""
        source = """pub fn build_plan() {}
pub fn validate_plan() { build_plan(); }
"""
        moved_items = [{
            "name": "validate_plan",
            "kind": "fn",
            "source": "pub fn validate_plan() { build_plan(); }",
        }]
        result = resolve_imports(
            moved_items,
            source,
            "src/core/refactor/decompose.rs",
            "src/core/refactor/decompose/validate.rs",
        )
        import_text = "\n".join(result["needed_imports"])
        self.assertIn("use super::build::build_plan;", import_text)

    def test_decompose_falls_back_to_parent_when_no_clear_child_owner(self):
        source = """pub fn helper() {}
pub fn moved() { helper(); }
"""
        moved_items = [{
            "name": "moved",
            "kind": "fn",
            "source": "pub fn moved() { helper(); }",
        }]
        result = resolve_imports(
            moved_items,
            source,
            "src/core/foo.rs",
            "src/core/foo/bar.rs",
        )
        import_text = "\n".join(result["needed_imports"])
        self.assertIn("use super::helper;", import_text)


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

    def test_ignores_foreign_use_lines_inside_raw_string_fixture(self):
        """Raw-string fixtures must not leak PHP imports into Rust destination files."""
        source = r'''use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use crate::error::{Error, Result};

pub struct Grammar {
    pub blocks: BlockSyntax,
}

pub struct BlockSyntax {
    pub open: String,
    pub close: String,
}

#[cfg(test)]
mod tests {
    #[test]
    fn load_and_use_php_grammar() {
        let sample = r#"<?php
namespace ExamplePlugin\Abilities;

use WP_UnitTestCase;
use ExamplePlugin\Core\Pipeline;

class PipelineAbilities extends BaseAbilities {}
"#;
        assert!(sample.contains("Pipeline"));
    }
}
'''
        moved_items = [{
            "name": "BlockSyntax",
            "kind": "struct",
            "source": '''pub struct BlockSyntax {
    pub open: String,
    pub close: String,
}''',
        }]
        result = resolve_imports(
            moved_items,
            source,
            "src/core/extension/grammar.rs",
            "src/core/extension/grammar/block_syntax.rs",
        )
        imports = result["needed_imports"]
        import_text = "\n".join(imports)
        self.assertEqual(import_text, "")
        self.assertNotIn("WP_UnitTestCase", import_text)
        self.assertNotIn("ExamplePlugin\\Core\\Pipeline", import_text)

    def test_ignores_comment_mentions_of_source_definitions(self):
        """Names mentioned in comments/docs must not trigger same-module imports."""
        source = """pub fn helper() {}

pub fn moved() {
    // helper should maybe be called later
    let x = 1;
}
"""
        moved_items = [{
            "name": "moved",
            "kind": "fn",
            "source": """pub fn moved() {
    // helper should maybe be called later
    let x = 1;
}""",
        }]
        result = resolve_imports(
            moved_items,
            source,
            "src/core/foo.rs",
            "src/core/foo/moved.rs",
        )
        import_text = "\n".join(result["needed_imports"])
        self.assertNotIn("helper", import_text)

    def test_ignores_string_mentions_of_source_definitions(self):
        """Names inside string literals must not trigger same-module imports."""
        source = '''pub fn helper() {}

pub fn moved() {
    let doc = "call helper maybe";
    println!("{}", doc);
}
'''
        moved_items = [{
            "name": "moved",
            "kind": "fn",
            "source": '''pub fn moved() {
    let doc = "call helper maybe";
    println!("{}", doc);
}''',
        }]
        result = resolve_imports(
            moved_items,
            source,
            "src/core/foo.rs",
            "src/core/foo/moved.rs",
        )
        import_text = "\n".join(result["needed_imports"])
        self.assertNotIn("helper", import_text)

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


class TestVisibilityDecompose(unittest.TestCase):
    """Visibility adjuster must upgrade pub(crate) → pub in decompose case."""

    def test_pub_crate_upgraded_to_pub_in_decompose(self):
        """When decomposing, pub(crate) items need pub so glob re-export works."""
        items = [{
            "source": "pub(crate) fn do_thing() -> bool {\n    true\n}",
            "visibility": "pub(crate)",
            "kind": "function",
        }]
        result = adjust_visibility(
            items,
            "src/core/big.rs",
            "src/core/big/helpers.rs",
        )
        adjusted = result["items"]
        self.assertEqual(len(adjusted), 1)
        self.assertTrue(adjusted[0]["changed"])
        self.assertEqual(adjusted[0]["new_visibility"], "pub")
        self.assertIn("pub fn do_thing()", adjusted[0]["source"])
        self.assertNotIn("pub(crate)", adjusted[0]["source"])

    def test_pub_crate_struct_upgraded_in_decompose(self):
        """pub(crate) structs also need upgrading."""
        items = [{
            "source": "pub(crate) struct Config {\n    pub name: String,\n}",
            "visibility": "pub(crate)",
            "kind": "struct",
        }]
        result = adjust_visibility(
            items,
            "src/core/engine.rs",
            "src/core/engine/config.rs",
        )
        adjusted = result["items"]
        self.assertTrue(adjusted[0]["changed"])
        self.assertEqual(adjusted[0]["new_visibility"], "pub")
        self.assertIn("pub struct Config", adjusted[0]["source"])

    def test_pub_crate_NOT_upgraded_in_regular_move(self):
        """In a regular move (not decompose), pub(crate) stays unchanged."""
        items = [{
            "source": "pub(crate) fn helper() {}",
            "visibility": "pub(crate)",
            "kind": "function",
        }]
        result = adjust_visibility(
            items,
            "src/core/foo.rs",
            "src/core/bar.rs",
        )
        adjusted = result["items"]
        self.assertFalse(adjusted[0]["changed"])
        self.assertEqual(adjusted[0]["new_visibility"], "pub(crate)")

    def test_private_still_gets_pub_crate_in_decompose(self):
        """Private items still get pub(crate) even during decompose."""
        items = [{
            "source": "fn internal() {}",
            "visibility": "",
            "kind": "function",
        }]
        result = adjust_visibility(
            items,
            "src/core/big.rs",
            "src/core/big/helpers.rs",
        )
        adjusted = result["items"]
        self.assertTrue(adjusted[0]["changed"])
        self.assertEqual(adjusted[0]["new_visibility"], "pub(crate)")

    def test_impl_blocks_unchanged_in_decompose(self):
        """impl blocks have no visibility of their own."""
        items = [{
            "source": "impl Foo {\n    pub fn bar() {}\n}",
            "visibility": "",
            "kind": "impl",
        }]
        result = adjust_visibility(
            items,
            "src/core/big.rs",
            "src/core/big/helpers.rs",
        )
        adjusted = result["items"]
        self.assertFalse(adjusted[0]["changed"])

    def test_pub_items_unchanged_in_decompose(self):
        """Already pub items don't need upgrading."""
        items = [{
            "source": "pub fn already_public() {}",
            "visibility": "pub",
            "kind": "function",
        }]
        result = adjust_visibility(
            items,
            "src/core/big.rs",
            "src/core/big/helpers.rs",
        )
        adjusted = result["items"]
        self.assertFalse(adjusted[0]["changed"])
        self.assertEqual(adjusted[0]["new_visibility"], "pub")


class TestStandaloneTraitImports(unittest.TestCase):
    """Phase 1b: standalone trait imports (not in {self, Trait} groups) must be carried."""

    def test_standalone_trait_import_carried(self):
        """use crate::foo::MyTrait; should be carried when trait methods are used."""
        source = """use crate::engine::local_files::FileSystem;

pub fn read_file(path: &str) -> String {
    let fs = get_filesystem();
    fs.read(path).unwrap()
}
"""
        moved_items = [{
            "name": "read_file",
            "kind": "fn",
            "source": """pub fn read_file(path: &str) -> String {
    let fs = get_filesystem();
    fs.read(path).unwrap()
}""",
        }]
        result = resolve_imports(
            moved_items, source,
            "src/core/big.rs",
            "src/core/big/helpers.rs",
        )
        imports = result["needed_imports"]
        import_text = "\n".join(imports)
        # FileSystem is PascalCase, not used literally in moved code,
        # so Phase 1b should carry it as a trait import
        self.assertIn("FileSystem", import_text)

    def test_screaming_case_not_treated_as_trait(self):
        """SCREAMING_CASE constants should not be carried as trait imports."""
        source = """use crate::constants::MAX_RETRIES;

pub fn try_connect() -> bool {
    true
}
"""
        moved_items = [{
            "name": "try_connect",
            "kind": "fn",
            "source": "pub fn try_connect() -> bool { true }",
        }]
        result = resolve_imports(
            moved_items, source,
            "src/core/big.rs",
            "src/core/big/helpers.rs",
        )
        imports = result["needed_imports"]
        import_text = "\n".join(imports)
        # SCREAMING_CASE should NOT be treated as a trait
        self.assertNotIn("MAX_RETRIES", import_text)

    def test_type_used_literally_not_double_carried(self):
        """If a PascalCase name IS used literally, Phase 1 handles it — no double carry."""
        source = """use crate::types::Config;

pub fn build_config() -> Config {
    Config::default()
}
"""
        moved_items = [{
            "name": "build_config",
            "kind": "fn",
            "source": """pub fn build_config() -> Config {
    Config::default()
}""",
        }]
        result = resolve_imports(
            moved_items, source,
            "src/core/big.rs",
            "src/core/big/helpers.rs",
        )
        imports = result["needed_imports"]
        # Should appear exactly once (from Phase 1, not duplicated by Phase 1b)
        config_imports = [i for i in imports if "Config" in i]
        self.assertEqual(len(config_imports), 1)

    def test_multiple_trait_imports_carried(self):
        """Multiple standalone trait imports should all be carried."""
        source = """use std::io::Read;
use std::io::Write;
use std::path::Path;

pub fn copy_data(input: &mut impl Read, output: &mut impl Write) {
    let mut buf = [0u8; 1024];
    loop {
        let n = input.read(&mut buf).unwrap();
        if n == 0 { break; }
        output.write_all(&buf[..n]).unwrap();
    }
}
"""
        # Note: Read and Write appear in `impl Read` and `impl Write` in the
        # function signature, so Phase 1 would catch them. Let's test the case
        # where they DON'T appear literally.
        moved_items = [{
            "name": "copy_data",
            "kind": "fn",
            "source": """pub fn copy_data(input: &mut dyn Any, output: &mut dyn Any) {
    let mut buf = [0u8; 1024];
    loop {
        let n = input.read(&mut buf).unwrap();
        if n == 0 { break; }
        output.write_all(&buf[..n]).unwrap();
    }
}""",
        }]
        result = resolve_imports(
            moved_items, source,
            "src/core/big.rs",
            "src/core/big/helpers.rs",
        )
        imports = result["needed_imports"]
        import_text = "\n".join(imports)
        # Path IS used literally (via Path::new etc), so Phase 1 won't catch it
        # unless it's in the source. Read and Write are not used literally.
        self.assertIn("Read", import_text)
        self.assertIn("Write", import_text)

    def test_grouped_pascalcase_imports_not_carried_by_trait_fallback(self):
        """Grouped imports should not be hoisted by the broad trait-like fallback."""
        source = """use serde::{Deserialize, Serialize};

pub fn helper() -> usize {
    1
}
"""
        moved_items = [{
            "name": "helper",
            "kind": "fn",
            "source": "pub fn helper() -> usize { 1 }",
        }]
        result = resolve_imports(
            moved_items,
            source,
            "src/core/foo.rs",
            "src/core/foo/helpers.rs",
        )
        import_text = "\n".join(result["needed_imports"])
        self.assertNotIn("Deserialize", import_text)
        self.assertNotIn("Serialize", import_text)


if __name__ == "__main__":
    unittest.main()
