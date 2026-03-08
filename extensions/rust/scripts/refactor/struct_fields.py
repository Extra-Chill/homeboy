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
