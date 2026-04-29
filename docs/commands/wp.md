# `homeboy wp`

Run WP-CLI commands through Homeboy's WordPress extension routing.

## Synopsis

```sh
homeboy wp <project_id> [args]...
```

For multisite projects, use `project:subtarget` as the project ID.

## Examples

```sh
homeboy wp my-site plugin list
homeboy wp my-site option get blogname
homeboy wp extra-chill:events datamachine pipelines list
```

## Related

- [extension](extension.md)
