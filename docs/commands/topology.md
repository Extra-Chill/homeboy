# `homeboy topology`

Inspect declared resource relationships without resolving effective
configuration.

## Synopsis

```sh
homeboy topology <KIND> <ID>
```

## Arguments

- `<KIND>` — kind of the root resource to inspect: `component`, `project`,
  `server`, `fleet`, or `runner`
- `<ID>` — ID of the root resource to inspect

## Behavior

`topology` starts from the named root and reports the resources reachable
through declared relationships, as a snapshot of resource references, directed
edges, and diagnostics. The declared relationships are:

- a fleet contains a project
- a project targets a server
- a project uses a component
- a runner uses a server

It reads declarations only; it does not resolve effective configuration, so it
shows what was declared rather than the merged result an operation would act
on. A relationship that cannot resolve fully — for example, one that names a
resource that does not exist — is reported as a diagnostic rather than dropped.

```sh
homeboy topology component homeboy
homeboy topology fleet production
```
