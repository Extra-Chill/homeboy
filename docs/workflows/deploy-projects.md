# Deploy Projects

Use project, deploy, and status commands when Homeboy is operating configured environments instead of only local quality gates. This workflow is for operator-controlled deployment.

## Use This When

- You need to know what is actually deployed to a project.
- A component should be deployed to a configured project.
- You need remote status or file operations on a configured project.

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

Deploy targets one project per invocation. Run one invocation per project when the same component ships to several projects.

## 3. Apply Deployment Deliberately

After checking the plan, run the deployment:

```bash
homeboy deploy <project-id> <component-id>
```

Dangerous modes require explicit confirmation:

```bash
homeboy deploy <project-id> <component-id> --head --confirm-dangerous
homeboy deploy <project-id> <component-id> --force --confirm-dangerous
```

Prefer release tags or accepted stable refs for production. Deploying branch HEADs is an operator decision and should not be the default path.

## 4. Run Remote Commands

Use `homeboy ssh <project-id> -- <command>` for one-off remote inspection, for example reading a log file or running `wp` on the target.

## 5. Capture Evidence

For automation or incident review, write JSON output:

```bash
homeboy --output /tmp/homeboy-results/deploy-check.json deploy <project-id> --check
```

Pair deployment evidence with release evidence when a reviewer needs to understand merged -> released -> deployed state.

## Reference

- [deploy command](../commands/deploy.md)
- [status command](../commands/status.md)
- [project command](../commands/project.md)
- [Capture evidence](capture-evidence.md)
