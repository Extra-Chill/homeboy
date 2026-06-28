# Homeboy

Homeboy is a component-aware automation CLI for modern software work: many branches, many worktrees, many agents, and many projects moving at once.

It gives local developers, CI jobs, scheduled automation, and coding agents the same operational surface for checks, reviews, tests, benchmarks, traces, releases, and evidence. Humans get readable terminal output; automation gets stable JSON artifacts.

Homeboy core is intentionally domain-agnostic. The CLI owns orchestration, configuration, structured output, persisted runs, baselines, remote execution, and release/evidence workflows. Domain-specific behavior lives in extensions such as Rust, WordPress, Node.js, GitHub, Homebrew, Swift, and custom team extensions.

## What You Can Do

- Run repeatable quality gates with `homeboy audit`, `homeboy lint`, `homeboy test`, `homeboy build`, and `homeboy review`.
- Produce structured evidence with `--output` so CI jobs and coding agents can inspect results without scraping terminal logs.
- Capture benchmark, fuzz, trace, and review artifacts as persisted runs.
- Coordinate many branches and worktrees with comparable checks, reports, and PR evidence.
- Use runners for hot commands, remote execution, and durable agent workflows.
- Plan releases, versions, changelogs, tags, and deploy steps from component metadata.

## Start Here

New to Homeboy? Start with [docs/start-here.md](docs/start-here.md). It walks through the first local review gate, the smallest useful `homeboy.json`, JSON evidence, CI, and where each kind of documentation lives.

The shortest useful loop is:

```bash
homeboy review --changed-since origin/main
homeboy review --changed-since origin/main --output homeboy-results/review.json
```

Portable repo config starts with `homeboy.json`:

```json
{
  "id": "my-project",
  "extensions": {
    "rust": {}
  }
}
```

## Common Paths

- [Start here](docs/start-here.md) - first-time setup and first successful run.
- [Workflows](docs/workflows/index.md) - task guides for reviews, CI reproduction, evidence, runners, and releases.
- [Concepts](docs/concepts/index.md) - mental models for components, extensions, evidence, runners, and scope.
- [Reference](docs/reference/index.md) - command, config, schema, output, and template references.
- [Internals](docs/internals/index.md) - maintainer architecture, contracts, and docs-maintenance guidance.
- [Operations](docs/operations/index.md) - runbooks for release-gate proof, runner setup, and artifact publication.

## Installation

```bash
# Homebrew (macOS/Linux)
brew tap Extra-Chill/homebrew-tap
brew install homeboy

# Cargo
cargo install homeboy

# From source
git clone https://github.com/Extra-Chill/homeboy.git
cd homeboy && cargo install --path .
```

## Documentation

Documentation is checked into this repo and embedded in the binary:

```bash
homeboy docs list
homeboy docs index
homeboy docs commands/commands-index
```

The checked-in documentation starts at [docs/index.md](docs/index.md).

## Name

Named after ["Homeboy" by SUSTO](https://www.youtube.com/watch?v=-bBvwfn2ibU), a Charleston band.

## License

MIT License - Created by [Chris Huber](https://chubes.net)
