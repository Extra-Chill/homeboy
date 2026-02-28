# Homebrew Extension

Publishes Homebrew formulas to your tap repository.

## What This Extension Does

- Clones your tap repository
- Copies formula files (`.rb`) to `Formula/`
- Commits and pushes changes

## What This Extension Does NOT Do

- Build binaries (requires platform access)
- Generate formula files (use cargo-dist or manually)
- Calculate sha256 hashes

## Requirements

1. **Formula file** already generated with correct URLs and hashes
2. **GitHub CLI** (`gh`) authenticated with push access to tap repo
3. **Binaries uploaded** to a release or download location

## Usage with cargo-dist

cargo-dist generates a complete formula as a release asset. Download it and pass to this extension:

```bash
# Download formula from release
gh release download v1.0.0 --pattern "*.rb" --dir /tmp

# Publish to tap
homeboy extension run homebrew --settings '{"artifacts": ["/tmp/myapp.rb"]}'
```

## Platform Limitations

Building macOS/Windows binaries from Linux requires cross-compilation toolchains (osxcross, etc.). See homeboy's [Cross-Compilation Guide](https://github.com/Extra-Chill/homeboy/blob/main/docs/cross-compilation.md) for details.
