const requestSchema = 'homeboy/notification-route-resolver-request/v1';
const responseSchema = 'homeboy/notification-route-resolver/v1';
const transport = 'discord.run-completion';

let input = '';
for await (const chunk of process.stdin) input += chunk;

let request;
try {
  request = JSON.parse(input);
} catch {
  fail('Invalid notification route resolver request');
}

if (process.exitCode === undefined && !isValidRequest(request)) {
  fail('Invalid notification route resolver request');
}

if (process.exitCode === undefined) {
  const threadId = process.env.KIMAKI_THREAD_ID;
  if (!threadId) {
    emit({ schema: responseSchema, status: 'unmatched' });
  } else if (!/^\d{17,20}$/.test(threadId)) {
    fail('Invalid Discord thread attribution');
  } else {
    emit({
      schema: responseSchema,
      status: 'matched',
      route: `discord:v1:thread:${threadId}`,
    });
  }
}

function isValidRequest(value) {
  return (
    value !== null &&
    typeof value === 'object' &&
    !Array.isArray(value) &&
    Object.keys(value).sort().join(',') === 'schema,transport' &&
    value.schema === requestSchema &&
    value.transport === transport
  );
}

function emit(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

function fail(message) {
  process.stderr.write(`${message}\n`);
  process.exitCode = 2;
}
