// Replays one persisted case from the generic case env, with the same parser
// the fuzz workload exercises: exits 1 while the case still reproduces.
import { existsSync, readFileSync } from 'node:fs';

const mode = existsSync('mode') ? readFileSync('mode', 'utf8').trim() : 'fail';
const { input } = JSON.parse(readFileSync(process.env.HOMEBOY_WORKLOAD_CASE_ARTIFACT_FILE, 'utf8'));
console.log(
  JSON.stringify({
    mode: process.env.HOMEBOY_WORKLOAD_CASE_MODE,
    case_id: process.env.HOMEBOY_WORKLOAD_CASE_ID,
    seed: process.env.HOMEBOY_WORKLOAD_SEED,
    source_run: process.env.HOMEBOY_WORKLOAD_RUN_ID,
    input,
  }),
);
if (input === '' && mode === 'fail') {
  console.log('replay: reproduced unexpected end of input');
  process.exit(1);
}
JSON.parse(input === '' ? 'null' : input);
console.log('replay: case passes');
