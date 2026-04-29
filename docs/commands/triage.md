# `homeboy triage`

Produce a read-only attention report for components, projects, fleets, rigs, or the full configured workspace.

## Synopsis

```sh
homeboy triage [OPTIONS] <COMMAND>
```

## Subcommands

- `component` — triage one registered component
- `project` — triage every component attached to a project
- `fleet` — triage unique components used across a fleet
- `rig` — triage components declared in a local rig spec
- `workspace` — triage every configured project, rig, and registered component once per repo

## Useful filters

- `--issues` / `--prs` — restrict which GitHub item types are included
- `--mine` — show work assigned to or authored by the authenticated GitHub user
- `--assigned <USER>` — restrict to one assignee
- `--label <LABEL>` — restrict to one label; repeatable
- `--needs-review` — restrict PRs to review-required items
- `--failing-checks` — restrict PRs to failing-check items
- `--drilldown` — include compact failing check names and URLs

## Related

- [status](status.md)
- [issues](issues.md)
- [review](review.md)
