# project subcommands

## project create

```bash
homeboy project create <name> --type <type> [--id <id>]
```

## project show

```bash
homeboy project show <id> [--field <field>]
```

## project set

```bash
homeboy project set <id> [options]
```

## project delete

```bash
homeboy project delete <id> --force
```

## project switch

```bash
homeboy project switch <id>
```

## project subtarget

```bash
homeboy project subtarget add <project> <id> --name <name> --domain <domain> [--number <n>] [--is-default]
homeboy project subtarget remove <project> <id> --force
homeboy project subtarget list <project>
homeboy project subtarget set <project> <id> [--name <name>] [--domain <domain>] [--number <n>] [--is-default]
```

## project discover

```bash
homeboy project discover <project>
homeboy project discover <project> --list
homeboy project discover <project> --set <path>
```

## project component

```bash
homeboy project component add <project> <component-id...> [--skip-errors]
homeboy project component remove <project> <component-id> --force
homeboy project component list <project>
```
