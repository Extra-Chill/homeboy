/**
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const extension = path.resolve(import.meta.dirname, '..');
const runner = path.join(extension, 'scripts/test/test-runner-wp-codebox.sh');
const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-database-service-'));
const component = path.join(root, 'plugin');
const dependency = path.join(root, 'dependency');
const cli = path.join(root, 'wp-codebox');
await mkdir(component, { recursive: true });
await mkdir(dependency, { recursive: true });
await writeFile(path.join(component, 'plugin.php'), '<?php\n/**\n * Plugin Name: Provider Test\n */\n');
await writeFile(path.join(dependency, 'composer.json'), '{}\n');
await writeFile(cli, `#!/usr/bin/env node
import { appendFile, mkdir, readFile, writeFile } from 'node:fs/promises';
const args = process.argv.slice(2);
if (args.includes('--version')) { process.stdout.write('0.21.0'); process.exit(0); }
if (args[0] === 'runtime' && args[1] === 'descriptor') {
  const capabilities = process.env.OMIT_NATIVE_DATABASE_CAPABILITY === '1' ? [] : ['runtime-service:mysql:native:mariadb'];
  process.stdout.write(JSON.stringify({
    schema: 'wp-codebox/runtime-descriptor/v1',
    capabilities,
    contractManifest: {
      capabilities: {
        runtimeServices: { schema: 'wp-codebox/runtime-service-capabilities/v1', capabilities },
      },
    },
  }));
  process.exit(0);
}
if (args[0] === 'recipe' && args[1] === 'build') {
  const options = JSON.parse(await readFile(args[args.indexOf('--options') + 1], 'utf8'));
  const configuration = options.services?.[0]?.configuration || {};
  const adminNames = [configuration.hostEnv, configuration.portEnv, configuration.usernameEnv, configuration.passwordEnv].filter(Boolean);
  const adminEnvironment = Object.fromEntries(adminNames.map((name) => [name, Object.hasOwn(process.env, name)]));
  await appendFile(process.env.OBSERVED, JSON.stringify({ phase: 'build', options, adminEnvironment }) + '\\n');
  for (const name of adminNames) {
    if (process.env[name] !== undefined) {
      process.stdout.write('build stdout ' + process.env[name] + '\\n');
      process.stderr.write('build stderr ' + process.env[name] + '\\n');
    }
  }
  if (options.services?.[0]?.configuration?.provider === 'unregistered') {
    process.stderr.write('Unsupported managed runtime service provider: unregistered\\n');
    process.exit(2);
  }
  const recipe = { schema: 'wp-codebox/workspace-recipe/v1', inputs: { services: options.services } };
  await writeFile(args[args.indexOf('--output') + 1], JSON.stringify(recipe));
  process.exit(0);
}
if (args[0] === 'recipe-run') {
  const recipe = JSON.parse(await readFile(args[args.indexOf('--recipe') + 1], 'utf8'));
  const artifacts = args[args.indexOf('--artifacts') + 1];
  await appendFile(process.env.OBSERVED, JSON.stringify({ phase: 'run', recipe, args }) + '\\n');
  const configuration = recipe.inputs.services?.[0]?.configuration || {};
  for (const name of [configuration.hostEnv, configuration.portEnv, configuration.usernameEnv, configuration.passwordEnv].filter(Boolean)) {
    process.stdout.write('provider stdout ' + process.env[name] + '\\n');
    process.stderr.write('provider stderr ' + process.env[name] + '\\n');
  }
  await mkdir(artifacts + '/runtime-fixture/files', { recursive: true });
  await writeFile(artifacts + '/latest-runtime.json', JSON.stringify({ paths: { runtimeDirectory: 'runtime-fixture' } }));
  await writeFile(artifacts + '/runtime-fixture/files/test-results.json', JSON.stringify({ schema: 'wp-codebox/test-results/v1', status: 'passed', summary: { total: 1, passed: 1, failed: 0, skipped: 0 }, suites: [], rawLogReferences: [] }));
}
`);
await chmod(cli, 0o755);

let scenario = 0;
function invoke(settings, env = {}) {
  return invokeSettingsJson(JSON.stringify(settings), env);
}

function invokeSettingsJson(settingsJson, env = {}) {
  scenario += 1;
  const observed = path.join(root, `observed-${scenario}.jsonl`);
  const artifacts = path.join(root, `artifacts-${scenario}`);
  const result = spawnSync(runner, [], {
    env: {
      ...process.env,
      HOMEBOY_COMPONENT_PATH: component,
      COMPONENT_ID: 'provider-test',
      HOMEBOY_WP_CODEBOX_BIN: cli,
      HOMEBOY_SETTINGS_JSON: settingsJson,
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: artifacts,
      OBSERVED: observed,
      ...env,
    },
    encoding: 'utf8',
  });
  return { result, observed, artifacts };
}

async function observations(file) {
  try {
    return (await readFile(file, 'utf8')).trim().split('\n').filter(Boolean).map(JSON.parse);
  } catch {
    return [];
  }
}

async function retainedRuntimeLog(invocation, file) {
  const runDirectory = (await readdir(invocation.artifacts)).find((entry) => entry.startsWith('wp-codebox-phpunit.'));
  return readFile(path.join(invocation.artifacts, runDirectory, 'runtime-fixture', 'logs', file), 'utf8');
}

function expectPreflightFailure(settings, pattern, env = {}) {
  const invocation = invoke(settings, env);
  assert.notEqual(invocation.result.status, 0);
  assert.match(invocation.result.stderr, pattern);
  return invocation;
}

try {
  const defaultInvocation = invoke({ database_type: 'mysql', wp_codebox_multisite: true });
  assert.equal(defaultInvocation.result.status, 0, defaultInvocation.result.stderr);
  const defaultBuild = (await observations(defaultInvocation.observed))[0].options;
  assert.equal('services' in defaultBuild, false, 'omitted provider preserves WP Codebox service defaults');
  assert.equal('secretEnv' in defaultBuild, false, 'omitted provider adds no secret declarations');
  assert.equal(defaultBuild.databaseType, 'mysql');
  assert.equal(defaultBuild.multisite, true);

  const inheritedSecret = 'must-not-enter-native-recipe';
  const nativeInvocation = invoke({
    database_type: 'mysql',
    wp_codebox_multisite: true,
    wp_codebox_database_service: { provider: 'native', engine: 'mariadb' },
  }, { PROVIDER_ADMIN_PASSWORD: inheritedSecret });
  assert.equal(nativeInvocation.result.status, 0, nativeInvocation.result.stderr);
  const nativeObservations = await observations(nativeInvocation.observed);
  assert.deepEqual(nativeObservations[0].options.services, [{
    id: 'wordpress-database',
    kind: 'mysql',
    configuration: { provider: 'native', engine: 'mariadb' },
    outputs: { host: 'DB_HOST', port: 'DB_PORT', username: 'DB_USER', password: 'DB_PASSWORD', database: 'DB_NAME' },
  }]);
  assert.equal('secretEnv' in nativeObservations[0].options, false, 'native mode inherits no caller secret declaration');
  assert.equal(JSON.stringify(nativeObservations).includes(inheritedSecret), false, 'native recipe translation contains no ambient secret values');
  assert.equal(nativeObservations[1].args.includes('--approve-external-service-writes'), false, 'native mode requires no external-write approval');
  assert.equal(nativeObservations[1].args.includes('--policy'), false, 'native mode preserves the WP Codebox default network policy');
  assert.equal('externalServices' in nativeObservations[1].recipe.inputs, false, 'native mode adds no external-service boundary');

  const missingNativeCapability = expectPreflightFailure({
    database_type: 'mysql',
    wp_codebox_database_service: { provider: 'native', engine: 'mariadb' },
  }, /does not advertise the required native MariaDB service capability/, { OMIT_NATIVE_DATABASE_CAPABILITY: '1' });
  assert.deepEqual(await observations(missingNativeCapability.observed), [], 'missing upstream capability fails before recipe build');

  for (const engine of [undefined, 'mysql', 'unsupported', 42]) {
    const rejectedEngine = expectPreflightFailure({
      database_type: 'mysql',
      wp_codebox_database_service: { provider: 'native', ...(engine === undefined ? {} : { engine }) },
    }, /native provider requires engine=mariadb/);
    assert.deepEqual(await observations(rejectedEngine.observed), [], 'unsupported native engines fail before recipe build');
    if (engine !== undefined) {
      assert.equal(rejectedEngine.result.stderr.includes(String(engine)), false, 'native engine diagnostics omit rejected values');
    }
  }

  for (const field of ['secret_env', 'allowed_hosts', 'host', 'port', 'username', 'password', 'socket', 'config', 'defaults', 'datadir', 'pid_path', 'log_path', 'externalService', 'args']) {
    const forbiddenField = expectPreflightFailure({
      database_type: 'mysql',
      wp_codebox_database_service: { provider: 'native', engine: 'mariadb', [field]: 'forbidden-native-value' },
    }, /contains unsupported fields/);
    assert.deepEqual(await observations(forbiddenField.observed), [], 'native connection, administration, path, and process fields fail before recipe build');
    assert.equal(forbiddenField.result.stderr.includes('forbidden-native-value'), false, 'native field diagnostics omit rejected values');
  }

  const nativeDatabaseMismatch = expectPreflightFailure({
    database_type: 'sqlite',
    wp_codebox_database_service: { provider: 'native', engine: 'mariadb' },
  }, /requires database_type=mysql/);
  assert.deepEqual(await observations(nativeDatabaseMismatch.observed), [], 'native database type mismatch fails before recipe build');

  const secretValues = {
    PROVIDER_ADMIN_HOST: 'database.internal.example',
    PROVIDER_ADMIN_PORT: '3306',
    PROVIDER_ADMIN_USER: 'temporary-database-admin',
    PROVIDER_ADMIN_PASSWORD: 'do-not-persist-this-password',
  };
  const configured = {
    database_type: 'mysql',
    wp_codebox_multisite: true,
    validation_dependencies: [dependency],
    wp_codebox_database_service: {
      provider: 'external',
      allowed_hosts: ['database.internal.example:3306'],
      secret_env: {
        host: 'PROVIDER_ADMIN_HOST',
        port: 'PROVIDER_ADMIN_PORT',
        username: 'PROVIDER_ADMIN_USER',
        password: 'PROVIDER_ADMIN_PASSWORD',
      },
    },
  };
  const externalInvocation = invoke(configured, secretValues);
  assert.equal(externalInvocation.result.status, 0, externalInvocation.result.stderr);
  const externalObservations = await observations(externalInvocation.observed);
  const options = externalObservations[0].options;
  assert.deepEqual(externalObservations[0].adminEnvironment, {
    PROVIDER_ADMIN_HOST: false,
    PROVIDER_ADMIN_PORT: false,
    PROVIDER_ADMIN_USER: false,
    PROVIDER_ADMIN_PASSWORD: false,
  }, 'recipe build receives administrative names without host values');
  assert.equal(options.databaseType, 'mysql');
  assert.equal(options.multisite, true, 'external MySQL remains compatible with multisite PHPUnit');
  // The recipe declares that a validation dependency may need Composer
  // preparation; WP Codebox owns detecting whether it does and running it. This
  // runner never inspects vendor state, so the declaration is unconditional.
  assert.equal(options.extra_plugins[0].composer, 'install', 'validation dependencies declare Composer preparation for the substrate to own');
  assert.equal('composer' in options.extra_plugins.at(-1), false, 'the component under review carries no Composer preparation instruction');
  assert.equal('secretEnv' in options, false, 'administrative credentials are not forwarded into the sandbox runtime');
  assert.deepEqual(options.services, [{
    id: 'wordpress-database',
    kind: 'mysql',
    configuration: {
      provider: 'external',
      externalService: 'wordpress-database-administration',
      hostEnv: 'PROVIDER_ADMIN_HOST',
      portEnv: 'PROVIDER_ADMIN_PORT',
      usernameEnv: 'PROVIDER_ADMIN_USER',
      passwordEnv: 'PROVIDER_ADMIN_PASSWORD',
    },
    outputs: { host: 'DB_HOST', port: 'DB_PORT', username: 'DB_USER', password: 'DB_PASSWORD', database: 'DB_NAME' },
  }]);
  const serializedOptions = JSON.stringify(options);
  const serializedRecipe = JSON.stringify(externalObservations[1].recipe);
  for (const value of Object.values(secretValues)) {
    assert.equal(serializedOptions.includes(value), false, 'builder options omit provider credential values');
    assert.equal(externalInvocation.result.stdout.includes(value), false, 'Homeboy stdout omits provider credential values');
    assert.equal(externalInvocation.result.stderr.includes(value), false, 'Homeboy stderr omits provider credential values');
  }
  for (const value of [secretValues.PROVIDER_ADMIN_USER, secretValues.PROVIDER_ADMIN_PASSWORD]) {
    assert.equal(serializedRecipe.includes(value), false, 'generated recipe omits provider credential values');
  }
  assert.deepEqual(externalObservations[1].recipe.inputs.externalServices, [{
    id: 'wordpress-database-administration',
    environment: 'external',
    allowedHosts: ['database.internal.example:3306'],
    writes: 'allowed-with-approval',
  }]);
  assert.equal(externalObservations[1].args.includes('--approve-external-service-writes'), true);
  const policy = JSON.parse(externalObservations[1].args[externalObservations[1].args.indexOf('--policy') + 1]);
  assert.deepEqual(policy.network, { allowHosts: ['database.internal.example:3306'] });
  assert.equal(policy.approvals, 'on-write');
  assert.match(await retainedRuntimeLog(externalInvocation, 'recipe-run.stdout.log'), /provider stdout \[REDACTED\]/);
  assert.match(await retainedRuntimeLog(externalInvocation, 'recipe-run.stderr.log'), /provider stderr \[REDACTED\]/);

  const prepareInvocation = invoke({
    ...configured,
    wp_codebox_prepare_steps: [{
      command: process.execPath,
      args: ['-e', "process.stdout.write(process.env.PROVIDER_ADMIN_PASSWORD || 'prepare-admin-env-absent')"],
    }],
  }, secretValues);
  assert.equal(prepareInvocation.result.status, 0, prepareInvocation.result.stderr);
  assert.match(prepareInvocation.result.stdout, /prepare-admin-env-absent/);
  assert.equal(prepareInvocation.result.stdout.includes(secretValues.PROVIDER_ADMIN_PASSWORD), false, 'prepare steps cannot observe database administration values');

  const mariaDbInvocation = invoke({
    ...configured,
    wp_codebox_database_service: { ...configured.wp_codebox_database_service, engine: 'mariadb' },
  }, secretValues);
  assert.equal(mariaDbInvocation.result.status, 0, mariaDbInvocation.result.stderr);
  const mariaDbOptions = (await observations(mariaDbInvocation.observed))[0].options;
  assert.equal(mariaDbOptions.services[0].configuration.engine, 'mariadb', 'MariaDB selects WP Codebox MariaDB client semantics');

  const malformedSettings = invokeSettingsJson('{"database_type":', secretValues);
  assert.notEqual(malformedSettings.result.status, 0);
  assert.match(malformedSettings.result.stderr, /HOMEBOY_SETTINGS_JSON must contain a valid JSON object/);
  assert.equal(malformedSettings.result.stderr.includes('{"database_type":'), false, 'malformed settings diagnostics omit input values');
  assert.deepEqual(await observations(malformedSettings.observed), [], 'malformed settings fail before recipe build');

  const collision = expectPreflightFailure({
    ...configured,
    bench_env: { PROVIDER_ADMIN_HOST: 'serialized-administrative-host' },
  }, /bench_env must not expose database service administration environment: PROVIDER_ADMIN_HOST/, secretValues);
  assert.deepEqual(await observations(collision.observed), [], 'bench_env collisions fail before options are serialized');
  for (const value of [...Object.values(secretValues), 'serialized-administrative-host']) {
    assert.equal(collision.result.stdout.includes(value), false);
    assert.equal(collision.result.stderr.includes(value), false);
  }

  const missingProvider = expectPreflightFailure({
    database_type: 'mysql',
    wp_codebox_database_service: { secret_env: { host: 'PROVIDER_ADMIN_HOST', username: 'PROVIDER_ADMIN_USER', password: 'PROVIDER_ADMIN_PASSWORD' } },
  }, /provider must be external or native/, secretValues);
  assert.deepEqual(await observations(missingProvider.observed), [], 'missing provider fails before WP Codebox execution');

  for (const provider of ['docker', 'unregistered', 42]) {
    const rejectedProvider = expectPreflightFailure({
      ...configured,
      wp_codebox_database_service: { ...configured.wp_codebox_database_service, provider },
    }, /provider must be external or native/, secretValues);
    assert.deepEqual(await observations(rejectedProvider.observed), [], 'unsupported providers fail before recipe build');
    // Check the diagnostic text, not the stack frames. Frames carry source line
    // numbers, and a numeric provider such as 42 is a substring of a line number
    // like 428 — that is a false positive about line numbering, not a leak of
    // the rejected value.
    const rejectedDiagnostics = rejectedProvider.result.stderr
      .split('\n')
      .filter((line) => !/^\s*at\s/.test(line) && !/^\s*(file:)?\/\S*:\d+$/.test(line))
      .join('\n');
    assert.equal(rejectedDiagnostics.includes(String(provider)), false, 'provider diagnostics omit rejected values');
  }

  expectPreflightFailure({
    database_type: 'mysql',
    wp_codebox_database_service: { provider: 'external', secret_env: configured.wp_codebox_database_service.secret_env },
  }, /allowed_hosts must contain hostnames with optional ports/, secretValues);
  expectPreflightFailure({
    database_type: 'mysql',
    wp_codebox_database_service: { provider: 'external', allowed_hosts: ['database.internal.example'] },
  }, /secret_env must contain provider secret environment references/);
  expectPreflightFailure({
    database_type: 'mysql',
    wp_codebox_database_service: { provider: 'external', allowed_hosts: ['database.internal.example'], secret_env: { host: 'PROVIDER_ADMIN_HOST' } },
  }, /missing required references: username, password/, secretValues);
  expectPreflightFailure({
    database_type: 'mysql',
    wp_codebox_database_service: { provider: 'external', allowed_hosts: ['database.internal.example'], secret_env: { host: 'MISSING_PROVIDER_HOST', username: 'PROVIDER_ADMIN_USER', password: 'PROVIDER_ADMIN_PASSWORD' } },
  }, /secret environment variable is unavailable: MISSING_PROVIDER_HOST.*homeboy init.*homeboy extension show wordpress.*readlink/, secretValues);
  expectPreflightFailure({
    database_type: 'mysql',
    wp_codebox_database_service: { provider: 'external', allowed_hosts: ['database.internal.example'], secret_env: ['PROVIDER_ADMIN_HOST'] },
  }, /secret_env must contain provider secret environment references/);
  expectPreflightFailure({
    ...configured,
    wp_codebox_database_service: { ...configured.wp_codebox_database_service, engine: 'unsupported' },
  }, /engine must be mysql or mariadb/, secretValues);
  expectPreflightFailure({
    database_type: 'mysql',
    wp_codebox_database_service: { provider: 'external', secret_env: { host: 'PROVIDER_ADMIN_HOST', username: 'PROVIDER_ADMIN_USER', password: 'PROVIDER_ADMIN_PASSWORD' }, password: 'plaintext' },
  }, /unsupported fields: password/, secretValues);
  expectPreflightFailure({
    database_type: 'sqlite',
    wp_codebox_database_service: { provider: 'external', secret_env: { host: 'PROVIDER_ADMIN_HOST', username: 'PROVIDER_ADMIN_USER', password: 'PROVIDER_ADMIN_PASSWORD' } },
  }, /requires database_type=mysql/, secretValues);

} finally {
  await rm(root, { recursive: true, force: true });
}

console.log('WP Codebox database service smoke passed.');
