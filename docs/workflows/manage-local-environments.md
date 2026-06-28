# Manage Local Environments

Use rigs, stacks, and task worktrees when local work spans more than one checkout, branch, component, or agent task. These commands keep local environments reproducible instead of relying on hand-maintained shell notes.

## Use This When

- A project needs multiple components checked out and started together.
- A combined-fixes branch should be rebuilt from a base plus a known PR list.
- An agent or human needs an isolated task worktree.
- Local setup has drifted and needs a repeatable health check or repair path.

## 1. Start With Rig Inventory

List and inspect installed rigs before starting anything:

```bash
homeboy rig list
homeboy rig show <rig-id>
homeboy rig status <rig-id>
```

If a rig is package-backed, update its source metadata before assuming local specs are current:

```bash
homeboy rig sources list
homeboy rig sources refresh
homeboy rig update <rig-id>
```

## 2. Bring The Environment Up

Run the rig lifecycle in the normal order:

```bash
homeboy rig up <rig-id>
homeboy rig check <rig-id>
```

Use `check` as the first diagnostic when a service, symlink, build output, or component checkout is not in the expected state.

When finished:

```bash
homeboy rig down <rig-id>
```

## 3. Repair Drift

Use repair only after inspecting status/check output:

```bash
homeboy rig status <rig-id>
homeboy rig check <rig-id>
homeboy rig repair <rig-id>
```

Repair should fix declared rig drift. It is not a substitute for understanding failed checks or uncommitted component changes.

## 4. Sync Combined-Fixes Stacks

Rigs can reference stacks for components that need a combined branch. Preview first:

```bash
homeboy rig sync <rig-id> --dry-run
homeboy stack status <stack-id>
```

When the plan is correct, sync the stack:

```bash
homeboy rig sync <rig-id>
```

`stack sync` and `stack apply` mutate local branches. Use dry-run/status paths before branch mutation, and authenticate `gh` when stack reports need private PR state.

## 5. Create Task Worktrees

Use task worktrees when a human or agent needs an isolated branch tied to a component:

```bash
homeboy worktree create <component-id> --branch fix/example --from origin/main --task-url <issue-url>
homeboy worktree list
homeboy worktree status <worktree-id>
```

Remove only when the worktree is safe to discard:

```bash
homeboy worktree remove <worktree-id>
homeboy worktree cleanup
```

Removal refuses dirty worktrees, unpushed commits, primary checkouts, and paths outside the component checkout parent. Treat `--force` as an explicit operator decision, not routine cleanup.

## 6. Connect To Agent Workflows

Agent-task cooks can target a managed worktree:

```bash
homeboy agent-task cook \
  --repo <repo-id> \
  --to-worktree <repo-id>@fix-example \
  --verify "homeboy review <repo-id> --changed-since origin/main" \
  --prompt @task.txt
```

Use [Run agent task loops](run-agent-task-loops.md) when those one-shot cooks become a durable multi-step controller workflow.

## Reference

- [rig command](../commands/rig.md)
- [stack command](../commands/stack.md)
- [worktree command](../commands/worktree.md)
- [agent-task command](../commands/agent-task.md)
- [Use runners](use-runners.md)
