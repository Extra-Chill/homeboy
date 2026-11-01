"""Prove profile isolation and restored release reuse using real Cargo artifacts."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

resolver = Path(__file__).with_name('resolve-source-cargo-cache.sh')


def run(*args, cwd=None, env=None):
    return subprocess.run(args, cwd=cwd, env=env, text=True, capture_output=True, check=True)


with tempfile.TemporaryDirectory() as temporary:
    root = Path(temporary)
    source = root / 'source'
    source.mkdir()
    (source / 'src').mkdir()
    (source / 'dependency/src').mkdir(parents=True)
    (source / 'Cargo.toml').write_text(
        '[package]\nname="cache-fixture"\nversion="0.1.0"\nedition="2021"\n'
        '[dependencies]\ncache-dependency={path="dependency"}\n'
    )
    (source / 'src/main.rs').write_text('fn main() { println!("{}", cache_dependency::value()); }\n')
    (source / 'dependency/Cargo.toml').write_text(
        '[package]\nname="cache-dependency"\nversion="0.1.0"\nedition="2021"\n'
    )
    (source / 'dependency/src/lib.rs').write_text('pub fn value() -> u32 { 42 }\n')
    run('cargo', 'generate-lockfile', cwd=source)
    git_env = dict(os.environ, GIT_AUTHOR_NAME='Cache fixture', GIT_AUTHOR_EMAIL='fixture@example.test',
                   GIT_COMMITTER_NAME='Cache fixture', GIT_COMMITTER_EMAIL='fixture@example.test')
    (source / '.gitignore').write_text('target/\n')
    run('git', 'init', cwd=source)
    run('git', 'add', '.', cwd=source)
    run('git', 'commit', '-m', 'initial fixture', cwd=source, env=git_env)

    def resolve(profile='release', **overrides):
        output = root / 'outputs'
        output.unlink(missing_ok=True)
        env = dict(os.environ, SOURCE_PATH=str(source), SOURCE_BUILD_PROFILE=profile,
                   GITHUB_OUTPUT=str(output), XDG_CACHE_HOME=str(root / 'cache'), **overrides)
        run('bash', str(resolver), cwd=root, env=env)
        values = {}
        lines = iter(output.read_text().splitlines())
        for line in lines:
            if '<<' in line:
                name, delimiter = line.split('<<', 1)
                content = []
                for entry in lines:
                    if entry == delimiter:
                        break
                    content.append(entry)
                values[name] = '\n'.join(content)
            elif '=' in line:
                name, value = line.split('=', 1)
                values[name] = value
        return values

    def build(cache, release=True):
        env = dict(os.environ, CARGO_TARGET_DIR=cache['target-path'])
        command = ['cargo', 'build', '--locked', '--message-format=json']
        if release:
            command.append('--release')
        result = run(*command, cwd=source, env=env)
        return [json.loads(line) for line in result.stdout.splitlines()
                if json.loads(line).get('reason') == 'compiler-artifact']

    dev = resolve('dev')
    build(dev, release=False)
    release = resolve()
    assert dev['key'] != release['key'], 'dev artifacts cannot be an exact release cache hit'
    assert dev['restore-keys'] != release['restore-keys'], 'dev is not a release fallback'
    same_lock, compatible = release['restore-keys'].splitlines()
    assert release['key'].startswith(same_lock), 'exact tree key is tried before prefixes'
    assert same_lock.startswith(compatible), 'same lock is preferred over any compatible lock'
    assert len(same_lock.removeprefix(compatible)) == 65, 'same-lock prefix includes SHA-256 and dash'
    assert len(compatible.split('-')[-2]) == 64, 'broad fallback retains compatibility hash'
    for prefix in dev['restore-keys'].splitlines():
        assert not release['key'].startswith(prefix), 'no cross-profile fallback'
    assert '/homeboy-action/source-targets/' in release['target-path']
    assert not Path(release['target-path']).is_relative_to(source)
    assert not (source / 'target/release').exists(), 'quality target is not a source build owner'
    cold = build(release)
    assert all(not artifact['fresh'] for artifact in cold), cold

    # Model cache save/restore: compatible artifacts survive removal of the
    # source target, and a changed consumer source receives a new immutable key.
    snapshot = root / 'saved-release'
    target = Path(release['target-path'])
    shutil.copytree(target, snapshot)
    shutil.rmtree(target)
    shutil.copytree(snapshot, target)
    warm = build(resolve())
    assert all(artifact['fresh'] for artifact in warm), warm
    (source / 'src/main.rs').write_text('fn main() { println!("next: {}", cache_dependency::value()); }\n')
    run('git', 'add', '.', cwd=source)
    run('git', 'commit', '-m', 'change consumer source', cwd=source, env=git_env)
    changed = resolve()
    assert changed['key'] != release['key'], 'new source can save newly built artifacts'
    assert changed['restore-keys'] == release['restore-keys'], 'compatible dependencies are reusable'
    changed_artifacts = build(changed)
    dependency = next(artifact for artifact in changed_artifacts
                      if artifact['target']['name'] == 'cache_dependency')
    assert dependency['fresh'], 'a new source revision must reuse restored dependency artifacts'
    binary = next(artifact for artifact in changed_artifacts
                  if artifact['target']['name'] == 'cache-fixture')
    assert not binary['fresh'], 'changed candidate is compiled rather than using an old executable'
    assert run(str(Path(binary['executable']))).stdout.strip() == 'next: 42'

    # A version-only lock rewrite must miss the exact/same-lock cache but
    # still restore the compatible dependency target. Cargo decides freshness.
    lockfile = source / 'Cargo.lock'
    original_lock = lockfile.read_text()
    lockfile.write_text(original_lock + '\n# release lock rewrite\n')
    new_lock = resolve()
    assert new_lock['key'] != changed['key']
    new_same_lock, new_compatible = new_lock['restore-keys'].splitlines()
    assert new_same_lock != same_lock
    assert new_compatible == compatible
    assert not changed['key'].startswith(new_same_lock)
    assert changed['key'].startswith(new_compatible), 'old lock is a compatibility fallback'
    assert all(artifact['fresh'] for artifact in build(new_lock))
    lockfile.write_text(original_lock)

    flagged = resolve(RUSTFLAGS='-C opt-level=1')
    assert flagged['restore-keys'] != changed['restore-keys'], 'flags partition compatibility'
    configured = resolve(CARGO_PROFILE_RELEASE_OPT_LEVEL='1')
    assert configured['restore-keys'] != changed['restore-keys'], 'profile overrides partition compatibility'
    targeted = resolve(CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS='-C opt-level=1')
    for isolated in (flagged, configured, targeted):
        assert all(not changed['key'].startswith(prefix)
                   for prefix in isolated['restore-keys'].splitlines()), 'fallbacks retain configuration isolation'
    toolchains = run('rustup', 'toolchain', 'list').stdout.splitlines()
    # Read compilers from the resolver's working directory: a rust-toolchain
    # file above the test's own cwd (e.g. a monorepo root) must not make the
    # "current" compiler differ from the one the resolver actually sees.
    current = run('rustc', '-vV', cwd=root).stdout
    for line in toolchains:
        toolchain = line.split()[0]
        env = dict(os.environ, RUSTUP_TOOLCHAIN=toolchain)
        if run('rustc', '-vV', cwd=root, env=env).stdout != current:
            other_compiler = resolve(RUSTUP_TOOLCHAIN=toolchain)
            assert other_compiler['restore-keys'] != changed['restore-keys']
            break
    shutil.rmtree(target)
    print('PASS: native dev/release isolation, cold/warm restore, source refresh and dependency reuse')
