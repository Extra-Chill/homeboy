import fs from 'node:fs';

export class ProviderError extends Error {
  constructor(type, message, details = {}) { super(message); this.type = type; this.details = details; }
}
export function verifyRun({ directory, nonce, startedAt, artifacts = [] }) {
  const fail = (type, message) => { throw new ProviderError(type, message); };
  const output = `${directory}/output.txt`;
  if (!fs.existsSync(output)) fail('missing_output', 'Run output does not exist');
  const metadataPath = `${directory}/result.json`;
  if (!fs.existsSync(metadataPath)) fail('missing_result', 'Run result does not exist');
  let metadata;
  try { metadata = JSON.parse(fs.readFileSync(metadataPath, 'utf8')); }
  catch { fail('invalid_result', 'Run result is not valid JSON'); }
  // Use guest timestamps: host and guest clocks need not agree. Nonce binds the
  // guest start marker and creation time to this invocation, not an old transfer.
  if (metadata.nonce !== nonce) fail('nonce_mismatch', 'Result belongs to another run');
  const start = Date.parse(metadata.startedAt);
  const created = Date.parse(metadata.outputCreatedAt);
  if (!Number.isFinite(start) || !Number.isFinite(created) || created < start || metadata.requestStartedAt !== startedAt)
    fail('stale_output', 'Output predates this run or has invalid timestamps');
  const text = fs.readFileSync(output, 'utf8').replace(/^\uFEFF/, '');
  const trailer = text.match(/(?:^|\r?\n)EXIT:(-?\d+) NONCE:([^\r\n]+)\r?\n?$/);
  if (!trailer) fail('missing_trailer', 'Output has no final EXIT/nonce trailer');
  if (trailer[2] !== nonce) fail('nonce_mismatch', 'Output belongs to another run');
  for (const artifact of artifacts) {
    if (!fs.existsSync(artifact.local)) fail('missing_artifact', `Missing artifact: ${artifact.remote}`);
  }
  const exitCode = Number(trailer[1]);
  if (exitCode !== 0) throw new ProviderError('command_failed', `Command exited ${exitCode}`, { exitCode });
  return { exitCode, output };
}
