# Artifact Postprocess Runner Contract

Homeboy core owns a product-neutral artifact postprocess contract for persisted artifact roots.

The contract schema is `homeboy/artifact-postprocess-plan/v1`. A plan declares:

- `artifact_roots`: persisted artifact roots or runner artifact refs the postprocess action reads from.
- `actions`: helper/action invocations with optional inputs, parameters, required flags, and output paths confined under the artifact root.
- `reviewer_refs`: reviewer-facing URLs for produced evidence.
- `metadata`: generic object metadata for the producer.

The result schema is `homeboy/artifact-postprocess-result/v1`. Core records action outputs and produced artifacts without interpreting product semantics.

Output paths are relative paths and may not contain absolute, current-directory, parent-directory, or platform prefix components. Reviewer refs must be shareable evidence refs, not local filesystem paths or localhost URLs.
