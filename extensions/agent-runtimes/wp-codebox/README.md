# WP Codebox Agent Runtime

This package is the first-class WP Codebox agent-task runtime staging area used
by Homeboy. For this behavior-preserving extraction step, it delegates to the
legacy WordPress executor implementation while preserving the existing
`wordpress/lib/codebox-agent-task-executor.js` export and
`wordpress/scripts/agent/homeboy-codebox-agent-task-executor.cjs` command path.

Pinned WordPress callers can keep using the existing WordPress extension paths.
New runtime consumers can import this package or invoke the runtime-local CLI.
