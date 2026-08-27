# Triaging a twin candidate

The `twin_type_declaration` detector reports two declarations with the same
field shape. Shape is what a detector can see. It is not what decides whether
the declarations are one thing written twice.

The rule, stated once:

> **Shape similarity is not semantic identity.** Two declarations that look
> alike are frequently two meanings that happen to have the same fields, and
> collapsing them is how a distinction the code was carrying gets destroyed.

This is not a caution about being careful. It is a measured result. Under
[#6761][6761] four candidates were investigated in sequence and **all four were
wrong in the same direction** — each looked like a mechanical collapse and was
not one. The two changes that were worth making both went the opposite way:
they made a distinction the code had lost visible again.

Read a finding as *a question raised*, never as *a verdict returned*.

## Why acting wrongly is not neutral

Collapsing a false twin is worse than leaving it alone, because the union type
makes previously-impossible states representable. When the wider type is also
serialized somewhere durable, the over-wide shape becomes a persisted format
that permits values no reader can interpret. Leaving a twin costs a hand-synced
copy. Collapsing a false one costs a format.

## Four discriminators

Each of these fired on a real candidate in this repository. Symbols are given
rather than line numbers, because line numbers rot and a document that cannot
notice its own staleness is the thing this page exists to warn about.

### 1. Lifecycle plus a disposition, not one lifecycle

`CleanupState`, `FinalizationState`, and `ArtifactRetentionStatus` in
`crates/contracts/homeboy-lifecycle-contract/src/run_lifecycle_record.rs` share
`Pending` and `Failed`, and two of the three share `Running` and `Succeeded`.
That is most of a lifecycle, three times.

They are not three spellings of one enum. `ArtifactRetentionStatus` has no
`Running` and no `Succeeded`, and `Retained` is not a phase — it is the outcome
of a retention decision that has already finished. `Preserved` belongs only to
cleanup, `NotRequested` only to finalization.

A union is roughly a dozen variants of which each subsystem legitimately uses
six or seven, so nonsense typechecks:

```rust
cleanup.state = Retained;             // meaningless
artifact_retention.status = Preserved; // meaningless
```

**Ask:** does one side carry terminal *outcomes* the others lack? Then the
shared part is the phase, and the outcome is a second, per-subsystem field.

There is real duplication inside this one, and it is worth fixing on its own:
`FinalizationState::NotRequested` and `ArtifactRetentionStatus::NotApplicable`
are two spellings of "this did not apply". Collapsing *those* carries none of
the risk above.

Tracked as [#13398][13398]. Note that the issue's own snapshot has already
drifted from the code — it describes three not-applicable spellings and
variants (`Expired`, `Deleted`, `Blocked`) that no longer exist. Check the
enums before quoting it.

### 2. A different vocabulary, not a different format

`JobStatus` in `crates/contracts/homeboy-api-jobs-contract/src/types.rs` carries
two functions returning `&'static str`, which reads as one value formatted two
ways. Only one was redundant.

`as_str` returns what `#[serde(rename_all)]` already produces. That one *was* a
copy, and it is now pinned to the serialized form by a test.

`run_status_label` spells the terminal states `pass` and `fail`, and
`homeboy runs list --status pass` filters on those strings. Merging it into
`as_str` breaks that filter through a string comparison **with no compile
error**. It is kept, and a test pins the distinction because the merge is what
a reader who sees "two label functions" reaches for first.

**Ask:** do the two spellings have different *consumers*? A CLI filter, a wire
contract, a durable record. If merging changes what some consumer matches on,
they are two vocabularies and the resemblance is a coincidence.

Landed as [#13396][13396].

### 3. A parse boundary, where the narrowing is the work

A type that takes `Option<Value>` off the wire and a type that narrows it to
`Option<Map<String, Value>>` share every field *name*. They are not duplicates.
The narrowing is the entire purpose of the second declaration.

The detector already handles this: it groups on each field's resolved `type_id`
rather than its name, so an unresolved shape is skipped instead of reported.
See the module documentation in
`crates/homeboy-code-audit/src/detectors/twin_types.rs`.

**Ask:** is one side the narrowed or validated form of the other? Then the pair
is a boundary, and the second declaration is what makes the boundary checkable.

### 4. One sequence under several bindings

`dispatch_with_provider_requirements` in
`crates/homeboy-agents/src/agent_task_dispatch_service.rs` and
`run_loaded_plan_with_derived_cook_baseline_in_optional_store` in
`crates/homeboy-agents/src/agent_task_service/execution.rs` both build a harvest
context, mark running, run the scheduler, and record an aggregate. About
twenty-five lines, written twice.

They differ on four independent axes — whether a lifecycle store is threaded,
whether a derived cook baseline is carried, which scheduler entry point is
called, and whether snapshot attestation runs — and they return different
shapes. A facade covering all of that is a four-parameter configuration object
standing in front of twenty-five lines, in one of the repository's most
defect-dense crates. A third consumer, `AgentTaskFanoutScheduler`, was not in
the original count.

**Ask:** how large is the shared part *relative to the axes of variation*? If
unifying requires an option per caller, the sequence was never the abstraction.

Declined under [#6761][6761], with the analysis recorded there.

## Two signals that are worth acting on

The rule above is mostly about restraint. These are the shapes that justify a
change, and both of them *add* enforcement rather than removing a declaration.

### A projection that claims to be total and is not

Score a candidate on **"does this remove a place where a correct-looking change
is wrong"** — never on lines removed.

`AgentTaskRunState`'s projection carried a doc comment asserting the two enums
"share every agent-task variant 1:1". The match beneath it was eight variants to
six. `CandidateRecoverable`, `PartialRecoverable`, and `PartialFailure` all
collapsed to `PartialFailure`, so an orchestrator reading the projected state
could not tell "there is a promotable candidate" from "this partially failed".

Landed as [#13395][13395].

### A copy with nothing tying it to its source

A hand-written function that restates a value some other mechanism already
derives — a `serde` attribute, a config key, a filename convention — will drift,
because nothing fails when the two disagree.

The fix is usually **not** to merge the copy. It is to pin it: a test asserting
that the copy still equals its source. That keeps a genuinely useful second
spelling while removing the drift.

## The reason the first case survived

`AgentTaskRunState`'s projection was wrong for a long time, in a repository with
a test suite, an audit gate, and a lint gate. What protected the error was that
the false claim lived in a doc comment.

**A comment asserting a property cannot fail.** If a property matters, it needs
a test. This generalizes well past twin types, and it applies to this page: the
worked examples above are prose, so they carry symbol names to grep rather than
line numbers to trust, and one of them already had to be corrected against the
issue that first described it.

[6761]: https://github.com/Extra-Chill/homeboy/issues/6761
[13395]: https://github.com/Extra-Chill/homeboy/pull/13395
[13396]: https://github.com/Extra-Chill/homeboy/pull/13396
[13398]: https://github.com/Extra-Chill/homeboy/issues/13398
