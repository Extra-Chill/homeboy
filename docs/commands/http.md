# http

Make generic HTTP requests to full URLs.

Use this when a one-off internal or external endpoint needs Homeboy's local transport affordances without creating a project API config.

## Examples

```sh
homeboy auth profile set-basic matticspace --username chubes4
homeboy http get https://logstash.a8c.com/logstash/... --proxy socks5://127.0.0.1:8080 --auth-profile matticspace
```

```sh
homeboy http request POST https://example.com/api --json '{"ok":true}' --header 'X-Example: value'
```

## Options

- `--proxy <url>` routes the request through an explicit proxy URL.
- `--auth-profile <name>` adds an Authorization header from `homeboy auth profile` keychain storage.
- `--header 'Name: value'` adds a request header.
- `--json <json>` sends a JSON body.
- `--form key=value` sends form fields.

Output is structured JSON with `method`, `url`, `status`, `headers`, and `body`.
