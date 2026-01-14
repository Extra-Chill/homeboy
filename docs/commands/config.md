# `homeboy config`

This command group is not implemented in the current CLI.

The CLI currently stores configuration under the OS config directory (`dirs::config_dir()/homeboy/`) using per-entity JSON files.

Configured entities:

- `projects/<id>.json`
- `servers/<id>.json`
- `components/<id>.json`
- `modules/<moduleId>/<moduleId>.json` (module manifest)

(There is no separate global `homeboy.json` file in the current CLI implementation.)

