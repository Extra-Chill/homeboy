"""Tests for collapse_struct_defaults — the inverse of propagate_struct_fields.

Verifies that default-valued fields in struct instantiations are collapsed into
`..Default::default()`, and — critically — that non-default values are preserved
and risky literals are left untouched.
"""

import unittest

from .struct_fields import collapse_struct_defaults


STRUCT_SOURCE = """
pub struct AgentTaskOutcome {
    pub schema: String,
    pub task_id: String,
    pub status: AgentTaskOutcomeStatus,
    pub summary: Option<String>,
    pub artifacts: Vec<AgentTaskArtifact>,
    pub outputs: serde_json::Value,
    pub metadata: serde_json::Value,
}
"""


def _apply_edits(content: str, edits: list) -> str:
    """Apply replace-range edits to content (1-indexed inclusive lines).

    Mirrors what the Rust EditOp apply engine does, so tests assert on the
    resulting source, not just the edit objects.
    """
    lines = content.split('\n')
    # Apply from the bottom up so earlier line numbers stay valid.
    for edit in sorted(edits, key=lambda e: e['start_line'], reverse=True):
        start = edit['start_line'] - 1
        end = edit['end_line']  # exclusive slice end == inclusive last line
        replacement = edit['replacement'].split('\n')
        lines[start:end] = replacement
    return '\n'.join(lines)


class TestCollapseStructDefaults(unittest.TestCase):
    def _run(self, file_content, struct_source=STRUCT_SOURCE, name='AgentTaskOutcome'):
        return collapse_struct_defaults({
            'struct_name': name,
            'struct_source': struct_source,
            'file_content': file_content,
            'file_path': 'test.rs',
        })

    def test_collapses_trailing_default_fields(self):
        content = '''fn make() -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: schema(),
        task_id: "cook".to_string(),
        status: AgentTaskOutcomeStatus::Succeeded,
        summary: None,
        artifacts: Vec::new(),
        outputs: serde_json::Value::Null,
        metadata: serde_json::Value::Null,
    }
}'''
        result = self._run(content)
        self.assertEqual(result['instantiations_collapsed'], 1)
        applied = _apply_edits(content, result['edits'])
        self.assertIn('..Default::default()', applied)
        self.assertIn('task_id: "cook".to_string()', applied)
        self.assertIn('status: AgentTaskOutcomeStatus::Succeeded', applied)
        # The default-valued fields are gone.
        self.assertNotIn('summary: None', applied)
        self.assertNotIn('artifacts: Vec::new()', applied)
        self.assertNotIn('metadata: serde_json::Value::Null', applied)

    def test_preserves_interleaved_non_default_values(self):
        content = '''fn make() -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: schema(),
        task_id: "cook".to_string(),
        status: AgentTaskOutcomeStatus::Succeeded,
        summary: Some("done".to_string()),
        artifacts: Vec::new(),
        outputs: serde_json::json!({"k": "v"}),
        metadata: serde_json::Value::Null,
    }
}'''
        result = self._run(content)
        applied = _apply_edits(content, result['edits'])
        # Non-default values must survive.
        self.assertIn('summary: Some("done".to_string())', applied)
        self.assertIn('outputs: serde_json::json!({"k": "v"})', applied)
        # Defaults collapsed.
        self.assertNotIn('artifacts: Vec::new()', applied)
        self.assertNotIn('metadata: serde_json::Value::Null', applied)
        self.assertIn('..Default::default()', applied)

    def test_preserves_multiline_macro_value(self):
        content = '''fn make() -> AgentTaskOutcome {
    AgentTaskOutcome {
        task_id: "cook".to_string(),
        status: AgentTaskOutcomeStatus::Succeeded,
        summary: None,
        metadata: serde_json::json!({
            "run_id": "run-1",
            "nested": { "a": 1 }
        }),
    }
}'''
        result = self._run(content)
        applied = _apply_edits(content, result['edits'])
        # The multi-line json! must be preserved verbatim.
        self.assertIn('"run_id": "run-1"', applied)
        self.assertIn('"nested": { "a": 1 }', applied)
        self.assertNotIn('summary: None', applied)
        self.assertIn('..Default::default()', applied)

    def test_no_op_when_no_default_fields(self):
        content = '''fn make() -> AgentTaskOutcome {
    AgentTaskOutcome {
        task_id: "cook".to_string(),
        status: AgentTaskOutcomeStatus::Succeeded,
        summary: Some("x".to_string()),
        metadata: serde_json::json!({"k": 1}),
    }
}'''
        result = self._run(content)
        self.assertEqual(result['instantiations_collapsed'], 0)
        self.assertEqual(result['edits'], [])

    def test_skips_literal_that_already_spreads(self):
        content = '''fn make() -> AgentTaskOutcome {
    AgentTaskOutcome {
        task_id: "cook".to_string(),
        summary: None,
        ..Default::default()
    }
}'''
        result = self._run(content)
        self.assertEqual(result['instantiations_collapsed'], 0)

    def test_skips_literal_with_interspersed_comment(self):
        content = '''fn make() -> AgentTaskOutcome {
    AgentTaskOutcome {
        task_id: "cook".to_string(),
        // an explanatory comment
        summary: None,
        metadata: serde_json::Value::Null,
    }
}'''
        result = self._run(content)
        # Bail rather than orphan the comment.
        self.assertEqual(result['instantiations_collapsed'], 0)

    def test_accepts_alternate_default_spellings(self):
        content = '''fn make() -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: "".to_string(),
        task_id: "cook".to_string(),
        status: AgentTaskOutcomeStatus::Succeeded,
        artifacts: vec![],
    }
}'''
        result = self._run(content)
        applied = _apply_edits(content, result['edits'])
        # `"".to_string()` (String default) and `vec![]` (Vec default) collapse.
        self.assertNotIn('schema: ""', applied)
        self.assertNotIn('artifacts: vec![]', applied)
        self.assertIn('..Default::default()', applied)

    def test_does_not_collapse_unknown_type_fields(self):
        # `status` is an enum — no provable default spelling — so a value that
        # happens to look empty must never be collapsed.
        content = '''fn make() -> AgentTaskOutcome {
    AgentTaskOutcome {
        task_id: "cook".to_string(),
        status: AgentTaskOutcomeStatus::Succeeded,
        summary: None,
    }
}'''
        result = self._run(content)
        applied = _apply_edits(content, result['edits'])
        # status stays explicit; only summary collapses.
        self.assertIn('status: AgentTaskOutcomeStatus::Succeeded', applied)
        self.assertNotIn('summary: None', applied)

    def test_preserves_shorthand_fields(self):
        content = '''fn make(task_id: String) -> AgentTaskOutcome {
    AgentTaskOutcome {
        task_id,
        status: AgentTaskOutcomeStatus::Succeeded,
        summary: None,
    }
}'''
        result = self._run(content)
        applied = _apply_edits(content, result['edits'])
        self.assertIn('task_id,', applied)
        self.assertNotIn('summary: None', applied)


if __name__ == '__main__':
    unittest.main()
