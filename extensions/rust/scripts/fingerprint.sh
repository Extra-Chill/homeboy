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
#    "internal_calls": [...], "public_api": [...]}
#
# Extracts structural information from Rust source files using text matching.
# Handles methods inside impl blocks and test methods inside #[cfg(test)] modules.

set -euo pipefail

# Read stdin into variable
INPUT=$(cat)

# Extract content from JSON — use python3 for reliable JSON parsing
CONTENT=$(printf '%s' "$INPUT" | python3 -c "
import json, sys, re, hashlib

data = json.load(sys.stdin)
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
    def __init__(self, name, line_num, impl_type, is_test, is_public, signature_lines, body_start_line):
        self.name = name
        self.line_num = line_num          # 1-indexed line number
        self.impl_type = impl_type        # None for free fns, 'Type' for impl methods
        self.is_test = is_test            # Inside #[cfg(test)] or has #[test]
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
        is_test = has_test_attr or ctx.is_in_test_module()
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
# All non-test functions. Include impl methods.
methods = []
seen = set()
for fn in functions:
    if fn.is_test:
        continue
    if fn.name == 'tests':
        continue
    if fn.name not in seen:
        methods.append(fn.name)
        seen.add(fn.name)

# --- Test Methods ---
# Functions inside #[cfg(test)] or with #[test] attribute.
# Included in the methods list with the test_ prefix so test_coverage
# can identify them. Functions that already start with test_ keep their
# name; others get prefixed (e.g. 'dedup_works' -> 'test_dedup_works').
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

result = {
    'methods': methods,
    'type_name': type_name,
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
}

print(json.dumps(result))
")

echo "$CONTENT"
