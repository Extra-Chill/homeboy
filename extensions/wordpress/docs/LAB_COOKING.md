# Headless WordPress Agent Cooking on a Lab Runner

Homeboy can dispatch a coding task to a **WordPress-native AI agent that runs
fully headless** — inside a disposable WordPress instance, driven entirely
through WordPress abilities and the WordPress AI Client, with no browser, no
admin UI, and no external agent framework. With a connected **Lab runner**, that
agent runs on a separate machine over SSH while the controller (your laptop)
just dispatches the task and collects the result.

This is "cooking": give the agent a repo and a prompt, it edits files in a
mounted workspace, and Homeboy captures the diff as a reviewable artifact.

## What makes the agent WordPress-native

The agent is not a wrapper around an external CLI. The agent loop *is*
WordPress:

- **The loop** is the `agents/chat` ability provided by Agents API.
- **The tools** are WordPress abilities (`workspace_read`, `workspace_write`,
  `workspace_edit`, `workspace_git_add`, ...), projected by Data Machine Code.
- **The model** is reached through a WordPress AI Client provider plugin (for
  example an OpenAI, Codex, or Claude Code provider). The provider registers
  itself with the WP AI Client provider registry during WordPress boot.
- **The runtime** is real WordPress, booted disposably by WP Codebox
  (WordPress Playground), with Data Machine, Data Machine Code, and Agents API
  loaded as runtime components.

Nothing in the loop runs a browser or a vendor agent binary. WordPress is the
runtime, abilities are the tools, and the AI Client is the model transport.

## Architecture

```text
controller (your machine)
  homeboy agent-task cook
        |
        |  (optional) SSH offload to a Lab runner
        v
Lab runner (separate machine)
  homeboy executes the task locally on the runner
        |
        v
WP Codebox
  boots a disposable WordPress site (Playground), headless
  mounts: the repo workspace + runtime components + provider plugin
        |
        v
WordPress runtime
  Agents API  -> agents/chat (the agent loop)
  Data Machine + Data Machine Code -> abilities = workspace tools
  WP AI Client provider plugin -> the model
        |
        v
agent edits files in the mounted workspace
        |
        v
Homeboy captures the diff -> changed-files + patch artifacts
```

Running without a Lab runner uses the same path, executed locally instead of
over SSH. The Lab runner only changes *where* the work happens, not *how*.

## The cook command

```bash
homeboy agent-task cook \
  --repo data-machine \
  --cwd /path/to/worktree \
  --backend codebox \
  --provider-config @provider-config.json \
  --secret-env AI_PROVIDER_TOKEN \
  --prompt 'Fix the bug in inc/Example.php and stop.'
```

Useful flags:

| Flag | Purpose |
| --- | --- |
| `--repo` | Repo/component slug for metadata and grouping. |
| `--cwd` | Local repo checkout or worktree to cook in. |
| `--backend codebox` | Use the WP Codebox coding backend (the default). |
| `--provider-config` | JSON object, `@file`, or `-` (stdin) describing the runtime (see below). |
| `--secret-env` | Name of an environment variable to hydrate into the sandbox (repeatable). Values are passed by reference, never printed. |
| `--prompt` / `--task` | The task. Repeat `--task` (or pass `--tasks` JSON) to fan out into multiple cells. |
| `--concurrency` | Maximum task cells to run at once. |
| `--attempts` | Attempts per task, including the first. |
| `--run-id` | Durable run id (generated when omitted). |

Read the durable run afterward with `homeboy agent-task status <run-id>`,
`logs <run-id>`, and `artifacts <run-id>`.

## Provider config

The provider config describes the WordPress runtime the agent boots in. It is a
JSON object (passed inline, as `@file`, or on stdin via `--provider-config`).

```json
{
  "provider": "claude-code",
  "model": "claude-opus-4-8",
  "repo": "data-machine",
  "workspace_root": "/abs/path/to/worktree",
  "mounts": [
    {
      "source": "/abs/path/to/worktree",
      "target": "/workspace/data-machine",
      "mode": "readwrite",
      "metadata": { "kind": "homeboy-dmc-workspace", "workspace_slug": "data-machine" }
    }
  ],
  "runtime_component_paths": {
    "agent_runtime": "/abs/path/to/data-machine",
    "agent_runtime_tools": "/abs/path/to/data-machine-code"
  },
  "agents_api": "/abs/path/to/data-machine/vendor/wordpress/agents-api",
  "provider_plugin_paths": ["/abs/path/to/ai-provider-for-<name>"],
  "runtime_overlays": [
    {
      "kind": "bundled-library",
      "library": "php-ai-client",
      "source": "/abs/path/to/php-ai-client",
      "target": "/wordpress/wp-includes/php-ai-client",
      "strategy": "wordpress-scoped-bundle"
    }
  ],
  "secret_env": ["AI_PROVIDER_TOKEN"],
  "verify_steps": [
    { "command": "wordpress.phpunit", "args": ["plugin-slug=data-machine"] }
  ]
}
```

| Field | What it does |
| --- | --- |
| `provider` / `model` | The WP AI Client provider id and model the agent uses. |
| `mounts` | Directories mounted into the sandbox. The repo workspace mounts read-write at `/workspace/<slug>`; the agent's workspace tools operate there. |
| `workspace_root` | The repo checkout the agent cooks in. |
| `runtime_component_paths` | The WordPress runtime plugins to load as mu-plugins: `agent_runtime` (Data Machine) and `agent_runtime_tools` (Data Machine Code). |
| `agents_api` | The Agents API bundle (Data Machine vendors it) that provides `agents/chat`. |
| `provider_plugin_paths` | WordPress AI Client provider plugin(s) to load so the model provider registers. |
| `runtime_overlays` | Bundled libraries overlaid into the runtime. Use this to supply a specific `php-ai-client` build when a provider needs APIs newer than the one shipped with WordPress core. |
| `secret_env` | Names of environment variables to make available to the provider inside the sandbox (for example an API key or OAuth token). |
| `verify_steps` | Recipe steps run after the agent finishes (see Verify gate). |

## Verify gate

`verify_steps` run as recipe `workflow.after` steps *after* the agent finishes
editing, against the booted WordPress runtime. A non-zero exit from any verify
step fails the whole run, so the cook cannot report success unless its checks
pass. This turns "the agent finished" into "the agent finished and the gates are
green."

```json
"verify_steps": [
  { "command": "wordpress.phpunit", "args": ["plugin-slug=data-machine"] }
]
```

You can also run arbitrary PHP against the booted runtime with
`{ "command": "wordpress.run-php", "args": ["code=..."] }` or
`["code-file=/path/in/sandbox.php"]`.

## Running on a Lab runner

A Lab runner is a separate machine that executes Homeboy commands on your
behalf. Register and connect one with `homeboy runner` (see
`homeboy runner --help`: `add`, `connect`, `status`).

Once a default runner is connected, `homeboy agent-task cook` automatically
offloads to it. The controller:

1. syncs the `--cwd` workspace to the runner,
2. syncs the directories your provider config references — runtime components,
   the provider plugin, mount sources, and runtime overlay sources,
3. remaps those controller-local absolute paths to their synced locations on the
   runner, and
4. runs the cook on the runner, then copies artifacts back.

Because the workspace and every referenced path are synced and remapped, a
provider config authored with local paths runs unmodified on the runner — you
do not hand-edit paths for remote execution.

To force local execution even when a runner is connected, add
`--force-hot --allow-local-hot`.

### Where the agent's changes land

On a Lab cook the agent edits the **runner's synced copy** of the workspace, not
your local checkout. The diff is captured into the run's `changed-files` and
`patch` artifacts; read them with `homeboy agent-task artifacts <run-id>`.

## Fan-out (waves)

Pass multiple `--task` prompts (or `--tasks` as a JSON array) to run several
agent cells under one durable run, bounded by `--concurrency`. Each cell is an
isolated agent run; results aggregate under the run id. This is flat fan-out —
one orchestrator dispatching N independent cells — not agents spawning agents.

```bash
homeboy agent-task cook --repo data-machine --cwd /path/to/worktree \
  --backend codebox --provider-config @provider-config.json \
  --concurrency 2 \
  --task 'Fix issue A and stop.' \
  --task 'Fix issue B and stop.'
```

## Worked example

Cook a fix in a Data Machine worktree with a verify gate, on a Lab runner:

```bash
# 1. A clean worktree with dependencies installed.
git -C ~/Developer/data-machine worktree add ~/Developer/data-machine@cook -b cook/fix origin/main
( cd ~/Developer/data-machine@cook && composer install )

# 2. Provider config (paths are local; the runner sync + remap handles them).
cat > /tmp/cook.json <<'JSON'
{
  "provider": "claude-code",
  "model": "claude-opus-4-8",
  "repo": "data-machine",
  "workspace_root": "/Users/me/Developer/data-machine@cook",
  "mounts": [{ "source": "/Users/me/Developer/data-machine@cook", "target": "/workspace/data-machine", "mode": "readwrite", "metadata": { "kind": "homeboy-dmc-workspace", "workspace_slug": "data-machine" } }],
  "runtime_component_paths": { "agent_runtime": "/Users/me/Developer/data-machine", "agent_runtime_tools": "/Users/me/Developer/data-machine-code" },
  "agents_api": "/Users/me/Developer/data-machine/vendor/wordpress/agents-api",
  "provider_plugin_paths": ["/Users/me/Developer/ai-provider-for-claude-code"],
  "secret_env": ["AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN"]
}
JSON

# 3. Cook (auto-offloads to a connected Lab runner).
export AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN=...   # never printed; passed by name
homeboy agent-task cook \
  --repo data-machine \
  --cwd ~/Developer/data-machine@cook \
  --backend codebox \
  --provider-config @/tmp/cook.json \
  --secret-env AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN \
  --prompt 'Fix the bug described in the issue and stop.' \
  --run-id my-cook-001

# 4. Inspect the result.
homeboy agent-task status my-cook-001
homeboy agent-task artifacts my-cook-001   # changed-files + patch
```

A successful run reaches `succeeded`, the WordPress AI Client provider registers
inside the booted runtime, the agent writes its changes through workspace tools,
and the diff is captured as artifacts.

## Troubleshooting

| Symptom | Likely cause |
| --- | --- |
| `failed to read agent-task dispatch provider-config input: IO error` on a Lab cook | The provider-config path or a path inside it isn't reaching the runner. Ensure the referenced directories exist locally so they can be synced. |
| `Provider <id> is not registered in wp-ai-client` | The provider plugin didn't load, or its required AI Client APIs are missing. Confirm `provider_plugin_paths` points at the plugin, and add a `runtime_overlays` php-ai-client build if the provider needs newer AI Client APIs than WordPress core ships. |
| A test/verify step fatals on missing dependencies | The worktree needs its dependencies installed (for example `composer install`) before the gate runs. |
| `agents/chat unavailable` in the sandbox | The runtime components (Agents API, Data Machine, Data Machine Code) didn't mount. Check `runtime_component_paths` and `agents_api`. |

## Related

- `AGENT_CI_WP_CODEBOX.md` — running a Data Machine agent bundle from GitHub
  Actions CI on WP Codebox.
- `homeboy agent-task --help`, `homeboy runner --help` — full command surface.
