# WP Codebox full-suite failures replay fixture

Trimmed from the `extrachill-users` job of Extra-Chill/extrachill run
44062955343 (Extra-Chill/homeboy#18516):

- `test-results.json` is the WP Codebox `wp-codebox/test-results/v1` payload
  (17 failed of 641) without the references the Homeboy adapter adds.
- `phpunit-output.log` is the head of the preserved PHPUnit command log: the
  command banner, progress, the 1 error and 16 failure sections and the
  summary line. The thousands of trailing database-error stderr lines are
  dropped.

Used by `scripts/test/wp-codebox-full-suite-failures-replay-smoke.sh`.
