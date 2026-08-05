# Artifact Postprocess Runner Contract

Homeboy core owns a product-neutral artifact postprocess contract for persisted artifact roots.

The contract schema is `homeboy/artifact-postprocess/v1`. A plan declares:

- `artifact_roots`: persisted artifact roots or runner artifact refs the postprocess action reads from.
- `actions`: helper/action invocations with optional inputs, parameters, required flags, `side_effects: ["artifact_root_output"]`, and output paths confined under the artifact root. Other declared side effects are rejected by the contract.
- `reviewer_refs`: reviewer-facing URLs for produced evidence.
- `metadata`: generic object metadata for the producer.

The result schema is `homeboy/artifact-postprocess-result/v1`. Core records action outputs and produced artifacts without interpreting product semantics.

Output paths are relative paths and may not contain absolute, current-directory, parent-directory, or platform prefix components. Reviewer refs must be shareable evidence refs, not local filesystem paths or localhost URLs.

The runner gives each invocation a private staging artifact root. A supervisor writes a durable completion record with helper exit status, output digest, and produced artifacts before promotion. It atomically promotes that staged output and checkpoints the terminal outcome. Recovery adopts a valid completed stage (or its already-promoted digest) without invoking the helper again; incomplete or invalid stages are discarded and rerun.
