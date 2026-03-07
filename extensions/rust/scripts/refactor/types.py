"""Shared data types for the Rust refactor script."""

from dataclasses import dataclass


@dataclass
class ParsedItem:
    name: str
    kind: str
    start_line: int  # 1-indexed
    end_line: int    # 1-indexed, inclusive
    source: str
    visibility: str = ""
