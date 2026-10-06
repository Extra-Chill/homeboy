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
        return dict(line.split('=', 1) for line in output.read_text().splitlines() if '=' in line)

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

    flagged = resolve(RUSTFLAGS='-C opt-level=1')
    assert flagged['restore-keys'] != changed['restore-keys'], 'flags partition compatibility'
    configured = resolve(CARGO_PROFILE_RELEASE_OPT_LEVEL='1')
    assert configured['restore-keys'] != changed['restore-keys'], 'profile overrides partition compatibility'
    toolchains = run('rustup', 'toolchain', 'list').stdout.splitlines()
    current = run('rustc', '-vV').stdout
    for line in toolchains:
        toolchain = line.split()[0]
        env = dict(os.environ, RUSTUP_TOOLCHAIN=toolchain)
        if run('rustc', '-vV', env=env).stdout != current:
            other_compiler = resolve(RUSTUP_TOOLCHAIN=toolchain)
            assert other_compiler['restore-keys'] != changed['restore-keys']
            break
    shutil.rmtree(target)
    print('PASS: native dev/release isolation, cold/warm restore, source refresh and dependency reuse')
