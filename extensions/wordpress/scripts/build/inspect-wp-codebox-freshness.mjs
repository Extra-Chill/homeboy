#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import { extname } from 'node:path';

const options = parseArgs(process.argv.slice(2));
const expected = {
  mode: options.mode,
  version: options.mode === 'release' ? options.expectedVersion || undefined : undefined,
  dist_sha256: options.mode === 'release' ? options.expectedDist || undefined : undefined,
  ref: options.mode === 'source' ? options.expectedRef || undefined : undefined,
  commit: options.mode === 'source' ? options.expectedCommit || undefined : undefined,
};

const result = inspect();
process.stdout.write(`${JSON.stringify(result)}\n`);
process.exit(result.fresh ? 0 : 1);

function inspect() {
  const command = ['.js', '.cjs', '.mjs'].includes(extname(options.bin))
    ? [process.execPath, options.bin]
    : [options.bin];
  const doctor = spawnSync(command[0], [...command.slice(1), 'doctor', '--json'], {
    encoding: 'utf8',
    env: process.env,
    maxBuffer: 10 * 1024 * 1024,
  });

  if (doctor.error) {
    return rejected('doctor_unavailable', {}, doctor.error.message);
  }

  let payload;
  try {
    payload = JSON.parse(doctor.stdout);
  } catch {
    return rejected('doctor_json_malformed', {}, bounded(doctor.stderr || doctor.stdout));
  }

  if (payload?.schema !== 'wp-codebox/doctor/v1' || !Array.isArray(payload.checks)) {
    return rejected('doctor_schema_mismatch', {}, `observed schema ${payload?.schema || 'unavailable'}`);
  }

  const source = payload.checks.find((check) => check?.id === 'wp-codebox.source');
  if (!source) {
    return rejected('provenance_check_missing');
  }

  const provenance = source.details?.provenance;
  const observed = {
    doctor_status: payload.status,
    source_status: source.status,
    source_message: source.message,
    provenance_schema: provenance?.schema,
    version: provenance?.package?.version,
    dist_sha256: provenance?.dist?.sha256,
    ref: provenance?.git?.ref,
    commit: provenance?.git?.commit,
    evidence: source.details?.git?.evidence,
    upstream: source.details?.git?.upstream,
    ahead: source.details?.git?.ahead,
    behind: source.details?.git?.behind,
  };

  if (source.status !== 'ok') {
    return rejected(source.status === 'warning' ? 'provenance_unavailable' : 'provenance_invalid', observed, source.message);
  }
  if (provenance?.schema !== 'wp-codebox/cli-build-provenance/v1') {
    return rejected('provenance_schema_mismatch', observed);
  }
  if (!observed.version || !observed.dist_sha256) {
    return rejected('provenance_identity_incomplete', observed);
  }

  if (options.record) {
    return accepted(observed);
  }

  if (options.mode === 'release') {
    if (!expected.version || !expected.dist_sha256) {
      return rejected('expected_release_identity_unavailable', observed);
    }
    if (observed.version !== expected.version || observed.dist_sha256 !== expected.dist_sha256) {
      return rejected('release_identity_mismatch', observed);
    }
    return accepted(observed);
  }

  if (!expected.ref) {
    return rejected('expected_source_ref_unavailable', observed);
  }
  if (observed.ref !== expected.ref) {
    return rejected('source_ref_mismatch', observed);
  }
  if (expected.commit && observed.commit !== expected.commit) {
    return rejected('source_commit_mismatch', observed);
  }
  if (options.candidate !== 'managed' && !expected.commit) {
    return rejected('source_authority_unprovable', observed, 'an external source candidate requires an exact expected commit');
  }
  return accepted(observed);
}

function accepted(observed) {
  return { schema: 'homeboy-wordpress/wp-codebox-freshness/v1', fresh: true, candidate: options.candidate, bin: options.bin, expected, observed };
}

function rejected(reason, observed = {}, detail = '') {
  return { schema: 'homeboy-wordpress/wp-codebox-freshness/v1', fresh: false, reason, detail, candidate: options.candidate, bin: options.bin, expected, observed };
}

function bounded(value) {
  return String(value || '').trim().slice(0, 500);
}

function parseArgs(args) {
  const parsed = { candidate: 'ambient', mode: '', bin: '', record: false };
  for (let index = 0; index < args.length; index++) {
    const arg = args[index];
    if (arg === '--record') {
      parsed.record = true;
      continue;
    }
    const key = {
      '--bin': 'bin',
      '--mode': 'mode',
      '--candidate': 'candidate',
      '--expected-version': 'expectedVersion',
      '--expected-dist': 'expectedDist',
      '--expected-ref': 'expectedRef',
      '--expected-commit': 'expectedCommit',
    }[arg];
    if (!key || !args[index + 1]) throw new Error(`Invalid argument: ${arg}`);
    parsed[key] = args[++index];
  }
  if (!parsed.bin || !['release', 'source'].includes(parsed.mode) || !['managed', 'ambient', 'override'].includes(parsed.candidate)) {
    throw new Error('--bin, --mode, and a valid --candidate are required');
  }
  return parsed;
}
