#!/usr/bin/env python3
"""Lightweight Swift source fingerprinting for homeboy audit."""

from __future__ import annotations

import hashlib
import json
import re
import sys


DECL_RE = re.compile(
    r"^\s*(?:(?:@[A-Za-z_][\w.]*(?:\([^\n)]*\))?)\s*)*"
    r"(?P<mods>(?:(?:public|private|fileprivate|internal|open|final|static|class|actor)\s+)*)"
    r"(?P<kind>class|struct|enum|protocol|actor)\s+"
    r"(?P<name>[A-Za-z_][A-Za-z0-9_]*)"
    r"(?P<tail>[^\n{]*)",
    re.MULTILINE,
)

EXT_RE = re.compile(
    r"^\s*(?:(?:public|private|fileprivate|internal|open)\s+)*extension\s+"
    r"(?P<name>[A-Za-z_][A-Za-z0-9_.]*)"
    r"(?P<tail>[^\n{]*)",
    re.MULTILINE,
)

FUNC_RE = re.compile(
    r"^\s*(?:(?:@[A-Za-z_][\w.]*(?:\([^\n)]*\))?)\s*)*"
    r"(?P<mods>(?:(?:public|private|fileprivate|internal|open|static|class|override|mutating|nonmutating|async|rethrows|final)\s+)*)"
    r"func\s+(?P<name>`?[A-Za-z_][A-Za-z0-9_]*`?)\s*(?:<[^\n{]*>)?\s*\(",
    re.MULTILINE,
)

INIT_RE = re.compile(
    r"^\s*(?:(?:@[A-Za-z_][\w.]*(?:\([^\n)]*\))?)\s*)*"
    r"(?P<mods>(?:(?:public|private|fileprivate|internal|open|required|convenience|override)\s+)*)"
    r"init(?:\?|!)?\s*\(",
    re.MULTILINE,
)

PROPERTY_RE = re.compile(
    r"^\s*(?:(?:@[A-Za-z_][\w.]*(?:\([^\n)]*\))?)\s*)*"
    r"(?P<mods>(?:(?:public|private|fileprivate|internal|open|static|class|lazy|weak|unowned|@Published|@State|@Binding|@ObservedObject|@StateObject|@EnvironmentObject)\s+)*)"
    r"(?P<kind>let|var)\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\b",
    re.MULTILINE,
)

IMPORT_RE = re.compile(r"^\s*import\s+([A-Za-z_][A-Za-z0-9_.]*)", re.MULTILINE)
CALL_RE = re.compile(r"(?:\b|\.)([A-Za-z_][A-Za-z0-9_]*)\s*\(")

SKIP_CALLS = {
    "if",
    "for",
    "while",
    "switch",
    "guard",
    "return",
    "print",
    "String",
    "Int",
    "Double",
    "Float",
    "Bool",
    "Array",
    "Dictionary",
    "Set",
    "Optional",
    "Task",
    "DispatchQueue",
    "fatalError",
    "assert",
    "precondition",
    "super",
    "Self",
}


def load_input() -> tuple[str, str]:
    raw = sys.stdin.read()
    try:
        data = json.loads(raw)
    except json.JSONDecodeError:
        return raw, ""

    if isinstance(data, dict):
        return str(data.get("content", "")), str(data.get("file_path", ""))
    return raw, ""


def strip_comments(content: str) -> str:
    content = re.sub(r"/\*.*?\*/", "", content, flags=re.DOTALL)
    return re.sub(r"//.*", "", content)


def dedupe(items):
    seen = set()
    result = []
    for item in items:
        key = json.dumps(item, sort_keys=True) if isinstance(item, dict) else item
        if key in seen:
            continue
        seen.add(key)
        result.append(item)
    return result


def visibility_from_mods(mods: str) -> str:
    tokens = set(mods.split())
    if "open" in tokens:
        return "open"
    if "public" in tokens:
        return "public"
    if "private" in tokens:
        return "private"
    if "fileprivate" in tokens:
        return "fileprivate"
    return "internal"


def protocols_from_tail(tail: str) -> list[str]:
    if ":" not in tail:
        return []
    inherited = tail.split(":", 1)[1]
    inherited = re.sub(r"\bwhere\b.*", "", inherited).strip()
    return [part.strip().split(".")[-1] for part in inherited.split(",") if part.strip()]


def line_number(content: str, offset: int) -> int:
    return content.count("\n", 0, offset) + 1


def item_source(content: str, offset: int) -> str:
    brace_start = content.find("{", offset)
    line_end = content.find("\n", offset)
    if line_end == -1:
        line_end = len(content)
    if brace_start == -1 or brace_start > line_end:
        return content[offset:line_end]

    depth = 0
    for index in range(brace_start, len(content)):
        char = content[index]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return content[offset : index + 1]

    return content[offset:line_end]


def method_hash(content: str, offset: int) -> str:
    line_start = content.rfind("\n", 0, offset) + 1
    snippet = item_source(content, line_start)
    normalized = re.sub(r"\s+", " ", snippet).strip()
    return hashlib.sha256(normalized.encode()).hexdigest()[:16]


def main() -> None:
    content, file_path = load_input()
    code = strip_comments(content)

    declarations = []
    type_names = []
    implements = []
    extends = None
    for match in DECL_RE.finditer(code):
        kind = match.group("kind")
        name = match.group("name")
        tail = match.group("tail") or ""
        protocols = protocols_from_tail(tail)
        declarations.append(
            {
                "kind": kind,
                "name": name,
                "visibility": visibility_from_mods(match.group("mods") or ""),
                "line": line_number(code, match.start()),
                "implements": protocols,
            }
        )
        type_names.append(name)
        implements.extend(protocols)
        if kind == "class" and protocols and extends is None:
            extends = protocols[0]

    extensions = []
    for match in EXT_RE.finditer(code):
        protocols = protocols_from_tail(match.group("tail") or "")
        name = match.group("name").split(".")[-1]
        extensions.append(
            {
                "name": name,
                "visibility": visibility_from_mods(match.group(0)),
                "line": line_number(code, match.start()),
                "implements": protocols,
            }
        )
        implements.extend(protocols)

    methods = []
    visibility = {}
    method_hashes = {}
    structural_hashes = {}
    public_api = []
    for match in FUNC_RE.finditer(code):
        name = match.group("name").strip("`")
        if name.startswith("test"):
            continue
        methods.append(name)
        vis = visibility_from_mods(match.group("mods") or "")
        visibility[name] = vis
        item_hash = method_hash(code, match.start())
        method_hashes[name] = item_hash
        structural_hashes[name] = item_hash
        if vis in {"open", "public"}:
            public_api.append(name)

    init_count = 0
    for match in INIT_RE.finditer(code):
        init_count += 1
        name = "init" if init_count == 1 else f"init_{init_count}"
        methods.append(name)
        vis = visibility_from_mods(match.group("mods") or "")
        visibility[name] = vis
        if vis in {"open", "public"}:
            public_api.append(name)

    properties = []
    for match in PROPERTY_RE.finditer(code):
        name = match.group("name")
        properties.append(f"{match.group('kind')} {name}")
        vis = visibility_from_mods(match.group("mods") or "")
        if vis in {"open", "public"}:
            public_api.append(name)

    imports = IMPORT_RE.findall(code)
    internal_calls = sorted({m.group(1) for m in CALL_RE.finditer(code) if m.group(1) not in SKIP_CALLS})

    namespace = None
    if file_path:
        namespace = "/".join(file_path.split("/")[:-1]) or None

    result = {
        "methods": dedupe(methods),
        "type_name": type_names[0] if type_names else None,
        "type_names": dedupe(type_names),
        "type_declarations": declarations,
        "extensions": dedupe(extensions),
        "extends": extends,
        "implements": dedupe(implements),
        "registrations": [],
        "namespace": namespace,
        "imports": dedupe(imports),
        "properties": dedupe(properties),
        "method_hashes": method_hashes,
        "structural_hashes": structural_hashes,
        "visibility": visibility,
        "unused_parameters": [],
        "dead_code_markers": [],
        "internal_calls": internal_calls,
        "public_api": dedupe(public_api),
        "symbols": {
            "classes": [d["name"] for d in declarations if d["kind"] == "class"],
            "structs": [d["name"] for d in declarations if d["kind"] == "struct"],
            "enums": [d["name"] for d in declarations if d["kind"] == "enum"],
            "protocols": [d["name"] for d in declarations if d["kind"] == "protocol"],
            "actors": [d["name"] for d in declarations if d["kind"] == "actor"],
            "extensions": [e["name"] for e in extensions],
            "functions": dedupe(methods),
            "properties": dedupe(properties),
        },
    }

    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
