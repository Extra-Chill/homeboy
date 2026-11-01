"""Source cache restores are universal; saves skip exact hits but keep PR scopes."""
from pathlib import Path

root = Path(__file__).resolve().parents[2]
for file, restore_name, save_name, resolver_id in (
    ('action.yml', 'Cache cargo (source build)', 'Save cargo (source build)', 'source-cargo-cache'),
    ('.github/workflows/ci.yml', 'Cache cargo', 'Save cargo', 'cargo-cache'),
):
    text = (root / file).read_text()

    def step(name):
        marker = '- name: ' + name + '\n'
        return text.split(marker, 1)[1].split('- name:', 1)[0]

    restore = step(restore_name)
    save = step(save_name)
    assert 'uses: actions/cache/restore@v5' in restore, file
    assert 'github.ref' not in restore, 'PRs must be able to restore shared caches'
    assert f'restore-keys: ${{{{ steps.{resolver_id}.outputs.restore-keys }}}}' in restore
    assert 'uses: actions/cache/save@v5' in save, file
    # The consumer CI is PR-only, so a default-branch-only save would never
    # write a cache and would drop a PR's reuse of its own earlier pushes.
    assert 'github.ref' not in save, 'PR-scoped saves must be kept'
    assert "outputs.cache-hit != 'true'" in save
    assert 'always()' not in save, 'failed builds must not save caches'
    assert f'key: ${{{{ steps.{resolver_id}.outputs.key }}}}' in save
print('PASS: source cache restore/save policy for composite and reusable workflow')
