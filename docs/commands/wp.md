# `homeboy wp`

Run WP-CLI commands through an extension-provided top-level CLI verb.

## Synopsis

```sh
homeboy wp <project_id> [args]...
```

For multisite projects, use `project:subtarget` as the project ID.

`wp` is not a core subcommand compiled into Homeboy. It is registered at runtime
by an installed extension with CLI tool metadata, typically the WordPress
extension. Static core docs describe the extension contract; availability in a
specific installation depends on installed extension metadata and can be
confirmed with `homeboy --help` or `homeboy docs list`.

## Examples

```sh
homeboy wp my-site plugin list
homeboy wp my-site option get blogname
homeboy wp extra-chill:events sampleplugin pipelines list
```

## Related

- [extension](extension.md)
