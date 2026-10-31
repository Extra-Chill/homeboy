#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { writeGithubOutput } = require('./lib/common.cjs');

function main() {
  const command = process.argv[2];
  if (command === 'resolve-transcript') {
    resolveTranscript();
    return;
  }
  if (command === 'prepare-pr-comment') {
    preparePrComment();
    return;
  }
  throw new Error(`Unknown artifacts/comments command: ${command || ''}`);
}

function resolveTranscript() {
  const transcriptJson = process.env.TRANSCRIPT_JSON || '';
  const transcriptHostDir = process.env.TRANSCRIPT_HOST_DIR || '';
  let transcriptPath = '';
  if (transcriptJson && fs.existsSync(transcriptJson) && fs.statSync(transcriptJson).isFile()) {
    transcriptPath = transcriptJson;
  } else if (transcriptJson && transcriptHostDir) {
    const candidate = path.join(transcriptHostDir, path.basename(transcriptJson));
    if (fs.existsSync(candidate) && fs.statSync(candidate).isFile()) {
      transcriptPath = candidate;
    }
  }
  if (!transcriptPath) {
    process.stdout.write(`Transcript artifact file not found for path: ${transcriptJson}\n`);
  }
  writeGithubOutput({ path: transcriptPath });
}

function preparePrComment() {
  const bodyFile = path.join(process.env.RUNNER_TEMP, 'runtime-agent-comment.md');
  const lines = [
    '## Runtime Agent Full-Run',
    '',
    `- Agent: \`${process.env.AGENT_SLUG}\``,
    `- Flow: \`${process.env.FLOW_SLUG}\``,
    `- Job status: \`${process.env.JOB_STATUS || 'unknown'}\``,
  ];
  if (process.env.TRANSCRIPT_ARTIFACT_NAME) {
    lines.push(`- Transcript artifact: \`${process.env.TRANSCRIPT_ARTIFACT_NAME}\``);
  }
  if (process.env.ENGINE_DATA_JSON && process.env.ENGINE_DATA_JSON !== '{}') {
    lines.push(`- Engine data: \`${process.env.ENGINE_DATA_JSON}\``);
  }
  fs.writeFileSync(bodyFile, `${lines.join('\n')}\n`);
  writeGithubOutput({ body_file: bodyFile });
}

try {
  main();
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exit(1);
}
