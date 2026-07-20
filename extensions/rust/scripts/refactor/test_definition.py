"""Tests for the `find_definition` refactor command (Rust struct discovery)."""

import unittest

from .definition import defines_struct, extract_struct_source, find_definition


PUB_STRUCT = """\
use std::collections::HashMap;

/// A widget.
#[derive(Debug, Default, Clone)]
pub struct Widget {
    pub id: u64,
    pub label: String,
    pub tags: Vec<String>,
}

impl Widget {
    pub fn new() -> Self {
        Self::default()
    }
}
"""

PUB_CRATE_STRUCT = """\
pub(crate) struct Internal {
    count: usize,
}
"""


class TestDefinesStruct(unittest.TestCase):
    def test_matches_pub_struct(self):
        self.assertTrue(defines_struct(PUB_STRUCT, "Widget"))

    def test_matches_pub_crate_struct(self):
        self.assertTrue(defines_struct(PUB_CRATE_STRUCT, "Internal"))

    def test_no_match_for_absent_struct(self):
        self.assertFalse(defines_struct(PUB_STRUCT, "Gadget"))

    def test_no_match_for_mere_usage(self):
        usage = "let w = Widget { id: 1, ..Default::default() };"
        self.assertFalse(defines_struct(usage, "Widget"))


class TestExtractStructSource(unittest.TestCase):
    def test_extracts_block_with_attrs_and_docs(self):
        source = extract_struct_source(PUB_STRUCT, "Widget")
        self.assertIsNotNone(source)
        # Leading doc comment and derive attribute are included.
        self.assertIn("/// A widget.", source)
        self.assertIn("#[derive(Debug, Default, Clone)]", source)
        self.assertIn("pub struct Widget {", source)
        # Body fields included, brace-balanced end.
        self.assertIn("pub tags: Vec<String>,", source)
        self.assertTrue(source.rstrip().endswith("}"))
        self.assertEqual(source.count("{"), source.count("}"))
        # The impl block after the struct must NOT be swept in.
        self.assertNotIn("fn new()", source)

    def test_returns_none_when_absent(self):
        self.assertIsNone(extract_struct_source(PUB_STRUCT, "Gadget"))


class TestFindDefinitionCommand(unittest.TestCase):
    def test_reports_definition_and_source(self):
        result = find_definition(
            {
                "struct_name": "Widget",
                "file_content": PUB_STRUCT,
                "file_path": "src/widget.rs",
            }
        )
        self.assertTrue(result["defines"])
        self.assertIn("pub struct Widget {", result["struct_source"])
        self.assertEqual(result["file_path"], "src/widget.rs")

    def test_reports_absence(self):
        result = find_definition(
            {
                "struct_name": "Gadget",
                "file_content": PUB_STRUCT,
                "file_path": "src/widget.rs",
            }
        )
        self.assertFalse(result["defines"])
        self.assertIsNone(result["struct_source"])
        self.assertEqual(result["file_path"], "src/widget.rs")


if __name__ == "__main__":
    unittest.main()
