# Fake Agent Runtime Fixture

This experimental fixture is a tiny local shell-style provider for the generic
Homeboy agent runtime contract.

It accepts one `homeboy/agent-task-request/v1` JSON object on stdin, validates an
explicit `executor.backend` value of `fake-runtime`, writes
`.homeboy/fake-runtime/outcome.json` and `.homeboy/fake-runtime/transcript.log`,
then emits one `homeboy/agent-task-outcome/v1` JSON object on stdout.

The fixture has no secret inputs. Its manifest declares generic workspace and
publication tool preset capabilities so contract tests can verify preset
expansion without relying on a domain-specific runtime.
