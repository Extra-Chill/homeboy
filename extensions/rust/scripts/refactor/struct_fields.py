"""Struct field propagation — detect and fix missing fields in struct instantiations.

Given a struct definition and a file, finds all instantiations of that struct
and generates edits to add missing fields with sensible defaults.
"""

import re


def parse_struct_fields(struct_source: str) -> list[dict]:
    """Parse a Rust struct definition and extract field names, types, and defaults.

    Handles:
      - pub field: Type,
      - pub(crate) field: Type,
      - field: Type,
      - /// doc comments (skipped)
      - #[serde(default)] attributes → marks field as having a default
    """
    fields = []
    lines = struct_source.split('\n')
    has_default_attr = False

    for line in lines:
        stripped = line.strip()

        # Track #[serde(default)] or #[default] on the next field
        if stripped.startswith('#[') and 'default' in stripped.lower():
            has_default_attr = True
            continue

        # Skip doc comments, empty lines, braces
        if stripped.startswith('//') or stripped == '' or stripped in ('{', '}'):
            has_default_attr = False
            continue

        # Match field: pub name: Type, or name: Type,
        field_match = re.match(
            r'(?:pub(?:\([^)]*\))?\s+)?(\w+)\s*:\s*(.+?)\s*,?\s*$',
            stripped
        )
        if field_match:
            field_name = field_match.group(1)
            field_type = field_match.group(2).rstrip(',').strip()

            # Infer a sensible default from the type
            default_value = infer_default(field_type)

            fields.append({
                'name': field_name,
                'type': field_type,
                'default': default_value,
                'has_serde_default': has_default_attr,
            })
            has_default_attr = False

    return fields


def infer_default(rust_type: str) -> str:
    """Infer a sensible default value for a Rust type."""
    t = rust_type.strip()

    # Option<T> → None
    if t.startswith('Option<'):
        return 'None'

    # Vec<T> → vec![]
    if t.startswith('Vec<') or t == 'Vec':
        return 'vec![]'

    # HashMap/BTreeMap → T::new()
    if 'HashMap' in t or 'BTreeMap' in t:
        return 'std::collections::HashMap::new()' if 'HashMap' in t else 'std::collections::BTreeMap::new()'

    # HashSet/BTreeSet
    if 'HashSet' in t or 'BTreeSet' in t:
        return 'std::collections::HashSet::new()' if 'HashSet' in t else 'std::collections::BTreeSet::new()'

    # String → String::new()
    if t == 'String':
        return 'String::new()'

    # bool → false
    if t == 'bool':
        return 'false'

    # Numeric types → 0
    if t in ('u8', 'u16', 'u32', 'u64', 'u128', 'usize',
             'i8', 'i16', 'i32', 'i64', 'i128', 'isize',
             'f32', 'f64'):
        return '0'

    # Fallback: Default::default()
    return 'Default::default()'


def find_struct_instantiations(content: str, struct_name: str) -> list[dict]:
    """Find all instantiations of a struct in file content.

    Returns list of {start_line, end_line, fields_present, indent} for each.
    """
    instantiations = []
    lines = content.split('\n')

    for i, line in enumerate(lines):
        # Match: StructName { or StructName{
        pattern = re.escape(struct_name) + r'\s*\{'
        match = re.search(pattern, line)
        if not match:
            continue

        # Check this isn't a struct definition or function return type
        before = line[:match.start()].strip()
        if before.endswith('struct') or before.endswith('enum') or 'struct ' in before:
            continue
        # Skip function signatures: -> StructName {
        if '->' in before:
            continue
        # Skip type aliases, trait definitions
        if before.startswith('type ') or before.startswith('trait '):
            continue

        # Find the closing brace
        depth = 0
        found_open = False
        end_line = i
        for j in range(i, len(lines)):
            for ch in lines[j]:
                if ch == '{':
                    depth += 1
                    found_open = True
                elif ch == '}':
                    depth -= 1
            if found_open and depth == 0:
                end_line = j
                break

        # Extract field names present in this instantiation
        block = '\n'.join(lines[i:end_line + 1])
        fields_present = set()

        # Match explicit field assignments: `field_name: value`
        for fm in re.finditer(r'(\w+)\s*:', block):
            field_name = fm.group(1)
            # Skip the struct name itself and common keywords
            if field_name != struct_name and field_name not in ('pub', 'crate', 'self', 'super'):
                fields_present.add(field_name)

        # Match Rust shorthand field init: bare identifier followed by comma,
        # closing brace, or newline (e.g., `content,` or `language,`)
        # Process inner lines (skip the struct opener and closer)
        for j in range(i + 1, end_line):
            stripped = lines[j].strip()
            # Skip comments and empty lines
            if not stripped or stripped.startswith('//'):
                continue
            # A shorthand field is a bare identifier (possibly with trailing comma)
            # that has no colon (not `field: value`)
            shorthand_match = re.match(r'^(\w+)\s*,?\s*$', stripped)
            if shorthand_match and ':' not in stripped:
                field_name = shorthand_match.group(1)
                if field_name not in ('pub', 'crate', 'self', 'super', struct_name):
                    fields_present.add(field_name)

        # Detect indentation of fields inside the struct
        field_indent = '            '  # default 12 spaces
        for j in range(i + 1, end_line + 1):
            stripped = lines[j].strip()
            if stripped and not stripped.startswith('//') and ':' in stripped:
                leading = len(lines[j]) - len(lines[j].lstrip())
                field_indent = lines[j][:leading]
                break

        instantiations.append({
            'start_line': i + 1,  # 1-indexed
            'end_line': end_line + 1,
            'fields_present': fields_present,
            'indent': field_indent,
            'closing_brace_line': end_line + 1,
        })

    return instantiations


def propagate_struct_fields(data: dict) -> dict:
    """Given a struct's fields and a file, return edits to add missing fields.

    Input:
      struct_name: str — name of the struct
      struct_fields: list[{name, type, default}] — full field list (from definition)
      file_content: str — content of file to fix
      file_path: str — path for reporting

    Output:
      edits: list[{line, column, insert_text, description}]
    """
    struct_name = data['struct_name']
    struct_fields = data.get('struct_fields', [])
    file_content = data['file_content']
    file_path = data.get('file_path', '')

    # If struct_fields not provided, try to parse from struct_source
    if not struct_fields and 'struct_source' in data:
        struct_fields = parse_struct_fields(data['struct_source'])

    instantiations = find_struct_instantiations(file_content, struct_name)
    lines = file_content.split('\n')

    edits = []
    for inst in instantiations:
        missing = []
        for field in struct_fields:
            if field['name'] not in inst['fields_present']:
                missing.append(field)

        if not missing:
            continue

        # Insert missing fields before the closing brace
        insert_line = inst['closing_brace_line']
        indent = inst['indent']

        for field in missing:
            default = field.get('default', 'Default::default()')
            insert_text = f"{indent}{field['name']}: {default},"
            edits.append({
                'file': file_path,
                'line': insert_line,
                'insert_text': insert_text,
                'description': f"Add missing field `{field['name']}` to {struct_name} instantiation",
            })

    return {
        'edits': edits,
        'instantiations_found': len(instantiations),
        'instantiations_needing_fix': len([i for i in instantiations
                                            if any(f['name'] not in i['fields_present']
                                                   for f in struct_fields)]),
    }


# ============================================================================
# collapse_struct_defaults — the inverse of propagate_struct_fields.
#
# Given a struct definition and a file, find each struct instantiation and
# collapse fields whose value equals the type's default into a single trailing
# `..Default::default()`. Reuses parse_struct_fields (field defaults) and the
# brace-aware literal-finding approach. Conservative by construction: it only
# removes a field when its literal value textually matches a known default
# expression for that field's type, never touches a literal that already
# contains `..`, and leaves the whole literal untouched if no field is
# removable.
# ============================================================================

# Per-type set of literal expressions that are semantically the default value.
# The comparison is intentionally textual (no eval) and matches the forms that
# actually appear in hand-written Rust. `_default_exprs_for_type` returns the
# accepted spellings for a field's declared type.
def _normalize_expr(expr: str) -> str:
    """Collapse whitespace so multi-line/oddly-spaced values compare cleanly."""
    return ' '.join(expr.split()).strip().rstrip(',').strip()


def _default_exprs_for_type(rust_type: str) -> set:
    """Return the set of literal expressions that equal the default for a type.

    Only returns spellings we can prove are the default. Types we can't reason
    about return an empty set, so their fields are never collapsed.
    """
    t = rust_type.strip()

    if t.startswith('Option<'):
        return {'None'}

    if t.startswith('Vec<') or t == 'Vec':
        return {'Vec::new()', 'vec![]', 'Vec::default()', 'Default::default()'}

    if 'HashMap' in t:
        return {'HashMap::new()', 'std::collections::HashMap::new()',
                'HashMap::default()', 'Default::default()'}
    if 'BTreeMap' in t:
        return {'BTreeMap::new()', 'std::collections::BTreeMap::new()',
                'BTreeMap::default()', 'Default::default()'}
    if 'HashSet' in t:
        return {'HashSet::new()', 'std::collections::HashSet::new()',
                'HashSet::default()', 'Default::default()'}
    if 'BTreeSet' in t:
        return {'BTreeSet::new()', 'std::collections::BTreeSet::new()',
                'BTreeSet::default()', 'Default::default()'}

    if t == 'String':
        return {'String::new()', 'String::default()', '"".to_string()',
                '"".to_owned()', 'String::from("")', 'Default::default()'}

    if t == 'bool':
        return {'false', 'bool::default()', 'Default::default()'}

    if t in ('u8', 'u16', 'u32', 'u64', 'u128', 'usize',
             'i8', 'i16', 'i32', 'i64', 'i128', 'isize'):
        return {'0', f'{t}::default()', 'Default::default()'}
    if t in ('f32', 'f64'):
        return {'0.0', '0.', f'{t}::default()', 'Default::default()'}

    if t == 'serde_json::Value' or t == 'Value':
        return {'serde_json::Value::Null', 'Value::Null', 'Default::default()'}

    # Unknown type: no provable default spelling.
    return set()


def _split_top_level_fields(inner_lines: list, start_index: int) -> list:
    """Split a struct literal body into top-level `name: value` field entries.

    `inner_lines` are the raw source lines strictly between the opening and
    closing brace of the literal. Returns a list of dicts:
      {name, value, first_line, last_line, is_spread, is_shorthand}
    where line numbers are ABSOLUTE (1-indexed), offset by start_index (the
    0-indexed line number of the first inner line).

    Brace/paren/bracket depth is tracked so multi-line values (e.g.
    `metadata: json!({ ... })`) are captured as a single field. Returns None
    if the body cannot be cleanly parsed (bail — never guess).
    """
    fields = []
    depth = 0
    buf = []
    buf_first = None

    for offset, raw in enumerate(inner_lines):
        abs_line = start_index + offset + 1  # 1-indexed absolute
        if buf_first is None and raw.strip():
            buf_first = abs_line
        buf.append((abs_line, raw))

        for ch in raw:
            if ch in '([{':
                depth += 1
            elif ch in ')]}':
                depth -= 1

        # A field entry terminates at a top-level trailing comma.
        stripped = raw.rstrip()
        if depth == 0 and stripped.endswith(','):
            entry = _classify_field_entry(buf, buf_first, abs_line)
            if entry is None:
                return None
            fields.append(entry)
            buf = []
            buf_first = None

    # Trailing entry without a comma (last field before closing brace).
    if any(raw.strip() for _, raw in buf):
        if depth != 0:
            return None
        entry = _classify_field_entry(buf, buf_first, buf[-1][0])
        if entry is None:
            return None
        fields.append(entry)

    return fields


def _classify_field_entry(buf, first_line, last_line) -> dict:
    """Turn a buffered set of (line, text) tuples into a field entry dict."""
    text = '\n'.join(t for _, t in buf).strip()
    if not text:
        return None

    # Skip comment-only buffers by ignoring leading comment lines.
    non_comment = [t for _, t in buf if t.strip() and not t.strip().startswith('//')]
    if not non_comment:
        # Comment-only region between fields — represent as a passthrough.
        return {'name': None, 'value': None, 'first_line': first_line,
                'last_line': last_line, 'is_spread': False, 'is_shorthand': False,
                'passthrough': True}

    joined = _normalize_expr(text)

    # Spread base: `..expr`
    if joined.startswith('..'):
        return {'name': None, 'value': joined, 'first_line': first_line,
                'last_line': last_line, 'is_spread': True, 'is_shorthand': False,
                'passthrough': False}

    # `name: value`
    m = re.match(r'^(\w+)\s*:\s*(.*)$', joined, re.DOTALL)
    if m:
        return {'name': m.group(1), 'value': _normalize_expr(m.group(2)),
                'first_line': first_line, 'last_line': last_line,
                'is_spread': False, 'is_shorthand': False, 'passthrough': False}

    # Shorthand: bare `name`
    m = re.match(r'^(\w+)$', joined)
    if m:
        return {'name': m.group(1), 'value': m.group(1),
                'first_line': first_line, 'last_line': last_line,
                'is_spread': False, 'is_shorthand': True, 'passthrough': False}

    # Anything else we don't understand — bail.
    return None


def collapse_struct_defaults(data: dict) -> dict:
    """Collapse default-valued fields in struct instantiations into
    `..Default::default()`.

    Input:
      struct_name: str
      struct_source: str — the struct definition block (for field types/defaults)
      file_content: str
      file_path: str

    Output:
      edits: list[{file, start_line, end_line, replacement, description}]
        (replace-range edits: lines [start_line, end_line] inclusive, 1-indexed,
         become `replacement`)
      instantiations_found, instantiations_collapsed
    """
    struct_name = data['struct_name']
    file_content = data['file_content']
    file_path = data.get('file_path', '')
    struct_fields = data.get('struct_fields', [])
    if not struct_fields and 'struct_source' in data:
        struct_fields = parse_struct_fields(data['struct_source'])

    field_types = {f['name']: f['type'] for f in struct_fields}

    lines = file_content.split('\n')
    instantiations = find_struct_instantiations(file_content, struct_name)

    edits = []
    collapsed = 0

    for inst in instantiations:
        open_line = inst['start_line'] - 1        # 0-indexed line with `Struct {`
        close_line = inst['closing_brace_line'] - 1  # 0-indexed line with `}`

        # Single-line literals are left alone (rare, low value, higher risk).
        if close_line <= open_line:
            continue

        inner = lines[open_line + 1:close_line]
        parsed = _split_top_level_fields(inner, open_line + 1)
        if parsed is None:
            continue

        # Never touch a literal that already spreads (`..base`).
        if any(f['is_spread'] for f in parsed):
            continue

        removable = []   # field entries safe to drop
        for f in parsed:
            if f.get('passthrough'):
                # A comment between fields blocks a clean collapse — bail on
                # this literal to avoid orphaning comments.
                removable = []
                break
            if f['is_shorthand']:
                continue
            ftype = field_types.get(f['name'])
            if ftype is None:
                continue
            accepted = _default_exprs_for_type(ftype)
            if f['value'] in accepted:
                removable.append(f)

        if not removable:
            continue

        # Keep the fields that are NOT removable, in original order, then append
        # `..Default::default()`. Rebuild the whole literal body deterministically.
        kept = [f for f in parsed if f not in removable and not f.get('passthrough')]

        # Indentation from the first inner field line.
        indent = inst['indent']

        new_body_lines = []
        for f in kept:
            # Re-emit the field's ORIGINAL source lines verbatim to preserve
            # exact formatting/values (multi-line json!, etc.).
            for ln in range(f['first_line'] - 1, f['last_line']):
                new_body_lines.append(lines[ln])
        new_body_lines.append(f"{indent}..Default::default()")

        # Replace the inner region (between braces) with the rebuilt body.
        # start/end are 1-indexed inclusive lines to replace.
        edits.append({
            'file': file_path,
            'start_line': open_line + 2,   # first inner line (1-indexed)
            'end_line': close_line,        # last inner line (1-indexed)
            'replacement': '\n'.join(new_body_lines),
            'description': (
                f"Collapse {len(removable)} default-valued field(s) "
                f"into ..Default::default() in {struct_name} instantiation"
            ),
        })
        collapsed += 1

    return {
        'edits': edits,
        'instantiations_found': len(instantiations),
        'instantiations_collapsed': collapsed,
    }
