#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const { validateControllerLoopProof } = require('../lib/controller-loop-proof-validator');

try {
  const args = parseArgs(process.argv.slice(2));
  const specPath = args.spec || args.config || '';
  const proofPath = args.proof || args.run || args.result || '';
  if (!specPath || !proofPath) {
    throw new Error('Pass --spec <path> and --proof <path>.');
  }
  const spec = readJson(specPath);
  const proof = readJson(proofPath);
  const policy = args.policy ? readJson(args.policy) : {};
  const report = validateControllerLoopProof({ spec, proof, policy });
  const json = `${JSON.stringify(report, null, 2)}\n`;
  if (args.output) {
    fs.writeFileSync(args.output, json);
  }
  process.stdout.write(json);
  process.exitCode = report.valid ? 0 : 1;
} catch (error) {
  process.stderr.write(`${error && error.message ? error.message : String(error)}\n`);
  process.exitCode = 1;
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function parseArgs(argv) {
  const args = {};
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!arg.startsWith('--')) {
      continue;
    }
    const key = arg.slice(2).replace(/-/g, '_');
    args[key] = argv[index + 1];
    index += 1;
  }
  return args;
}
