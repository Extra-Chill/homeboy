# Deploy And Operate Fleets

Use project, deploy, fleet, and status commands when Homeboy is operating configured environments instead of only local quality gates. This workflow is for operator-controlled deployment and fleet inspection.

## Use This When

- You need to know what is actually deployed to a project.
- A component should be deployed to one project, many projects, or every project that uses it.
- Multiple projects should be checked as a fleet.
- You need remote status, logs, files, database, or fleet fan-out operations.

## 1. Check Release And Deploy State Separately

Plain workspace status is git-state oriented:

```bash
homeboy status
```

Project status compares configured targets against latest release tags:

```bash
homeboy status <project-id>
homeboy status <project-id> --outdated
```

Do not treat `ready_to_deploy` as proof that a target is behind. Use project status for the installed-version question.

## 2. Preview Deployment

Start with read-only or planning commands:

```bash
homeboy deploy <project-id> --check
homeboy deploy <project-id> <component-id> --dry-run
homeboy deploy <project-id> --outdated --dry-run
```

For multiple projects:

```bash
homeboy deploy --projects <project-a>,<project-b> <component-id> --dry-run
```

For a fleet or shared component:

```bash
homeboy deploy <component-id> --fleet <fleet-id> --dry-run
homeboy deploy <component-id> --shared --dry-run
```

## 3. Apply Deployment Deliberately

After checking the plan, run the deployment:

```bash
homeboy deploy <project-id> <component-id>
```

Dangerous modes require explicit confirmation:

```bash
homeboy deploy <project-id> <component-id> --head --apply
homeboy deploy <project-id> <component-id> --force --apply
```

Prefer release tags or accepted stable refs for production. Deploying branch HEADs is an operator decision and should not be the default path.

## 4. Use Fleets For Groups Of Projects

Create and inspect fleets when the same operational question applies to many projects:

```bash
homeboy fleet create <fleet-id> --projects site-a,site-b,site-c
homeboy fleet status <fleet-id>
homeboy fleet check <fleet-id> --outdated
```

Deploy one component to the fleet:

```bash
homeboy deploy <component-id> --fleet <fleet-id>
```

Deploy to every configured project that uses the component:

```bash
homeboy deploy <component-id> --shared
```

## 5. Run Remote Operations With Plan/Apply Discipline

Fleet exec has an explicit check/apply boundary:

```bash
homeboy fleet exec <fleet-id> --check -- wp plugin list
homeboy fleet exec <fleet-id> --apply -- wp plugin list
```

Use `--check` first so the target set and command are visible before remote fan-out.

## 6. Capture Evidence

For automation or incident review, write JSON output:

```bash
homeboy --output homeboy-results/deploy-check.json deploy <project-id> --check
homeboy --output homeboy-results/fleet-status.json fleet status <fleet-id>
```

Pair deployment evidence with release evidence when a reviewer needs to understand merged -> released -> deployed state.

## Reference

- [deploy command](../commands/deploy.md)
- [fleet command](../commands/fleet.md)
- [status command](../commands/status.md)
- [project command](../commands/project.md)
- [Capture evidence](capture-evidence.md)
