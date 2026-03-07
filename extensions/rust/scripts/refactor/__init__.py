"""Rust refactor script — language-specific parsing for homeboy refactor move.

Receives JSON commands on stdin, outputs JSON results on stdout.

Commands:
  parse_items              — Parse all top-level items in a Rust source file
  resolve_imports          — Determine what imports the destination needs
  find_related_tests       — Find test functions related to named items
  adjust_visibility        — Adjust visibility for cross-module moves
  rewrite_caller_imports   — Rewrite import paths in caller files
  propagate_struct_fields  — Add missing fields to struct instantiations
"""
