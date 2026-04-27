:x: **3 new finding(s) on this PR:**

1. **docs** — Broken file reference `scripts/build/build.sh` (line 19) — target does not exist (`docs::runner::build`)
2. **docs** — Broken file reference `scripts/lint/lint-runner.sh` (line 17) — target does not exist (`docs::runner::lint`)
3. **docs** — Broken file reference `scripts/test/test-runner.sh` (line 18) — target does not exist (`docs::runner::test`)

_Full audit state below._
- Alignment score: **0.900**
- Outliers in current run: **4**
- Drift increased: **yes**
- Severity counts: **warning: 3**

<details><summary>Audit findings (6 shown)</summary>

```text
1. **docs/architecture/runner-contract.md** — broken_doc_reference — Broken file reference `scripts/build/build.sh` (line 19) — target does not exist
2. **docs/architecture/runner-contract.md** — broken_doc_reference — Broken file reference `scripts/lint/lint-runner.sh` (line 17) — target does not exist
3. **docs/architecture/runner-contract.md** — broken_doc_reference — Broken file reference `scripts/test/test-runner.sh` (line 18) — target does not exist
4. **src/commands/docs.rs** — outlier — (outlier)
5. **src/core/engine/undo/entry.rs** — outlier — (outlier)
6. **src/core/engine/undo/snapshot.rs** — outlier — (outlier)
```

</details>
