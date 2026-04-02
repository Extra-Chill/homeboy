# Go Extension

Adds first-class Go support to Homeboy for services, bridges, and command-line tools.

## Includes

- `go` CLI integration
- default build support
- `go fmt` + `go vet` linting
- `go test ./...` test runner
- basic symbol fingerprinting for audit workflows

## Build behavior

- If a local `build.sh` exists, Homeboy uses it.
- Otherwise:
  - builds each directory under `./cmd/*` into `./bin/*`
  - falls back to `go build -o ./bin/<repo-name> .`

## Typical usage

```bash
homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id go
homeboy build <project-or-component>
homeboy test <project-or-component>
homeboy lint <project-or-component>
```
