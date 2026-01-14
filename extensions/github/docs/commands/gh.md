# gh

Run GitHub CLI commands against a component's repository.

## Usage

```bash
homeboy gh <component> <gh-command> [args...]
```

## Examples

```bash
# List issues
homeboy gh artist-platform issue list

# Create an issue
homeboy gh artist-platform issue create --title "Bug in auth" --body "Details..."

# View a specific issue
homeboy gh artist-platform issue view 42

# List pull requests
homeboy gh artist-platform pr list

# Check PR status
homeboy gh artist-platform pr status

# View repo info
homeboy gh artist-platform repo view
```

## Requirements

- GitHub CLI (`gh`) must be installed and authenticated
- Component must have a git repository with a GitHub remote
