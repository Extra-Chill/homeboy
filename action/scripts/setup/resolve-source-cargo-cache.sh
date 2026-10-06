#!/usr/bin/env bash
set -euo pipefail

source_input="${SOURCE_PATH:-.}"
source_dir="$(cd "${source_input}" && pwd)"
lock_file="${source_dir}/Cargo.lock"

if [ ! -f "${lock_file}" ]; then
  echo "enabled=false" >> "${GITHUB_OUTPUT}"
  exit 0
fi

# Source tooling and extension quality builds have distinct target owners.
# A quality restore must never replace the source artifacts before cache save.
export SOURCE_CACHE_ROOT="${source_dir}"
python3 - <<'PY' >> "${GITHUB_OUTPUT}"
import hashlib
import json
import os
import pathlib
import subprocess

root = pathlib.Path(os.environ['SOURCE_CACHE_ROOT'])
digest = lambda value: hashlib.sha256(value).hexdigest()
compiler = subprocess.check_output(['rustc', '-vV']).decode()
profile = os.environ.get('SOURCE_BUILD_PROFILE', 'release')
configuration = {'source_root': str(root), 'runner_image': os.environ.get('ImageOS', '')}
for name in ('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_TARGET',
             'CARGO_INCREMENTAL', 'SOURCE_BUILD_COMMAND'):
    configuration[name] = os.environ.get(name, '')
configuration.update({key: value for key, value in os.environ.items()
                      if key.startswith('CARGO_PROFILE_') or key.startswith('CARGO_TARGET_')})
files = [root / 'Cargo.toml', root / '.cargo/config', root / '.cargo/config.toml']
for directory in root.parents:
    files.extend([directory / '.cargo/config', directory / '.cargo/config.toml'])
files.extend([pathlib.Path(os.environ.get('CARGO_HOME', str(pathlib.Path.home() / '.cargo'))) / name
              for name in ('config', 'config.toml')])
configuration['files'] = [(str(path.relative_to(root)) if path.is_relative_to(root) else str(path),
                           digest(path.read_bytes())) for path in files if path.is_file()]
compatibility = digest(json.dumps([compiler, profile, configuration], sort_keys=True).encode())
lock = digest((root / 'Cargo.lock').read_bytes())
revision = subprocess.run(['git', '-C', str(root), 'rev-parse', 'HEAD^{tree}'],
                          capture_output=True, text=True)
if revision.returncode == 0:
    tree = revision.stdout.strip()
else:
    source = [(str(path.relative_to(root)), digest(path.read_bytes()))
              for path in sorted(root.rglob('*')) if path.is_file()
              and 'target' not in path.relative_to(root).parts and '.git' not in path.relative_to(root).parts]
    tree = digest(json.dumps(source).encode())
prefix = f"{os.environ.get('RUNNER_OS', os.uname().sysname)}-cargo-source-v2-{compatibility}-{lock}-"
cache_home = pathlib.Path(os.environ.get('XDG_CACHE_HOME', str(pathlib.Path.home() / '.cache')))
source_target = cache_home / 'homeboy-action/source-targets' / digest(str(root).encode())
target = pathlib.Path(os.environ.get('SOURCE_TARGET_DIR', str(source_target)))
if not target.is_absolute():
    target = pathlib.Path.cwd() / target
print('enabled=true')
print(f'key={prefix}{tree}')
print(f'restore-keys={prefix}')
print(f'target-path={target}')
print('paths<<HOMEBOY_CACHE_PATHS')
print(pathlib.Path.home() / '.cargo/registry')
print(pathlib.Path.home() / '.cargo/git')
print(target)
print('HOMEBOY_CACHE_PATHS')
PY
