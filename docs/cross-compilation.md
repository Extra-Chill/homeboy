# Cross-Compilation Guide

Homeboy can orchestrate releases for any platform, but **building native binaries** requires access to that platform's toolchain.

## What Works Anywhere

| Task | Platform Required |
|------|-------------------|
| Version management | Any |
| Changelog operations | Any |
| Git operations (commit, tag, push) | Any |
| Deploy to servers via SSH | Any |
| Publish Homebrew formula | Any |
| Build Linux x86_64 binaries | Linux |

## What Requires Platform Access

| Task | Platform Required |
|------|-------------------|
| Build macOS binaries | macOS or osxcross |
| Build Windows binaries | Windows or cross-toolchain |
| Build Linux ARM binaries | Linux ARM or cross |

## Why?

Native binaries require platform-specific system libraries. For example, Rust crates like `keyring` and `security-framework` link against macOS frameworks (Security.framework, CoreFoundation.framework). These aren't available on Linux without the macOS SDK.

## Options for Cross-Platform Releases

### Option 1: Hybrid (Recommended)

Use homeboy for orchestration, GitHub Actions for cross-platform builds:

```
homeboy release → triggers GitHub Actions → builds all platforms → uploads release
homeboy extension run homebrew → publishes formula to tap
```

cargo-dist handles the cross-platform builds and generates the Homebrew formula with correct sha256 hashes.

### Option 2: Platform-Specific Build Servers

Run homeboy on each platform:
- macOS server builds macOS binaries
- Windows server builds Windows binaries  
- Linux server builds Linux binaries

Coordinate via fleet commands or post-release hooks.

### Option 3: osxcross (Linux → macOS)

Install osxcross to cross-compile for macOS from Linux:

```bash
# Install dependencies
apt-get install clang cmake libssl-dev libxml2-dev

# Clone and build osxcross (requires macOS SDK)
git clone https://github.com/tpoechtrager/osxcross
cd osxcross
# Download Xcode and extract SDK (see osxcross docs)
./build.sh

# Configure Rust
rustup target add x86_64-apple-darwin aarch64-apple-darwin
```

**Note:** The macOS SDK is ~1GB and Apple's license restricts redistribution. Many open-source projects use osxcross, but be aware of the legal gray area.

### Option 4: cargo-zigbuild (Partial)

Zig can cross-compile C code for macOS, but Rust crates with deep SDK dependencies (like `security-framework`) still fail:

```bash
# Install zig and cargo-zigbuild
cargo install cargo-zigbuild

# Works for simple crates
cargo zigbuild --release --target x86_64-apple-darwin

# Fails for crates needing Security.framework, etc.
```

## Homebrew Extension

The `homebrew` extension publishes formulas to your tap. It does **not** build binaries or generate formulas — it assumes:

1. Binaries are already built and uploaded (to GitHub Releases, etc.)
2. A formula file exists with correct download URLs and sha256 hashes

For full automation with cargo-dist:
1. cargo-dist builds binaries and generates `homeboy.rb`
2. Formula is uploaded as a release asset
3. `homeboy extension run homebrew` or post-release hook publishes to tap

## Recommended Setup

For Rust CLI projects distributing via Homebrew:

1. **cargo-dist** in GitHub Actions for cross-platform builds
2. **homeboy** for release orchestration (bump, changelog, deploy)
3. **homebrew extension** for tap publishing

This separates concerns:
- GitHub provides free macOS/Windows runners
- Homeboy handles the workflow you control
- No SDK licensing concerns

## Future

If Apple ever allows SDK redistribution or Rust improves cross-compilation support, homeboy could handle everything. Until then, the hybrid approach is pragmatic.
