# `homeboy wp`

Run WP-CLI commands through an extension-provided top-level CLI verb.

## Synopsis

```sh
homeboy wp <project_id> [args]...
```

For multisite projects, use `project:subtarget` as the project ID.

`wp` is not a core subcommand compiled into Homeboy. It is registered at runtime
by an installed extension with CLI tool metadata, typically the WordPress
extension. Use `homeboy docs list` to confirm the command is available in the
current installation.

## Examples

```sh
homeboy wp my-site plugin list
homeboy wp my-site option get blogname
homeboy wp extra-chill:events sampleplugin pipelines list
```

## Related

- [extension](extension.md)
