#!/usr/bin/env bash
# Rust fingerprint script for homeboy audit.
#
# Input (JSON on stdin):
#   {"file_path": "src/commands/deploy.rs", "content": "..."}
#
# Output (JSON on stdout):
#   {"methods": [...], "type_name": "...", "implements": [...],
#    "registrations": [], "namespace": "...", "imports": [...],
#    "method_hashes": {...}, "structural_hashes": {...},
#    "unused_parameters": [...], "dead_code_markers": [...],
#    "internal_calls": [...], "public_api": [...],
#    "aggregate_definitions": [...], "field_accesses": [...],
#    "aggregate_projections": [...], "decision_branches": [...],
#    "method_calls": [...]}
#
# Extracts structural information from Rust source files using text matching.
# Handles methods inside impl blocks and test methods inside #[cfg(test)] modules.

set -euo pipefail

# Read stdin into variable
INPUT=$(cat)

# Extract content from JSON — use python3 for reliable JSON parsing
CONTENT=$(INPUT_JSON="$INPUT" python3 <<'PY'
import json, sys, re, hashlib, os

data = json.loads(os.environ['INPUT_JSON'])
content = data['content']
file_path = data['file_path']

lines = content.split('\n')

# ============================================================================
# Context tracking: parse the file line-by-line to understand nesting.
# For each line we track:
#   - brace_depth: overall brace nesting level
#   - in_test_module: whether we're inside a #[cfg(test)] mod tests { }
#   - impl_context: the type name if we're inside an impl block
#   - pending_attrs: attributes accumulated before the next item
# ============================================================================

class Context:
    def __init__(self):
        self.brace_depth = 0
        # Stack of (kind, depth) where kind is 'impl', 'test_mod', 'other'
        # and depth is the brace_depth at which the block was entered
        self.block_stack = []
        self.pending_attrs = []
        self.impl_type = None      # Current impl target type (if in impl block)
        self.in_test_mod = False    # Inside #[cfg(test)] module

    def is_in_impl(self):
        for kind, _, _ in reversed(self.block_stack):
            if kind == 'impl':
                return True
        return False

    def current_impl_type(self):
        for kind, _, meta in reversed(self.block_stack):
            if kind == 'impl':
                return meta  # The impl type name
        return None

    def is_in_test_module(self):
        for kind, _, _ in self.block_stack:
            if kind == 'test_mod':
                return True
        return False

ctx = Context()

# ============================================================================
# First pass: identify all function locations and their context.
# We need this to correctly attribute methods to their impl blocks and
# distinguish test functions from production code.
# ============================================================================

class FnInfo:
    def __init__(self, name, line_num, impl_type, is_test, is_public, signature_lines, body_start_line, is_test_helper=False):
        self.name = name
        self.line_num = line_num          # 1-indexed line number
        self.impl_type = impl_type        # None for free fns, 'Type' for impl methods
        self.is_test = is_test            # Has #[test] attribute
        self.is_test_helper = is_test_helper  # Inside #[cfg(test)] without #[test]
        self.is_public = is_public        # pub fn
        self.signature_lines = signature_lines  # Lines comprising the fn signature
        self.body_start_line = body_start_line  # Index where body starts (0-indexed)
        self.body_lines = []              # Filled in during body extraction

functions = []
pending_attrs = []

# Regex patterns
fn_pattern = re.compile(r'^(\s*)(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+(\w+)')
impl_pattern = re.compile(r'^\s*impl(?:<[^>]*>)?\s+(?:(\w+(?:::\w+)*)\s+for\s+)?(\w+)')
mod_pattern = re.compile(r'^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)')
cfg_test_pattern = re.compile(r'#\[cfg\(test\)\]')
test_attr_pattern = re.compile(r'#\[test\]')

i = 0
while i < len(lines):
    line = lines[i]
    stripped = line.strip()

    # Track attributes (they apply to the next item)
    if stripped.startswith('#['):
        pending_attrs.append(stripped)
        # Don't count braces inside attributes
        i += 1
        continue

    # Track blank lines and comments — reset nothing, just skip
    if not stripped or stripped.startswith('//'):
        i += 1
        continue

    # Check for impl block start
    impl_match = impl_pattern.match(line)
    if impl_match and '{' in line:
        trait_or_type = impl_match.group(2)  # The concrete type
        old_depth = ctx.brace_depth
        for ch in line:
            if ch == '{':
                ctx.brace_depth += 1
            elif ch == '}':
                ctx.brace_depth -= 1
        ctx.block_stack.append(('impl', old_depth, trait_or_type))
        pending_attrs = []
        i += 1
        continue

    # Check for mod block start (especially #[cfg(test)] mod tests)
    mod_match = mod_pattern.match(line)
    if mod_match and '{' in line:
        mod_name = mod_match.group(1)
        is_test_mod = any(cfg_test_pattern.search(a) for a in pending_attrs) or mod_name == 'tests'
        old_depth = ctx.brace_depth
        for ch in line:
            if ch == '{':
                ctx.brace_depth += 1
            elif ch == '}':
                ctx.brace_depth -= 1
        kind = 'test_mod' if is_test_mod else 'other'
        ctx.block_stack.append((kind, old_depth, mod_name))
        pending_attrs = []
        i += 1
        continue

    # Check for fn declaration
    fn_match = fn_pattern.match(line)
    if fn_match:
        _indent = fn_match.group(1)
        fn_name = fn_match.group(2)

        has_test_attr = any(test_attr_pattern.search(a) for a in pending_attrs)
        # A function is a test only if it has #[test] attribute.
        # Being inside #[cfg(test)] mod tests {} WITHOUT #[test] makes
        # it a test helper (like make_fingerprint()) — these shouldn't
        # be prefixed with test_ or reported as orphaned tests.
        is_test = has_test_attr
        is_test_helper = not has_test_attr and ctx.is_in_test_module()
        is_public = bool(re.match(r'\s*pub(?:\([^)]*\))?\s+', line))
        impl_type = ctx.current_impl_type()

        # Collect the full signature (may span multiple lines if params are multi-line)
        sig_lines = [line]
        # Check if the signature is complete (has both opening paren and closing paren)
        paren_depth = line.count('(') - line.count(')')
        j = i + 1
        while paren_depth > 0 and j < len(lines):
            sig_lines.append(lines[j])
            paren_depth += lines[j].count('(') - lines[j].count(')')
            j += 1

        # Find where the body starts (the opening brace)
        # It might be on the same line as params close, or on a subsequent line
        body_start = j - 1  # Last line of signature
        combined_sig = '\n'.join(sig_lines)
        if '{' not in combined_sig:
            # Opening brace is on a later line (e.g., after -> ReturnType)
            while body_start + 1 < len(lines) and '{' not in lines[body_start]:
                body_start += 1
                sig_lines.append(lines[body_start])

        fn_info = FnInfo(
            name=fn_name,
            line_num=i + 1,
            impl_type=impl_type,
            is_test=is_test,
            is_public=is_public,
            signature_lines=sig_lines,
            body_start_line=i,
            is_test_helper=is_test_helper,
        )

        # Extract the full body (from fn line to matching closing brace)
        brace_depth = 0
        found_open = False
        body_lines_list = []
        k = i
        while k < len(lines):
            for ch in lines[k]:
                if ch == '{':
                    brace_depth += 1
                    found_open = True
                elif ch == '}':
                    brace_depth -= 1
            body_lines_list.append(lines[k])
            if found_open and brace_depth == 0:
                break
            k += 1

        fn_info.body_lines = body_lines_list

        functions.append(fn_info)

        # Advance context past the function body.
        # A balanced fn body (open brace ... close brace) nets zero depth change,
        # so the context brace_depth stays the same. We don't pop blocks here
        # because the fn body's internal braces are self-contained.
        # The net brace change for a well-formed function body is always zero.
        pending_attrs = []
        i = k + 1
        continue

    # Default: count braces and pop blocks as needed
    for ch in line:
        if ch == '{':
            ctx.brace_depth += 1
        elif ch == '}':
            ctx.brace_depth -= 1

    while ctx.block_stack and ctx.brace_depth <= ctx.block_stack[-1][1]:
        ctx.block_stack.pop()

    pending_attrs = []
    i += 1

# ============================================================================
# Build output from collected function info
# ============================================================================

# --- Methods ---
# All non-test functions. Exclude test helpers (inside #[cfg(test)]
# without #[test]) — they're not source methods and shouldn't trigger
# "missing test method" findings.
methods = []
seen = set()
for fn in functions:
    if fn.is_test:
        continue
    if fn.is_test_helper:
        continue
    if fn.name == 'tests':
        continue
    if fn.name not in seen:
        methods.append(fn.name)
        seen.add(fn.name)

# --- Test Methods ---
# Functions with #[test] attribute only.
# Included in the methods list with the test_ prefix so test_coverage
# can identify them. Functions that already start with test_ keep their
# name; others get prefixed (e.g. 'dedup_works' -> 'test_dedup_works').
# Note: test helpers (functions inside #[cfg(test)] WITHOUT #[test])
# are excluded — they're not tests and shouldn't appear in either list.
test_methods = []
for fn in functions:
    if fn.is_test:
        # Normalize: ensure test methods always carry the test_ prefix
        # so test_coverage.rs can distinguish them from source methods.
        prefixed = fn.name if fn.name.startswith('test_') else f'test_{fn.name}'
        if prefixed not in seen:
            methods.append(prefixed)
            seen.add(prefixed)
            test_methods.append(prefixed)

# --- Type name ---
# Primary struct or enum in the file (first pub struct/enum, or first struct/enum)
type_name = None
pub_types = re.findall(r'pub\s+(?:struct|enum)\s+(\w+)', content)
all_types = re.findall(r'(?:pub\s+)?(?:struct|enum)\s+(\w+)', content)
if pub_types:
    type_name = pub_types[0]
elif all_types:
    type_name = all_types[0]

# --- Extends ---
extends = None

# --- Implements ---
# Match impl blocks: impl Trait for Type, impl<T> Trait for Type
implements = []
for m in re.finditer(r'impl(?:<[^>]*>)?\s+(\w+(?:::\w+)*)\s+for\s+\w+', content):
    trait_name = m.group(1)
    implements.append(trait_name.split('::')[-1])
seen_impl = set()
implements = [x for x in implements if x not in seen_impl and not seen_impl.add(x)]

# --- Registrations ---
registrations = []
for m in re.finditer(r'(\w+)!\s*\(', content):
    macro_name = m.group(1)
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
seen_reg = set()
registrations = [r for r in registrations if r not in seen_reg and not seen_reg.add(r)]

# --- Namespace ---
crate_uses = re.findall(r'use\s+crate::(\w+)', content)
if crate_uses:
    from collections import Counter
    counts = Counter(crate_uses)
    most_common = counts.most_common(1)[0][0]
    namespace = f'crate::{most_common}'
else:
    parts = file_path.replace('.rs', '').split('/')
    if len(parts) > 1:
        namespace = 'crate::' + '::'.join(parts[1:-1]) if len(parts) > 2 else 'crate::' + parts[-1]
    else:
        namespace = None

# --- Imports ---
imports = []
for m in re.finditer(r'use\s+((?:crate|super|self|std|serde|clap|regex|chrono|tokio|anyhow)\S+);', content):
    imports.append(m.group(1))
for m in re.finditer(r'use\s+((?:crate|super|self)\S+::\{[^}]+\});', content):
    imports.append(m.group(1))
seen_imp = set()
imports = [x for x in imports if x not in seen_imp and not seen_imp.add(x)]

# --- Visibility (method -> visibility level) ---
visibility = {}
for fn in functions:
    if fn.is_test:
        continue
    sig = ' '.join(fn.signature_lines)
    if re.match(r'\s*pub\s*\(crate\)', sig):
        visibility[fn.name] = 'pub(crate)'
    elif re.match(r'\s*pub\s*\(super\)', sig):
        visibility[fn.name] = 'pub(super)'
    elif re.match(r'\s*pub\s', sig):
        visibility[fn.name] = 'public'
    else:
        visibility[fn.name] = 'private'

# ============================================================================
# Method Hashes & Structural Hashes (for duplication detection)
# Now works for ALL functions, not just top-level ones.
# ============================================================================

def structural_normalize(text):
    brace_idx = text.find('{')
    if brace_idx >= 0:
        text = text[brace_idx:]

    dq = chr(34)
    text = re.sub(dq + '[^' + dq + ']*' + dq, 'STR', text)
    text = re.sub(chr(39) + '[^' + chr(39) + ']*' + chr(39), 'CHR', text)
    text = re.sub(r'\b\d[\d_]*(?:\.\d[\d_]*)?\b', 'NUM', text)

    rust_keywords = {
        'as', 'async', 'await', 'break', 'const', 'continue', 'crate',
        'dyn', 'else', 'enum', 'extern', 'false', 'fn', 'for', 'if',
        'impl', 'in', 'let', 'loop', 'match', 'mod', 'move', 'mut',
        'pub', 'ref', 'return', 'self', 'Self', 'static', 'struct',
        'super', 'trait', 'true', 'type', 'unsafe', 'use', 'where',
        'while', 'yield',
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
    text = re.sub(r'\s+', ' ', text).strip()
    return text

method_hashes = {}
structural_hashes = {}

for fn in functions:
    if fn.is_test or fn.name == 'tests':
        continue
    if fn.body_lines:
        body_text = ' '.join(fn.body_lines)
        # Exact hash
        normalized = re.sub(r'\s+', ' ', body_text).strip()
        method_hashes[fn.name] = hashlib.sha256(normalized.encode()).hexdigest()[:16]
        # Structural hash
        struct_normalized = structural_normalize(body_text)
        structural_hashes[fn.name] = hashlib.sha256(struct_normalized.encode()).hexdigest()[:16]

# --- Public API ---
public_api = []
for fn in functions:
    if fn.is_test:
        continue
    if fn.is_public:
        public_api.append(fn.name)
seen_pub = set()
public_api = [p for p in public_api if p not in seen_pub and not seen_pub.add(p)]

# --- Internal Calls ---
# Function/method calls within this file (for cross-file reference analysis).
internal_calls = set()
skip_calls = {'if', 'while', 'for', 'match', 'loop', 'return', 'Some', 'None',
              'Ok', 'Err', 'Box', 'Vec', 'Arc', 'Rc', 'String', 'println',
              'eprintln', 'format', 'write', 'writeln', 'panic', 'assert',
              'assert_eq', 'assert_ne', 'todo', 'unimplemented', 'unreachable',
              'dbg', 'cfg', 'include', 'include_str', 'concat', 'env',
              'compile_error', 'stringify', 'vec', 'hashmap', 'bail', 'ensure',
              'anyhow', 'matches', 'debug_assert', 'debug_assert_eq',
              'allow', 'deny', 'warn', 'derive', 'serde', 'test',
              'inline', 'must_use', 'doc', 'feature', 'pub', 'crate', 'super'}
for m in re.finditer(r'\b(\w+)\s*\(', content):
    name = m.group(1)
    if name not in skip_calls and not name.startswith('test_'):
        internal_calls.add(name)
for m in re.finditer(r'[.:](\w+)\s*\(', content):
    name = m.group(1)
    if name not in skip_calls and not name.startswith('test_'):
        internal_calls.add(name)
internal_calls = sorted(internal_calls)

# --- Unused Parameters ---
# Now works for all functions (not just top-level).
unused_parameters = []
for fn in functions:
    if fn.is_test or fn.name == 'tests':
        continue
    # Parse params from the full signature
    full_sig = ' '.join(fn.signature_lines)
    params_match = re.search(r'fn\s+\w+\s*(?:<[^>]*>)?\s*\(([^)]*(?:\([^)]*\)[^)]*)*)\)', full_sig)
    if not params_match:
        continue
    params_str = params_match.group(1)
    param_names = []
    # Split on commas to get individual params, then extract the name
    # before the first colon. This avoids matching type path segments
    # like crate::commands::GlobalArgs as parameter names.
    for param_chunk in params_str.split(','):
        param_chunk = param_chunk.strip()
        if not param_chunk:
            continue
        # Match: optional mut, then the param name, then colon
        pmatch = re.match(r'(?:mut\s+)?(\w+)\s*:', param_chunk)
        if pmatch:
            pname = pmatch.group(1)
            if pname not in ('self', 'mut', 'Self'):
                param_names.append(pname)
    if not param_names:
        continue
    # Check if params appear in the body (excluding the signature).
    # Skip trait declarations (no body — the fn declaration line ends with ;).
    if fn.body_lines:
        # Detect trait method declarations: check the lines from the fn
        # declaration through where params close. If any of these lines
        # ends with ';', it's a bodyless declaration.
        fn_decl_line = fn.body_lines[0] if fn.body_lines else ''
        # For single-line trait decls like: fn foo(&self, x: T) -> bool;
        # The semicolon may be on the fn line or on a continuation line
        # (for multi-line signatures). Check the first few lines.
        is_bodyless = False
        for check_line in fn.body_lines[:3]:
            stripped = check_line.strip()
            if stripped.endswith(';') and '{' not in stripped:
                is_bodyless = True
                break
        if is_bodyless:
            continue
        full_body = '\n'.join(fn.body_lines)
        brace_pos = full_body.find('{')
        if brace_pos < 0:
            # No body brace found — can't determine usage
            continue
        body_only = full_body[brace_pos + 1:]
        for pname in param_names:
            if pname.startswith('_'):
                continue
            if not re.search(r'\b' + re.escape(pname) + r'\b', body_only):
                unused_parameters.append({'function': fn.name, 'param': pname})

# --- Dead Code Markers ---
dead_code_markers = []
for line_num, line in enumerate(lines, 1):
    stripped = line.strip()
    if stripped == '#[allow(dead_code)]':
        for k in range(line_num, min(line_num + 5, len(lines))):
            next_line = lines[k].strip()
            if next_line and not next_line.startswith('#[') and not next_line.startswith('//'):
                item_match = re.match(r'(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?(?:static\s+)?(?:fn|struct|enum|type|trait|const|static|mod)\s+(\w+)', next_line)
                if item_match:
                    dead_code_markers.append({
                        'item': item_match.group(1),
                        'line': line_num,
                        'marker_type': 'allow_dead_code',
                    })
                break

# ============================================================================
# Policy-flow facts
# ============================================================================

def module_id_for_path(path):
    path = path.replace('\\', '/').removeprefix('./')
    parts = path.split('/')
    if parts and parts[0] == 'src':
        parts = parts[1:]
    if not parts:
        return 'crate'
    leaf = parts[-1]
    if leaf in {'lib.rs', 'main.rs', 'mod.rs'}:
        parts = parts[:-1]
    elif leaf.endswith('.rs'):
        parts[-1] = leaf[:-3]
    return 'crate' + (f"::{'::'.join(parts)}" if parts else '')


module_id = module_id_for_path(file_path)
line_offsets = []
offset = 0
for source_line in lines:
    line_offsets.append(offset)
    offset += len(source_line) + 1


def location_at(offset):
    line = content.count('\n', 0, offset) + 1
    line_start = content.rfind('\n', 0, offset) + 1
    return {'line': line, 'column': offset - line_start + 1}


def mask_rust(text):
    """Mask comments and literals while preserving offsets and newlines."""
    chars = list(text)
    i = 0
    block_depth = 0
    while i < len(chars):
        if block_depth:
            if text.startswith('/*', i):
                chars[i:i + 2] = '  '
                block_depth += 1
                i += 2
            elif text.startswith('*/', i):
                chars[i:i + 2] = '  '
                block_depth -= 1
                i += 2
            else:
                if chars[i] != '\n':
                    chars[i] = ' '
                i += 1
            continue
        if text.startswith('//', i):
            end = text.find('\n', i)
            end = len(text) if end < 0 else end
            chars[i:end] = ' ' * (end - i)
            i = end
            continue
        if text.startswith('/*', i):
            chars[i:i + 2] = '  '
            block_depth = 1
            i += 2
            continue
        if chars[i] in {'"', "'"}:
            # A single quote before an identifier is a lifetime, not a char.
            if chars[i] == "'" and i + 1 < len(chars) and re.match(r'[A-Za-z_]', chars[i + 1]):
                lifetime = re.match(r"'[A-Za-z_]\w*", text[i:])
                if lifetime:
                    i += len(lifetime.group(0))
                    continue
            quote = chars[i]
            chars[i] = ' '
            i += 1
            while i < len(chars):
                if chars[i] == '\\':
                    chars[i] = ' '
                    if i + 1 < len(chars) and chars[i + 1] != '\n':
                        chars[i + 1] = ' '
                    i += 2
                    continue
                end_quote = chars[i] == quote
                if chars[i] != '\n':
                    chars[i] = ' '
                i += 1
                if end_quote:
                    break
            continue
        i += 1
    return ''.join(chars)


masked_content = mask_rust(content)


def matching_delimiter(text, opening, left='{', right='}'):
    depth = 0
    for pos in range(opening, len(text)):
        if text[pos] == left:
            depth += 1
        elif text[pos] == right:
            depth -= 1
            if depth == 0:
                return pos
    return None


inline_test_module_ranges = []
inline_test_module_pattern = re.compile(
    r'#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]'
    r'(?:\s*#\s*\[[^]]*\])*'
    r'\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{'
)
for test_module_match in inline_test_module_pattern.finditer(masked_content):
    opening = masked_content.rfind('{', test_module_match.start(), test_module_match.end())
    closing = matching_delimiter(masked_content, opening)
    if closing is not None:
        inline_test_module_ranges.append((test_module_match.start(), closing + 1))

# Keep source offsets stable while excluding test-only declarations from the
# production aggregate type table and definition scan.
aggregate_source_chars = list(masked_content)
for start, end in inline_test_module_ranges:
    for pos in range(start, end):
        if aggregate_source_chars[pos] != '\n':
            aggregate_source_chars[pos] = ' '
aggregate_source = ''.join(aggregate_source_chars)


def split_top_level_spans(text, delimiter=','):
    spans = []
    start = 0
    depths = {'(': 0, '[': 0, '{': 0, '<': 0}
    closing = {')': '(', ']': '[', '}': '{', '>': '<'}
    for pos, ch in enumerate(text):
        if ch in depths:
            depths[ch] += 1
        elif ch in closing and depths[closing[ch]]:
            depths[closing[ch]] -= 1
        elif ch == delimiter and not any(depths.values()):
            spans.append((start, pos))
            start = pos + 1
    spans.append((start, len(text)))
    return spans


type_declarations = {}
struct_fields = {}
aggregate_definitions = []

for type_match in re.finditer(r'\b(?:pub(?:\([^)]*\))?\s+)?(struct|enum)\s+([A-Z][A-Za-z0-9_]*)\b', aggregate_source):
    type_declarations[type_match.group(2)] = f'{module_id}::{type_match.group(2)}'

primitive_types = {
    'bool', 'char', 'str', 'String', 'usize', 'isize', 'u8', 'u16', 'u32',
    'u64', 'u128', 'i8', 'i16', 'i32', 'i64', 'i128', 'f32', 'f64',
}


def strip_generic_arguments(type_text):
    match = re.fullmatch(r'((?:crate|self|super|[A-Za-z_]\w*)(?:::(?:super|self|[A-Za-z_]\w*))*)(?:::)?\s*<.*>', type_text, re.S)
    return match.group(1) if match else type_text


def canonical_type_path(path):
    path = re.sub(r'\s*::\s*', '::', path.strip())
    segments = path.split('::')
    if not segments or any(not re.fullmatch(r'[A-Za-z_]\w*', segment) for segment in segments):
        return None
    if segments[0] == 'crate':
        resolved = ['crate']
        segments = segments[1:]
    elif segments[0] == 'self':
        resolved = module_id.split('::')
        segments = segments[1:]
    elif segments[0] == 'super':
        resolved = module_id.split('::')
    else:
        return None
    for segment in segments:
        if segment == 'self':
            continue
        if segment == 'super':
            if len(resolved) == 1:
                return None
            resolved.pop()
        else:
            resolved.append(segment)
    return '::'.join(resolved)


imports_by_name = {}
for import_match in re.finditer(r'\buse\s+((?:crate|self|super)(?:::\w+)+)\s*;', masked_content):
    imported = canonical_type_path(import_match.group(1))
    if imported:
        imports_by_name[imported.rsplit('::', 1)[-1]] = imported


def resolve_type(type_text, impl_type=None):
    value = re.sub(r'\b(?:mut|const|dyn)\b', '', type_text).strip()
    while value.startswith('&'):
        value = re.sub(r"^&\s*(?:'[A-Za-z_]\w*\s+)?(?:mut\s+)?", '', value).strip()
    value = strip_generic_arguments(value)
    if value == 'Self' and impl_type:
        return type_declarations.get(impl_type)
    if value in primitive_types:
        return value
    if value.startswith(('crate::', 'self::', 'super::')):
        return canonical_type_path(value)
    if value in type_declarations:
        return type_declarations[value]
    if value in imports_by_name:
        return imports_by_name[value]
    return None


# Generic constructors resolved by name rather than by declaration or import.
# They are in the prelude or are ubiquitous std containers, so a field typed
# `Option<T>` never carries an import that `imports_by_name` could resolve. The
# constructor name alone is not an identity -- `Option<String>` and
# `Option<u32>` are different field types -- so it is only ever emitted as the
# head of a fully resolved application by `resolve_field_type`.
std_generic_constructors = {
    'Option', 'Result', 'Vec', 'VecDeque', 'Box', 'Arc', 'Rc', 'Cow',
    'HashMap', 'BTreeMap', 'HashSet', 'BTreeSet', 'Mutex', 'RwLock',
    'RefCell', 'Cell', 'OnceLock', 'OnceCell',
}


def resolve_field_type(type_text, impl_type=None):
    """Resolve a declared field's type to a structural identity.

    `resolve_type` deliberately drops generic arguments: its callers ask "which
    declared aggregate is this value?", and for that `Vec<Foo>` and `Foo` are the
    same answer. A field type is a different question. `Option<String>` and
    `Option<u32>` are not the same field, so the arguments are part of the
    identity and an application is resolved head-and-arguments rather than
    stripped to its head.

    Resolution is total or nothing. If any argument is unresolvable the whole
    application is, because a partially resolved type compares equal to another
    type that differs precisely where neither could be resolved -- which is a
    false claim of sameness, and the thing consumers of `type_id` must never be
    handed.
    """
    value = re.sub(r'\b(?:mut|const|dyn)\b', '', type_text).strip()
    while value.startswith('&'):
        value = re.sub(r"^&\s*(?:'[A-Za-z_]\w*\s+)?(?:mut\s+)?", '', value).strip()

    application = re.fullmatch(
        r'((?:crate|self|super|[A-Za-z_]\w*)(?:::(?:super|self|[A-Za-z_]\w*))*)(?:::)?\s*<(.*)>',
        value,
        re.S,
    )
    if not application:
        return resolve_type(value, impl_type)

    head, argument_text = application.group(1), application.group(2)
    resolved_head = resolve_type(head, impl_type)
    if resolved_head is None:
        if head not in std_generic_constructors:
            return None
        resolved_head = head

    arguments = []
    for start, end in split_top_level_spans(argument_text):
        argument = argument_text[start:end].strip()
        if not argument:
            continue
        if re.fullmatch(r"'[A-Za-z_]\w*", argument):
            continue
        resolved_argument = resolve_field_type(argument, impl_type)
        if resolved_argument is None:
            return None
        arguments.append(resolved_argument)

    if not arguments:
        return None
    return '{}<{}>'.format(resolved_head, ', '.join(arguments))


named_struct_pattern = re.compile(
    r'\b(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Z][A-Za-z0-9_]*)'
    r'(?:\s*<[^>{;]*>)?(?:\s+where\b[^{};]*)?\s*\{'
)
for struct_match in named_struct_pattern.finditer(aggregate_source):
    name = struct_match.group(1)
    opening = aggregate_source.find('{', struct_match.start(), struct_match.end())
    closing = matching_delimiter(aggregate_source, opening)
    if closing is None:
        continue
    body = aggregate_source[opening + 1:closing]
    fields = []
    field_names = set()
    for start, end in split_top_level_spans(body):
        chunk = re.sub(r'#\s*\[[^]]*\]', ' ', body[start:end]).strip()
        field_match = re.match(r'(?:pub(?:\([^)]*\))?\s+)?([a-z_][A-Za-z0-9_]*)\s*:\s*(.+)', chunk, re.S)
        if not field_match:
            continue
        field = {'name': field_match.group(1)}
        field_type = resolve_field_type(field_match.group(2).strip())
        if field_type:
            field['type_id'] = field_type
        fields.append(field)
        field_names.add(field['name'])
    fields.sort(key=lambda item: (item['name'], item.get('type_id', '')))
    struct_fields[type_declarations[name]] = field_names
    aggregate_definitions.append({
        'type_id': type_declarations[name],
        'fields': fields,
        'location': location_at(struct_match.start(1)),
    })


def function_id(fn):
    if fn.impl_type and fn.impl_type in type_declarations:
        return f'{type_declarations[fn.impl_type]}::{fn.name}'
    return f'{module_id}::{fn.name}'


def signature_details(fn):
    signature = '\n'.join(fn.signature_lines)
    params_start = signature.find('(')
    params_end = matching_delimiter(signature, params_start, '(', ')') if params_start >= 0 else None
    params = signature[params_start + 1:params_end] if params_end is not None else ''
    suffix = signature[params_end + 1:] if params_end is not None else ''
    return_match = re.search(r'->\s*([^\{;]+)', suffix, re.S)
    return params, return_match.group(1).strip() if return_match else None


function_returns = {}
for fn in functions:
    _, return_text = signature_details(fn)
    resolved_return = resolve_type(return_text, fn.impl_type) if return_text else None
    function_returns[function_id(fn)] = resolved_return


def expression_type(expression, variable_types):
    expression = expression.strip().lstrip('&').strip()
    variable_match = re.fullmatch(r'(?:\*\s*)?([a-z_][A-Za-z0-9_]*)', expression)
    if variable_match:
        return variable_types.get(variable_match.group(1))
    call_match = re.fullmatch(r'([a-z_][A-Za-z0-9_]*|self)\s*\.\s*([a-z_][A-Za-z0-9_]*)\s*\([^)]*\)\s*\??', expression, re.S)
    if call_match:
        receiver_type = variable_types.get(call_match.group(1))
        if receiver_type:
            return function_returns.get(f"{receiver_type}::{call_match.group(2)}")
    literal_match = re.match(r'([A-Z][A-Za-z0-9_]*)(?:::)?(?:\s*<[^>{;]*>)?\s*\{', expression)
    if literal_match:
        return resolve_type(literal_match.group(1))
    return None


def discriminant_id(pattern, domain_type):
    normalized = re.sub(r'\s+', '', pattern)
    normalized = re.sub(r'\([^)]*\)|\{[^}]*\}', '', normalized)
    normalized = normalized.lstrip('&')
    if '::' in normalized:
        variant = normalized.rsplit('::', 1)[-1]
        return f'{domain_type}::{variant}'
    if re.fullmatch(r'[A-Z][A-Za-z0-9_]*', normalized):
        return f'{domain_type}::{normalized}'
    return normalized or '_'


field_accesses = []
aggregate_projections = []
decision_branches = []
method_calls = []

for fn in functions:
    if fn.is_test or fn.is_test_helper or not fn.body_lines:
        continue
    callable_id = function_id(fn)
    body_text = '\n'.join(fn.body_lines)
    body_masked = mask_rust(body_text)
    body_offset = line_offsets[fn.line_num - 1]
    params_text, return_text = signature_details(fn)
    return_type = resolve_type(return_text, fn.impl_type) if return_text else None
    variable_types = {}
    field_origins = {}
    if fn.impl_type in type_declarations and re.search(r'(?:^|,)\s*&?\s*(?:mut\s+)?self\b', params_text):
        variable_types['self'] = type_declarations[fn.impl_type]
    for start, end in split_top_level_spans(params_text):
        param_match = re.match(r'\s*(?:mut\s+)?([a-z_][A-Za-z0-9_]*)\s*:\s*(.+)', params_text[start:end], re.S)
        if param_match:
            param_type = resolve_type(param_match.group(2).strip(), fn.impl_type)
            if param_type:
                variable_types[param_match.group(1)] = param_type

    # Resolve explicit locals first, then simple constructor/method assignments.
    for local_match in re.finditer(r'\blet\s+(?:mut\s+)?([a-z_][A-Za-z0-9_]*)\s*:\s*([^=;]+)', body_masked):
        local_type = resolve_type(local_match.group(2).strip(), fn.impl_type)
        if local_type:
            variable_types[local_match.group(1)] = local_type
    for _ in range(2):
        for local_match in re.finditer(r'\blet\s+(?:mut\s+)?([a-z_][A-Za-z0-9_]*)(?:\s*:\s*[^=;]+)?\s*=\s*([^;]+)', body_masked, re.S):
            name, expression = local_match.group(1), local_match.group(2).strip()
            inferred = expression_type(expression, variable_types)
            if inferred:
                variable_types[name] = inferred
            origin_match = re.fullmatch(r'([a-z_][A-Za-z0-9_]*|self)\s*\.\s*([a-z_][A-Za-z0-9_]*)', expression)
            if origin_match and variable_types.get(origin_match.group(1)):
                field_origins[name] = (origin_match.group(1), origin_match.group(2))

    for access_match in re.finditer(r'\b([a-z_][A-Za-z0-9_]*|self)\s*\.\s*([a-z_][A-Za-z0-9_]*)\b(?!\s*\()', body_masked):
        receiver, field = access_match.group(1), access_match.group(2)
        owner_type = variable_types.get(receiver)
        if not owner_type or owner_type in primitive_types:
            continue
        if owner_type in struct_fields and field not in struct_fields[owner_type]:
            continue
        following = body_masked[access_match.end():access_match.end() + 12]
        write = bool(re.match(r'\s*(?:[+\-*/%&|^]?=)(?!=|>)', following))
        field_accesses.append({
            'owner_type_id': owner_type,
            'field': field,
            'callable_id': callable_id,
            'access': 'write' if write else 'read',
            'location': location_at(body_offset + access_match.start(2)),
        })

    for literal_match in re.finditer(r'\b([A-Z][A-Za-z0-9_]*)(?:::)?(?:\s*<[^>{;]*>)?\s*\{', body_masked):
        target_type = resolve_type(literal_match.group(1), fn.impl_type)
        if not target_type or target_type in primitive_types:
            continue
        opening = body_masked.find('{', literal_match.start(), literal_match.end())
        closing = matching_delimiter(body_masked, opening)
        if closing is None:
            continue
        literal_body = body_masked[opening + 1:closing]
        mappings_by_source = {}
        literal_items = [literal_body[start:end].strip() for start, end in split_top_level_spans(literal_body)]
        initialized_fields = {
            explicit.group(1)
            for item in literal_items
            if not item.startswith('..')
            for explicit in [re.match(r'([a-z_][A-Za-z0-9_]*)(?:\s*:|\s*$)', item)]
            if explicit
        }
        for item in literal_items:
            update_match = re.fullmatch(r'\.\.\s*([a-z_][A-Za-z0-9_]*)', item)
            if update_match:
                source = update_match.group(1)
                source_type = variable_types.get(source)
                if source_type and source_type in struct_fields:
                    common = (struct_fields[source_type] & struct_fields[target_type]) - initialized_fields
                    mappings_by_source.setdefault(source_type, set()).update((field, field) for field in common)
                continue
            explicit = re.match(r'([a-z_][A-Za-z0-9_]*)\s*:\s*([a-z_][A-Za-z0-9_]*|self)\s*\.\s*([a-z_][A-Za-z0-9_]*)\s*$', item)
            if explicit:
                target_field, source, source_field = explicit.groups()
                source_type = variable_types.get(source)
                target_valid = target_type not in struct_fields or target_field in struct_fields[target_type]
                source_valid = source_type not in struct_fields or source_field in struct_fields[source_type]
                if source_type and source_type not in primitive_types and target_valid and source_valid:
                    mappings_by_source.setdefault(source_type, set()).add((source_field, target_field))
                continue
            shorthand = re.fullmatch(r'([a-z_][A-Za-z0-9_]*)', item)
            if shorthand and shorthand.group(1) in field_origins:
                source, source_field = field_origins[shorthand.group(1)]
                source_type = variable_types.get(source)
                target_field = shorthand.group(1)
                target_valid = target_type not in struct_fields or target_field in struct_fields[target_type]
                source_valid = source_type not in struct_fields or source_field in struct_fields[source_type]
                if source_type and source_type not in primitive_types and target_valid and source_valid:
                    mappings_by_source.setdefault(source_type, set()).add((source_field, target_field))
        for source_type, mappings in mappings_by_source.items():
            aggregate_projections.append({
                'source_type_id': source_type,
                'target_type_id': target_type,
                'callable_id': callable_id,
                'field_mappings': [
                    {'source_field': source, 'target_field': target}
                    for source, target in sorted(mappings)
                ],
                'location': location_at(body_offset + literal_match.start(1)),
            })

    for call_match in re.finditer(r'\b([a-z_][A-Za-z0-9_]*|self)\s*\.\s*([a-z_][A-Za-z0-9_]*)\s*\([^()]*\)', body_masked):
        receiver, method = call_match.group(1), call_match.group(2)
        receiver_type = variable_types.get(receiver)
        if not receiver_type:
            continue
        target_method = f'{receiver_type}::{method}'
        before = body_masked[:call_match.start()]
        statement_start = max(before.rfind(';'), before.rfind('{'), before.rfind('}')) + 1
        prefix = re.sub(r'\s+', ' ', before[statement_start:]).strip()
        direct_control = bool(re.search(r'(?:^|\b)(?:if|while|match)(?:\s+let\s+.+?=\s*)?$', prefix))
        direct_return = bool(re.search(r'(?:^|\b)return\s*$', prefix))
        remainder = body_masked[call_match.end():].strip()
        tail_return = bool(re.fullmatch(r'\??\s*}', remainder))
        decision = direct_control or direct_return or tail_return
        call_return = function_returns.get(target_method)
        decision_domain = call_return if direct_control else (return_type if direct_return or tail_return else None)
        fact = {
            'caller_id': callable_id,
            'target_method_id': target_method,
            'receiver_type_id': receiver_type,
            'result_used_as_decision': decision,
            'location': location_at(body_offset + call_match.start(2)),
        }
        if decision and decision_domain:
            fact['decision_domain_type_id'] = decision_domain
        method_calls.append(fact)

    for branch_match in re.finditer(r'\b(?:if|while)\s+let\s+(.+?)\s*=\s*([^\{]+)\{', body_masked, re.S):
        pattern = branch_match.group(1).strip()
        domain = expression_type(branch_match.group(2).strip(), variable_types)
        if domain:
            pattern_offset = branch_match.start(1) + len(branch_match.group(1)) - len(branch_match.group(1).lstrip())
            decision_branches.append({
                'callable_id': callable_id,
                'domain_type_id': domain,
                'discriminant_id': discriminant_id(pattern, domain),
                'location': location_at(body_offset + pattern_offset),
            })

    for match_match in re.finditer(r'\bmatch\s+([^\{]+)\{', body_masked, re.S):
        domain = expression_type(match_match.group(1).strip(), variable_types)
        opening = body_masked.find('{', match_match.start(), match_match.end())
        closing = matching_delimiter(body_masked, opening)
        if not domain or closing is None:
            continue
        arms = body_masked[opening + 1:closing]
        for start, end in split_top_level_spans(arms):
            arm = arms[start:end]
            arrow = arm.find('=>')
            if arrow < 0:
                continue
            pattern = arm[:arrow].strip()
            if not pattern:
                continue
            pattern_start = start + len(arm[:arrow]) - len(arm[:arrow].lstrip())
            decision_branches.append({
                'callable_id': callable_id,
                'domain_type_id': domain,
                'discriminant_id': discriminant_id(pattern, domain),
                'location': location_at(body_offset + opening + 1 + pattern_start),
            })


def stable_unique(items, key):
    unique = {}
    for item in items:
        unique[json.dumps(item, sort_keys=True, separators=(',', ':'))] = item
    return sorted(unique.values(), key=key)


aggregate_definitions = stable_unique(aggregate_definitions, lambda item: (
    item['type_id'], item['location']['line'], item['location']['column']))
field_accesses = stable_unique(field_accesses, lambda item: (
    item['owner_type_id'], item['field'], item['callable_id'], item['access'],
    item['location']['line'], item['location']['column']))
aggregate_projections = stable_unique(aggregate_projections, lambda item: (
    item['source_type_id'], item['target_type_id'], item['callable_id'],
    item['location']['line'], item['location']['column']))
decision_branches = stable_unique(decision_branches, lambda item: (
    item['callable_id'], item['domain_type_id'], item['discriminant_id'],
    item['location']['line'], item['location']['column']))
method_calls = stable_unique(method_calls, lambda item: (
    item['caller_id'], item['target_method_id'], item.get('receiver_type_id', ''),
    item['location']['line'], item['location']['column']))

# --- Aggregate construction facts ---
# Rust-specific syntax recognition lives in the Rust extension. Homeboy core
# consumes these as generic aggregate construction facts.
def to_snake_case(name):
    out = []
    for i, ch in enumerate(name):
        if ch.isupper():
            if i > 0:
                out.append('_')
            out.append(ch.lower())
        else:
            out.append(ch)
    return ''.join(out)

def is_canonical_constructor(method, type_name):
    if method in {'new', 'builder', 'default'}:
        return True
    if method.startswith(('from_', 'for_', 'with_')):
        return True
    snake_type = to_snake_case(type_name)
    return method in {f'build_{snake_type}', f'create_{snake_type}'}

aggregate_construction_seams = []
seen_seams = set()
for fn in functions:
    if fn.is_test or fn.is_test_helper or not fn.impl_type:
        continue
    if is_canonical_constructor(fn.name, fn.impl_type):
        key = (fn.impl_type, fn.name)
        if key in seen_seams:
            continue
        seen_seams.add(key)
        aggregate_construction_seams.append({
            'type_name': fn.impl_type,
            'method': fn.name,
            'line': fn.line_num,
        })

aggregate_literals = []
seen_literals = set()
literal_pattern = re.compile(r'\b([A-Z][A-Za-z0-9_]*)\s*\{([^{};]*)\}', re.S)
definition_before_pattern = re.compile(r'(?:struct|enum|impl|trait|type|use)\s+$')
for m in literal_pattern.finditer(content):
    type_name_literal = m.group(1)
    before = content[:m.start()]
    if definition_before_pattern.search(before[-80:]):
        continue
    body = m.group(2)
    fields = []
    for field_match in re.finditer(r'\b([a-z_][A-Za-z0-9_]*)\s*:', body):
        field = field_match.group(1)
        if field not in fields:
            fields.append(field)
    if len(fields) < 2:
        continue
    line = before.count('\n') + 1
    key = (type_name_literal, tuple(fields), line)
    if key in seen_literals:
        continue
    seen_literals.add(key)
    aggregate_literals.append({
        'type_name': type_name_literal,
        'fields': fields,
        'line': line,
    })

result = {
    'methods': methods,
    'type_name': type_name,
    'type_names': pub_types,
    'extends': extends,
    'implements': implements,
    'registrations': registrations,
    'namespace': namespace,
    'imports': imports,
    'method_hashes': method_hashes,
    'structural_hashes': structural_hashes,
    'visibility': visibility,
    'unused_parameters': unused_parameters,
    'dead_code_markers': dead_code_markers,
    'internal_calls': internal_calls,
    'public_api': public_api,
    'aggregate_literals': aggregate_literals,
    'aggregate_construction_seams': aggregate_construction_seams,
    'aggregate_definitions': aggregate_definitions,
    'field_accesses': field_accesses,
    'aggregate_projections': aggregate_projections,
    'decision_branches': decision_branches,
    'method_calls': method_calls,
}

print(json.dumps(result))
PY
)

echo "$CONTENT"
