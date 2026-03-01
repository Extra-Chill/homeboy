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

result = {
    'methods': methods,
    'type_name': type_name,
    'implements': implements,
    'registrations': registrations,
    'namespace': namespace,
    'imports': imports,
}

print(json.dumps(result))
")

echo "$CONTENT"
