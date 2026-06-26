# `homeboy manifest`

## Synopsis

```sh
homeboy manifest
```

## Description

Prints the recursive command manifest as the standard Homeboy JSON envelope. The
payload includes visible and hidden command paths, aliases, mutation/operator
safety metadata, dry-run flags, output-contract notes, Lab routing metadata,
dangerous flags, risk exemptions, and command docs paths.

`risk_exemption` marks mutating commands whose action is explicit in the command
shape even though they do not yet expose a dry-run/apply guard. The manifest audit
helper reports visible mutating commands that lack dry-run, apply/dangerous flag,
or risk-exemption metadata; the initial gate enforces this for the first
high-risk command set while the broader audit remains report-only.

Use this command for automation that needs a machine-readable view of the CLI
surface. Human command discovery should use `homeboy --help`, command-specific
`--help`, or `homeboy docs`.
