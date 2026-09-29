# Gate toolchain readiness

Cook can fail before starting a verification suite when a required external
toolchain resource is missing or mismatched. Declare a probe with
`--gate-toolchain-spec`; it runs in the same selected environment as gates,
before provider work, and is retained for replay. Use `--gate-env-from` to map
the required host resource into the isolated gate environment.

For example, this checks that an explicitly mapped browser cache contains an
executable with the expected version before `npm test` can run:

```sh
homeboy agent-task cook ... \
  --gate-env-from 'PLAYWRIGHT_BROWSERS_PATH=HOME/.cache/ms-playwright' \
  --gate-toolchain-spec '{"command":"sh","probe_arguments":["-c","test -x \"$PLAYWRIGHT_BROWSERS_PATH/chromium\" && test \"$(\"$PLAYWRIGHT_BROWSERS_PATH/chromium\" --version)\" = \"Chromium 128.4\""]}' \
  --verify 'npm test'
```

Replace the example executable path and version with the pinned browser expected
by the project. A project can use `node -e` as the declared command to resolve
Playwright from its own dependencies, call `chromium.executablePath()`, and
compare that executable's version with the project's pinned Playwright browser
version. Keep the probe silent on failure so tool output does not disclose local
cache paths. The host cache is mapped explicitly; gate HOME and XDG directories
remain isolated, and unrelated host files are not copied into them.

If the probe fails, Cook reports a toolchain preflight failure before starting
the suite. Install the project's pinned browser into the declared cache, or
adjust the explicit `--gate-env-from NAME=SOURCE[/PATH]` mapping, then replay the
same command. No undeclared host browser is discovered or required.
