#!/usr/bin/env bash
# Rust fingerprint script for homeboy audit.
#
# Input (JSON on stdin):
#   {"file_path": "src/commands/deploy.rs", "content": "..."}
#
# Output (JSON on stdout):
#   {"methods": [...], "type_name": "...", "implements": [...],
#    "registrations": [], "namespace": "...", "imports": [...]}
#
# Extracts structural information from Rust source files using text matching.
# Does not require a Rust parser — uses grep/sed for speed.

set -euo pipefail

# Read stdin into variable
INPUT=$(cat)

# Extract content from JSON — use python3 for reliable JSON parsing
CONTENT=$(printf '%s' "$INPUT" | python3 -c "
import json, sys, re

data = json.load(sys.stdin)
content = data['content']
file_path = data['file_path']

# Strip #[cfg(test)] mod tests { ... } block to exclude test functions.
# Find the last #[cfg(test)] and remove everything from there to end of file.
cfg_test = re.search(r'#\[cfg\(test\)\]', content)
if cfg_test:
    content = content[:cfg_test.start()]

# --- Methods ---
# Match fn declarations: pub fn name, fn name, pub async fn name, pub(crate) fn name
methods = []
for m in re.finditer(r'(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)', content):
    name = m.group(1)
    # Skip test functions and test modules
    if name.startswith('test_') or name == 'tests':
        continue
    methods.append(name)

# Deduplicate while preserving order
seen = set()
methods = [m for m in methods if m not in seen and not seen.add(m)]

# --- Type name ---
# Primary struct or enum in the file (first pub struct/enum, or first struct/enum)
type_name = None
pub_types = re.findall(r'pub\s+(?:struct|enum)\s+(\w+)', content)
all_types = re.findall(r'(?:pub\s+)?(?:struct|enum)\s+(\w+)', content)
if pub_types:
    type_name = pub_types[0]
elif all_types:
    type_name = all_types[0]

# --- Implements ---
# Match impl blocks: impl Trait for Type, impl<T> Trait for Type
implements = []
for m in re.finditer(r'impl(?:<[^>]*>)?\s+(\w+(?:::\w+)*)\s+for\s+\w+', content):
    trait_name = m.group(1)
    # Use just the last segment for common traits
    implements.append(trait_name.split('::')[-1])
# Deduplicate
seen = set()
implements = [i for i in implements if i not in seen and not seen.add(i)]

# --- Registrations ---
# Match macro invocations that look like registration patterns
registrations = []
for m in re.finditer(r'(\w+)!\s*\(', content):
    macro_name = m.group(1)
    # Skip common non-registration macros
    skip = {'println', 'eprintln', 'format', 'vec', 'assert', 'assert_eq',
            'assert_ne', 'panic', 'todo', 'unimplemented', 'cfg', 'derive',
            'include', 'include_str', 'include_bytes', 'concat', 'stringify',
            'env', 'option_env', 'compile_error', 'write', 'writeln',
            'matches', 'dbg', 'debug_assert', 'debug_assert_eq',
            'debug_assert_ne', 'unreachable', 'cfg_if', 'lazy_static',
            'thread_local', 'once_cell', 'macro_rules', 'serde_json',
            'if_chain', 'bail', 'anyhow', 'ensure', 'Ok', 'Err',
            'Some', 'None', 'Box', 'Arc', 'Rc', 'RefCell', 'Mutex',
            'map', 'hashmap', 'btreemap', 'hashset'}
    if macro_name not in skip and not macro_name.startswith('test'):
        registrations.append(macro_name)
# Deduplicate
seen = set()
registrations = [r for r in registrations if r not in seen and not seen.add(r)]

# --- Namespace ---
# Infer from use crate:: patterns — most common prefix
crate_uses = re.findall(r'use\s+crate::(\w+)', content)
if crate_uses:
    # Count frequency of first path segment
    from collections import Counter
    counts = Counter(crate_uses)
    most_common = counts.most_common(1)[0][0]
    namespace = f'crate::{most_common}'
else:
    # Try to infer from file path
    parts = file_path.replace('.rs', '').split('/')
    if len(parts) > 1:
        namespace = 'crate::' + '::'.join(parts[1:-1]) if len(parts) > 2 else 'crate::' + parts[-1]
    else:
        namespace = None

# --- Imports ---
# Collect all use statements
imports = []
for m in re.finditer(r'use\s+((?:crate|super|self|std|serde|clap|regex|chrono|tokio|anyhow)\S+);', content):
    imports.append(m.group(1))
# Also match use with braces: use crate::foo::{bar, baz};
for m in re.finditer(r'use\s+((?:crate|super|self)\S+::\{[^}]+\});', content):
    imports.append(m.group(1))
# Deduplicate
seen = set()
imports = [i for i in imports if i not in seen and not seen.add(i)]

# --- Method Hashes (for duplication detection) ---
# Extract function bodies, normalize whitespace, hash with SHA-256.
# Only hashes top-level functions (not methods inside impl blocks).
import hashlib

method_hashes = {}
# Find top-level fn declarations (zero indentation)
lines = content.split('\n')
i = 0
while i < len(lines):
    line = lines[i]
    # Skip indented lines (methods inside impl/struct/etc.)
    if line and line[0] in (' ', '\t'):
        i += 1
        continue
    # Match fn declaration at start of line (with optional pub/async/unsafe/const)
    fn_match = re.match(r'(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+(\w+)', line)
    if not fn_match:
        i += 1
        continue
    fn_name = fn_match.group(1)
    if fn_name.startswith('test_') or fn_name == 'tests':
        i += 1
        continue
    # Find the opening brace
    brace_depth = 0
    found_open = False
    body_lines = []
    j = i
    while j < len(lines):
        for ch in lines[j]:
            if ch == '{':
                brace_depth += 1
                found_open = True
            elif ch == '}':
                brace_depth -= 1
        body_lines.append(lines[j])
        if found_open and brace_depth == 0:
            break
        j += 1
    if body_lines:
        # Normalize: join, collapse whitespace, strip
        body_text = ' '.join(body_lines)
        normalized = re.sub(r'\s+', ' ', body_text).strip()
        body_hash = hashlib.sha256(normalized.encode()).hexdigest()[:16]
        method_hashes[fn_name] = body_hash
    i = j + 1

# --- Structural Hashes (for near-duplicate detection) ---
# Same body extraction as method_hashes, but identifiers and literals are
# replaced with positional placeholders before hashing.  Two functions that
# differ only in variable names, constant references, or string values will
# produce the same structural hash.

def structural_normalize(text):
    # Replace identifiers and literals with positional tokens.
    # Remove the fn signature line (keeps only the body)
    brace_idx = text.find('{')
    if brace_idx >= 0:
        text = text[brace_idx:]

    # Replace string literals with STR (use chr(34) to avoid breaking bash double-quoting)
    dq = chr(34)
    text = re.sub(dq + '[^' + dq + ']*' + dq, 'STR', text)
    # Replace char literals with CHR
    text = re.sub(chr(39) + '[^' + chr(39) + ']*' + chr(39), 'CHR', text)
    # Replace numeric literals (integers, floats) with NUM
    text = re.sub(r'\b\d[\d_]*(?:\.\d[\d_]*)?\b', 'NUM', text)

    # Replace identifiers with positional tokens.
    # Collect unique identifiers in order of appearance, map to ID_N.
    # Preserve Rust keywords as-is (they define structure).
    rust_keywords = {
        'as', 'async', 'await', 'break', 'const', 'continue', 'crate',
        'dyn', 'else', 'enum', 'extern', 'false', 'fn', 'for', 'if',
        'impl', 'in', 'let', 'loop', 'match', 'mod', 'move', 'mut',
        'pub', 'ref', 'return', 'self', 'Self', 'static', 'struct',
        'super', 'trait', 'true', 'type', 'unsafe', 'use', 'where',
        'while', 'yield',
        # Common types/macros kept as structural markers
        'Some', 'None', 'Ok', 'Err', 'Result', 'Option', 'Vec',
        'String', 'Box', 'Arc', 'Rc', 'HashMap', 'HashSet',
        'bool', 'u8', 'u16', 'u32', 'u64', 'u128', 'usize',
        'i8', 'i16', 'i32', 'i64', 'i128', 'isize',
        'f32', 'f64', 'str', 'char',
    }

    id_map = {}
    id_counter = [0]

    def replace_id(m):
        word = m.group(0)
        if word in rust_keywords:
            return word
        if word not in id_map:
            id_map[word] = f'ID_{id_counter[0]}'
            id_counter[0] += 1
        return id_map[word]

    text = re.sub(r'\b[a-zA-Z_]\w*\b', replace_id, text)

    # Collapse whitespace
    text = re.sub(r'\s+', ' ', text).strip()
    return text

structural_hashes = {}
# Re-extract bodies and compute structural hashes
lines = content.split('\n')
i = 0
while i < len(lines):
    line = lines[i]
    if line and line[0] in (' ', '\t'):
        i += 1
        continue
    fn_match = re.match(r'(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+(\w+)', line)
    if not fn_match:
        i += 1
        continue
    fn_name = fn_match.group(1)
    if fn_name.startswith('test_') or fn_name == 'tests':
        i += 1
        continue
    brace_depth = 0
    found_open = False
    body_lines_s = []
    j = i
    while j < len(lines):
        for ch in lines[j]:
            if ch == '{':
                brace_depth += 1
                found_open = True
            elif ch == '}':
                brace_depth -= 1
        body_lines_s.append(lines[j])
        if found_open and brace_depth == 0:
            break
        j += 1
    if body_lines_s:
        body_text = ' '.join(body_lines_s)
        struct_normalized = structural_normalize(body_text)
        struct_hash = hashlib.sha256(struct_normalized.encode()).hexdigest()[:16]
        structural_hashes[fn_name] = struct_hash
    i = j + 1

# --- Public API ---
# Public functions/methods exported from this file.
public_api = []
for m in re.finditer(r'^pub(?:\([^)]*\))?\s+(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+(\w+)', content, re.MULTILINE):
    name = m.group(1)
    if not name.startswith('test_') and name != 'tests':
        public_api.append(name)
# Deduplicate
seen = set()
public_api = [p for p in public_api if p not in seen and not seen.add(p)]

# --- Internal Calls ---
# Function/method calls within this file (for cross-file reference analysis).
# Matches: function_name( and self.method_name( and Type::method_name(
internal_calls = set()
# Free function calls: word followed by (
for m in re.finditer(r'\b(\w+)\s*\(', content):
    name = m.group(1)
    # Skip keywords, macros (already captured), and common non-function patterns
    skip_calls = {'if', 'while', 'for', 'match', 'loop', 'return', 'Some', 'None',
                  'Ok', 'Err', 'Box', 'Vec', 'Arc', 'Rc', 'String', 'println',
                  'eprintln', 'format', 'write', 'writeln', 'panic', 'assert',
                  'assert_eq', 'assert_ne', 'todo', 'unimplemented', 'unreachable',
                  'dbg', 'cfg', 'include', 'include_str', 'concat', 'env',
                  'compile_error', 'stringify', 'vec', 'hashmap', 'bail', 'ensure',
                  'anyhow', 'matches', 'debug_assert', 'debug_assert_eq',
                  'allow', 'deny', 'warn', 'derive', 'serde', 'test',
                  'inline', 'must_use', 'doc', 'feature'}
    if name not in skip_calls and not name.startswith('test_'):
        internal_calls.add(name)
# Method calls: .method_name( and ::method_name(
for m in re.finditer(r'[.:](\w+)\s*\(', content):
    name = m.group(1)
    if name not in skip_calls and not name.startswith('test_'):
        internal_calls.add(name)
internal_calls = sorted(internal_calls)

# --- Unused Parameters ---
# For each function, extract parameter names and check if they appear in the body.
unused_parameters = []
lines = content.split('\n')
i = 0
while i < len(lines):
    line = lines[i]
    # Only top-level functions (zero indentation)
    if line and line[0] in (' ', '\t'):
        i += 1
        continue
    fn_match = re.match(r'(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+(\w+)\s*(?:<[^>]*>)?\s*\(([^)]*(?:\([^)]*\)[^)]*)*)\)', line)
    if not fn_match:
        # Try multi-line signature (params continue on next lines)
        fn_match_start = re.match(r'(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+(\w+)\s*(?:<[^>]*>)?\s*\(', line)
        if fn_match_start:
            fn_name = fn_match_start.group(1)
            # Collect lines until we find the closing )
            sig_lines = [line]
            j = i + 1
            paren_depth = line.count('(') - line.count(')')
            while j < len(lines) and paren_depth > 0:
                sig_lines.append(lines[j])
                paren_depth += lines[j].count('(') - lines[j].count(')')
                j += 1
            full_sig = ' '.join(sig_lines)
            params_match = re.search(r'fn\s+\w+\s*(?:<[^>]*>)?\s*\(([^)]*(?:\([^)]*\)[^)]*)*)\)', full_sig)
            if params_match:
                params_str = params_match.group(1)
            else:
                i += 1
                continue
        else:
            i += 1
            continue
    else:
        fn_name = fn_match.group(1)
        params_str = fn_match.group(2)

    if fn_name.startswith('test_') or fn_name == 'tests':
        i += 1
        continue

    # Parse parameter names from the signature
    param_names = []
    for param in re.finditer(r'(\w+)\s*:', params_str):
        pname = param.group(1)
        # Skip 'self', 'mut' (as in &mut self), and type-only params
        if pname not in ('self', 'mut', 'Self'):
            param_names.append(pname)

    if not param_names:
        i += 1
        continue

    # Extract function body
    brace_depth = 0
    found_open = False
    body_lines = []
    j = i
    while j < len(lines):
        for ch in lines[j]:
            if ch == '{':
                brace_depth += 1
                found_open = True
            elif ch == '}':
                brace_depth -= 1
        if found_open:
            body_lines.append(lines[j])
        if found_open and brace_depth == 0:
            break
        j += 1

    if body_lines:
        # Join body, then strip the signature (everything before the first {)
        # so parameter names in the signature don't cause false negatives.
        full_body = '\n'.join(body_lines)
        brace_pos = full_body.find('{')
        if brace_pos >= 0:
            body_only = full_body[brace_pos + 1:]
        else:
            body_only = full_body
        for pname in param_names:
            # Skip params prefixed with _ (intentionally unused)
            if pname.startswith('_'):
                continue
            # Check if the parameter name appears anywhere in the body
            # Use word boundary to avoid false positives (e.g., 'id' in 'width')
            if not re.search(r'\b' + re.escape(pname) + r'\b', body_only):
                unused_parameters.append({'function': fn_name, 'param': pname})

    i = j + 1

# --- Dead Code Markers ---
# Find #[allow(dead_code)] annotations and the item they apply to.
dead_code_markers = []
lines = content.split('\n')
for line_num, line in enumerate(lines, 1):
    stripped = line.strip()
    if stripped == '#[allow(dead_code)]':
        # The next non-attribute, non-empty line should be the item
        for k in range(line_num, min(line_num + 5, len(lines))):
            next_line = lines[k].strip()
            if next_line and not next_line.startswith('#[') and not next_line.startswith('//'):
                # Extract item name
                item_match = re.match(r'(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?(?:static\s+)?(?:fn|struct|enum|type|trait|const|static|mod)\s+(\w+)', next_line)
                if item_match:
                    dead_code_markers.append({
                        'item': item_match.group(1),
                        'line': line_num,
                        'marker_type': 'allow_dead_code',
                    })
                break

result = {
    'methods': methods,
    'type_name': type_name,
    'implements': implements,
    'registrations': registrations,
    'namespace': namespace,
    'imports': imports,
    'method_hashes': method_hashes,
    'structural_hashes': structural_hashes,
    'unused_parameters': unused_parameters,
    'dead_code_markers': dead_code_markers,
    'internal_calls': internal_calls,
    'public_api': public_api,
}

print(json.dumps(result))
")

echo "$CONTENT"
