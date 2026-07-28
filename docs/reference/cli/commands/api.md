<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy api` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/api.md](../../../commands/api.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy api`

```sh
homeboy api <COMMAND>
```

Make API requests to a project

| Subcommand | Summary |
| --- | --- |
| `homeboy api auth` | Manage API credentials and auth profiles |
| `homeboy api http` | Make generic HTTP requests to full URLs |
| `homeboy api get` | Make a GET request |
| `homeboy api post` | Make a POST request |
| `homeboy api put` | Make a PUT request |
| `homeboy api patch` | Make a PATCH request |
| `homeboy api delete` | Make a DELETE request |

## `homeboy api auth`

```sh
homeboy api auth <COMMAND>
```

Manage API credentials and auth profiles

| Subcommand | Summary |
| --- | --- |
| `homeboy api auth login` | Authenticate with a project's API |
| `homeboy api auth set` | Store a project API variable in the OS keychain |
| `homeboy api auth get` | Read a project API variable from the OS keychain |
| `homeboy api auth remove` | Remove a project API variable from the OS keychain |
| `homeboy api auth logout` | Clear stored authentication for a project |
| `homeboy api auth status` | Show authentication status for a project |
| `homeboy api auth profile` | Manage reusable auth profiles for generic HTTP requests |

## `homeboy api auth login`

```sh
homeboy api auth login [OPTIONS]
```

Authenticate with a project's API

| Option | Value | Description |
| --- | --- | --- |
| `--project` | `<PROJECT>` | Project ID |
| `--identifier` | `<IDENTIFIER>` | Username or email |
| `--password` | `<PASSWORD>` | Password (or read from stdin) |

## `homeboy api auth set`

```sh
homeboy api auth set [OPTIONS] <VARIABLE> [VALUE]
```

Store a project API variable in the OS keychain

| Argument | Required | Description |
| --- | --- | --- |
| `<VARIABLE>` | yes | Variable name |
| `[VALUE]` | no | Secret value (or read from stdin) |

| Option | Value | Description |
| --- | --- | --- |
| `--project` | `<PROJECT>` | Project ID |

## `homeboy api auth get`

```sh
homeboy api auth get [OPTIONS] <VARIABLE>
```

Read a project API variable from the OS keychain

| Argument | Required | Description |
| --- | --- | --- |
| `<VARIABLE>` | yes | Variable name |

| Option | Value | Description |
| --- | --- | --- |
| `--project` | `<PROJECT>` | Project ID |
| `--redacted` | flag | Return a redacted marker instead of the secret value |

## `homeboy api auth remove`

```sh
homeboy api auth remove [OPTIONS] <VARIABLE>
```

Remove a project API variable from the OS keychain

| Argument | Required | Description |
| --- | --- | --- |
| `<VARIABLE>` | yes | Variable name |

| Option | Value | Description |
| --- | --- | --- |
| `--project` | `<PROJECT>` | Project ID |

## `homeboy api auth logout`

```sh
homeboy api auth logout [OPTIONS]
```

Clear stored authentication for a project

| Option | Value | Description |
| --- | --- | --- |
| `--project` | `<PROJECT>` | Project ID |

## `homeboy api auth status`

```sh
homeboy api auth status [OPTIONS]
```

Show authentication status for a project

| Option | Value | Description |
| --- | --- | --- |
| `--project` | `<PROJECT>` | Project ID |

## `homeboy api auth profile`

```sh
homeboy api auth profile <COMMAND>
```

Manage reusable auth profiles for generic HTTP requests

| Subcommand | Summary |
| --- | --- |
| `homeboy api auth profile set-basic` | Store a Basic auth profile in the OS keychain |
| `homeboy api auth profile set-bearer` | Store a Bearer token auth profile in the OS keychain |
| `homeboy api auth profile status` | Show whether an auth profile is available |
| `homeboy api auth profile remove` | Remove an auth profile from the OS keychain |

## `homeboy api auth profile set-basic`

```sh
homeboy api auth profile set-basic [OPTIONS] <PROFILE>
```

Store a Basic auth profile in the OS keychain

| Argument | Required | Description |
| --- | --- | --- |
| `<PROFILE>` | yes | Profile name |

| Option | Value | Description |
| --- | --- | --- |
| `--username` | `<USERNAME>` | Username |
| `--password` | `<PASSWORD>` | Password; omit to prompt securely |

## `homeboy api auth profile set-bearer`

```sh
homeboy api auth profile set-bearer [OPTIONS] <PROFILE>
```

Store a Bearer token auth profile in the OS keychain

| Argument | Required | Description |
| --- | --- | --- |
| `<PROFILE>` | yes | Profile name |

| Option | Value | Description |
| --- | --- | --- |
| `--token` | `<TOKEN>` | Token; omit to prompt securely |

## `homeboy api auth profile status`

```sh
homeboy api auth profile status <PROFILE>
```

Show whether an auth profile is available

| Argument | Required | Description |
| --- | --- | --- |
| `<PROFILE>` | yes | Profile name |

## `homeboy api auth profile remove`

```sh
homeboy api auth profile remove <PROFILE>
```

Remove an auth profile from the OS keychain

| Argument | Required | Description |
| --- | --- | --- |
| `<PROFILE>` | yes | Profile name |

## `homeboy api http`

```sh
homeboy api http <COMMAND>
```

Make generic HTTP requests to full URLs

| Subcommand | Summary |
| --- | --- |
| `homeboy api http get` | Make a GET request to a full URL |
| `homeboy api http request` | Make an arbitrary HTTP request to a full URL |

## `homeboy api http get`

```sh
homeboy api http get [OPTIONS] <URL>
```

Make a GET request to a full URL

| Argument | Required | Description |
| --- | --- | --- |
| `<URL>` | yes | Full URL to request |

| Option | Value | Description |
| --- | --- | --- |
| `--proxy` | `<PROXY>` | Optional proxy URL, e.g. socks5://127.0.0.1:8080 |
| `--auth-profile` | `<AUTH_PROFILE>` | Auth profile from `homeboy api auth profile ...` |
| `--header` | `<HEADERS>` | Header in `Name: value` format; repeatable |
| `--json` | `<JSON>` | JSON request body |
| `--form` | `<FORM>` | Form field as key=value; repeatable |

## `homeboy api http request`

```sh
homeboy api http request [OPTIONS] <METHOD> <URL>
```

Make an arbitrary HTTP request to a full URL

| Argument | Required | Description |
| --- | --- | --- |
| `<METHOD>` | yes | HTTP method |
| `<URL>` | yes | Full URL to request |

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Confirm the mutating request should be sent |
| `--proxy` | `<PROXY>` | Optional proxy URL, e.g. socks5://127.0.0.1:8080 |
| `--auth-profile` | `<AUTH_PROFILE>` | Auth profile from `homeboy api auth profile ...` |
| `--header` | `<HEADERS>` | Header in `Name: value` format; repeatable |
| `--json` | `<JSON>` | JSON request body |
| `--form` | `<FORM>` | Form field as key=value; repeatable |

## `homeboy api get`

```sh
homeboy api get <PROJECT_ID> <ENDPOINT>
```

Make a GET request

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<ENDPOINT>` | yes | API endpoint (e.g., /wp/v2/posts) |

## `homeboy api post`

```sh
homeboy api post [OPTIONS] <PROJECT_ID> <ENDPOINT>
```

Make a POST request

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<ENDPOINT>` | yes | API endpoint |

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Confirm the mutating request should be sent |
| `--body` | `<BODY>` | JSON body |
| `--form` | `<FORM>` | Form field as key=value. May be repeated |

## `homeboy api put`

```sh
homeboy api put [OPTIONS] <PROJECT_ID> <ENDPOINT>
```

Make a PUT request

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<ENDPOINT>` | yes | API endpoint |

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Confirm the mutating request should be sent |
| `--body` | `<BODY>` | JSON body |
| `--form` | `<FORM>` | Form field as key=value. May be repeated |

## `homeboy api patch`

```sh
homeboy api patch [OPTIONS] <PROJECT_ID> <ENDPOINT>
```

Make a PATCH request

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<ENDPOINT>` | yes | API endpoint |

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Confirm the mutating request should be sent |
| `--body` | `<BODY>` | JSON body |
| `--form` | `<FORM>` | Form field as key=value. May be repeated |

## `homeboy api delete`

```sh
homeboy api delete [OPTIONS] <PROJECT_ID> <ENDPOINT>
```

Make a DELETE request

| Argument | Required | Description |
| --- | --- | --- |
| `<PROJECT_ID>` | yes | Project ID |
| `<ENDPOINT>` | yes | API endpoint |

| Option | Value | Description |
| --- | --- | --- |
| `--apply` | flag | Confirm the mutating request should be sent |

