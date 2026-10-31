import crypto from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { boundTextUtf8, redactText } from './redaction.mjs';

const [namespace, testId, suite, file, line, message, failureType, inputFile] = process.argv.slice(2);
const [stdoutExcerpt = '', stderrExcerpt = ''] = (await readFile(inputFile, 'utf8')).split('\0');
const fingerprintInput = [namespace, testId, suite, file, line, message, failureType].join('\0');
const sourceLine = line === '' ? null : Number(line || 0);
const sanitizeExcerpt = (value) => boundTextUtf8(redactText(value));

console.log(JSON.stringify({
    test_id: testId,
    test_name: testId,
    suite: suite || null,
    file: file || null,
    test_file: file || null,
    line: sourceLine,
    message,
    failure_type: failureType || 'test_failure',
    error_type: failureType || 'test_failure',
    fingerprint: crypto.createHash('sha256').update(fingerprintInput).digest('hex'),
    stdout_excerpt: sanitizeExcerpt(stdoutExcerpt),
    stderr_excerpt: sanitizeExcerpt(stderrExcerpt),
    source_file: file || null,
    source_line: sourceLine,
}));
