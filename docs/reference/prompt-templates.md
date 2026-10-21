# Agent-task prompt templates

Homeboy authors no agent-facing prose. A Cook attempt's model-visible
instructions are the operator's prompt plus, when the operator declared one, the
template for that attempt's purpose, rendered against structured Cook facts.
With no template declared for a purpose, the model sees the operator's prompt
unchanged and the facts stay available as request data.

Templates live in `agent_task.prompt_templates`, keyed by purpose. A value is
inline text or a stored prompt reference (`prompt:<id>`, managed with
`homeboy agent-task prompts`):

```bash
homeboy agent-task prompts save cook-no-change --input @no-change.md
homeboy config set /agent_task/prompt_templates/no_change prompt:cook-no-change
```

## Purposes

| Purpose | Rendered for | Purpose-specific facts |
| --- | --- | --- |
| `attempt` | The first provider attempt of a Cook. | `inputs.homeboy` |
| `follow_up` | A gate-fix attempt after deterministic gates failed on the candidate. | `inputs.cook_loop` (`failed_gates`, `failure_progression`, `retry_policy`, `current_diff`, `changed_files`) |
| `no_change` | A retry after an attempt that changed no files. | `inputs.cook_loop` (`no_change_feedback: true`, `failed_gates` observed on the unchanged workspace) |
| `review_form` | A read-only attempt whose remaining work is the review form. | `inputs.cook_loop` (`review_form_feedback`, `changed_files`, `current_diff`) |

Every follow-up purpose also carries the attempt counters in `inputs.cook_loop`:
`attempt`, `next_attempt`, `max_attempts`,
`retry_budget_remaining_after_dispatch`, and `promotion_status`.

Each attempt renders from the operator's original prompt, so a template
replaces the previous attempt's template rather than accumulating.

## Placeholders

`{{ dotted.path }}` resolves against this context:

- `purpose` — the purpose name.
- `instructions` — the operator's prompt.
- `task_id`
- `inputs` — including `inputs.homeboy` (controller facts such as
  `gates.verify`, `gates.private_verify_count`, `publication.owner`) and
  `inputs.cook_loop` (follow-up facts above).
- `policy`, `outputs` (the request's output declarations), `metadata`.

Strings render raw, other values as pretty JSON, and missing paths as empty
text. There are no conditionals; one template per purpose keeps each one
specific to its situation.

## Reference template set

Copy and adapt these. Each says what to do and what done looks like.

`attempt`:

```markdown
Deliver a committed change in this workspace that completes the task above,
with tests that fail without the change.

After you finish, the controller runs these gates: {{ inputs.homeboy.gates.verify }}
```

`follow_up`:

```markdown
## Cook attempt {{ inputs.cook_loop.next_attempt }} of {{ inputs.cook_loop.max_attempts }}

The previous candidate is in this workspace. These deterministic gates failed
on it:

{{ inputs.cook_loop.failed_gates }}

Fix the cause shown in each gate's output so every gate passes, keeping the
candidate's intent. Done means a committed change on top of the candidate.
```

`no_change`:

```markdown
## Cook attempt {{ inputs.cook_loop.next_attempt }} of {{ inputs.cook_loop.max_attempts }}

The previous attempt finished with no file changes. Gate results on the
unchanged workspace: {{ inputs.cook_loop.failed_gates }}

Build on what that attempt learned and deliver a committed change that
completes the task above, with tests. When the task needs no change, declare
a verified intentional no-change verdict.
```

`review_form`:

```markdown
The change is committed and its gates pass. Inspect it read-only and return
the review form. Outstanding: {{ inputs.cook_loop.review_form_feedback }}
```

## Declared outputs

Requests with output declarations (for example the `review_form`) also ask the
model for one JSON object under `outputs`. Agent runtimes append that request
from their own template, which an operator overrides per executor with
`executor.config.output_instructions_template` (inline text, or `""` to
disable). `{{ outputs }}` in the templates above exposes the same declarations.
