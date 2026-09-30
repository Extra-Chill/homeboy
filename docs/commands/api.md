# `homeboy api`

## Synopsis

```sh
homeboy api <COMMAND>
```

## Description

Make HTTP requests to a project’s configured API, manage project API credentials, and make generic HTTP requests to full URLs.

Project API requests use the project’s API configuration (`projects/<project_id>.json`) and any stored authentication from `homeboy api auth`.

## Subcommands

### `request`

```sh
homeboy api request <method> <project_id> <endpoint> [--apply] [--body <json>] [--form <key=value>]...
```

`<method>` is one of `GET`, `POST`, `PUT`, `PATCH`, or `DELETE`. Mutating methods (`POST`, `PUT`, `PATCH`, `DELETE`) require `--apply`; `GET` does not.

## Notes

- `auth login|set|get|remove|logout|status|profile` manages project API secrets and generic HTTP auth profiles in the OS keychain.
- `http get|request` makes generic HTTP requests to full URLs. Mutating `request` methods require `--apply`; `GET`, `HEAD`, and `OPTIONS` do not.
- `<endpoint>` is passed through as provided (example: `/wp/v2/posts`).
- `--body` is parsed as JSON. If parsing fails, the request is sent with `body: null`.
- `--form key=value` may be repeated for `POST`, `PUT`, and `PATCH` requests; form fields take precedence over `--body`.
- If `--body` and `--form` are omitted, `body` is `null`.
- `GET` is allowed without `--apply`; `POST`, `PUT`, `PATCH`, and `DELETE` require `--apply` before Homeboy sends the request.

## Output

JSON output is wrapped in the global envelope. `data` is the `homeboy::api::ApiOutput` struct.

## Related

- [project](project.md)
- [JSON output contract](../architecture/output-system.md)

## Project API Examples

```sh
homeboy api request GET myproject /wp/v2/posts
homeboy api request POST myproject /wp/v2/posts --apply --body '{"title": "New Post"}'
homeboy api request POST myproject /wp/v2/posts --apply --form title=Hello --form status=draft
```

## Auth Examples

```sh
homeboy api auth login --project <project_id> [--identifier <username_or_email>] [--password <password>]
homeboy api auth set --project <project_id> <variable> [value]
homeboy api auth get --project <project_id> <variable> [--redacted]
homeboy api auth remove --project <project_id> <variable>
homeboy api auth logout --project <project_id>
homeboy api auth status --project <project_id>
homeboy api auth profile set-basic <profile> [--username <username>] [--password <password>]
homeboy api auth profile set-bearer <profile> [--token <token>]
homeboy api auth profile status <profile>
homeboy api auth profile remove <profile>
```

## Generic HTTP Examples

```sh
homeboy api auth profile set-basic example-profile --username example-org
homeboy api http get https://logstash.example.com/logstash/... --proxy socks5://127.0.0.1:8080 --auth-profile example-profile
homeboy api http request POST --apply https://example.com/api --json '{"ok":true}' --header 'X-Example: value'
```
