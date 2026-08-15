#!/usr/bin/env node
'use strict';

/* eslint-disable no-console */

/**
 * External dependencies
 */
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn, spawnSync } = require('node:child_process');

const {
  assertProviderSecretEnvPreflight,
  normalizeStringArray,
  providerGuidance,
  providerPreflightManifest,
} = require('../../lib/provider-preflight-manifest');
const {
  assertProviderCredentialBoundaryNamesOnly,
  providerCredentialRequestFields,
  providerCredentialSecretEnvNames,
} = require('../../lib/provider-credential-boundary');
const {
  WP_CODEBOX_ARTIFACT_RESULT_ENVELOPE_SCHEMA,
  artifactNameFromDeclaration,
  artifactPath,
  normalizeTypedArtifactEntry,
  normalizeTypedArtifacts,
  typedArtifactFileRefs,
} = require('../../lib/codebox-artifact-contract');
const { normalizeRuntimeAgentBundleResult } = require('../../lib/runtime-agent-bundle-result');
const {
  codeboxRunAgentTaskInvocation,
} = require('../../lib/codebox-run-agent-task-contract');
const {
  wpCodeboxProviderPluginPathsFromEnv,
  wpCodeboxResolveCommand,
} = require('../../lib/wp-codebox-adapter-descriptor');
const { preflightWpCodeboxRuntime } = require('../../lib/wp-codebox-runtime-selection');
const {
  RuntimeOverlayProfileError,
  runtimeOverlayProfileReadinessDiagnostics,
  validateRuntimeOverlayProfiles,
} = require('../../lib/runtime-overlay-profiles');


// Diagnostics mode. When enabled, the wp-codebox CLI subprocess stderr is
// streamed (fd-inherited) straight to this process's stderr in real time
// instead of being buffered into structured payloads, so a hard crash or
// timeout inside the sandbox still surfaces its raw output in the job log.
// Raw stdout/stderr files are written for every run regardless of this flag.
function runtimeAgentDebugEnabled(env = process.env) {
  return ['1', 'true', 'yes', 'on'].includes(String(env.HOMEBOY_RUNTIME_AGENT_DEBUG || '').trim().toLowerCase());
}

function safeRuntimeFileSegment(value) {
  return String(value || 'wp-codebox-task').replace(/[^a-zA-Z0-9._-]+/g, '-').replace(/^-+|-+$/g, '') || 'wp-codebox-task';
}

// Persist the wp-codebox CLI's raw stdout/stderr to the runtime-agent run
// directory so they are always retrievable as CI artifacts, independent of
// whether a structured outcome was produced. In debug mode stderr is inherited
// (live in the log) so result.stderr is null and only stdout is written here.
function writeRuntimeRawOutputFiles(result, request) {
  const runDir = process.env.HOMEBOY_RUNTIME_AGENT_RUN_DIR || '';
  if (!runDir) {
    return;
  }
  try {
    fs.mkdirSync(runDir, { recursive: true });
    const taskId = safeRuntimeFileSegment(request?.orchestrator?.agent_task_id || request?.task_id);
    fs.writeFileSync(path.join(runDir, `${taskId}-wp-codebox-runtime-stdout.txt`), String(result.stdout || ''));
    if (result.stderr != null) {
      fs.writeFileSync(path.join(runDir, `${taskId}-wp-codebox-runtime-stderr.txt`), String(result.stderr || ''));
    }
  } catch (error) {
    process.stderr.write(`Failed to persist wp-codebox raw runtime output: ${error && error.message ? error.message : String(error)}\n`);
  }
}

function argValue(name) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : '';
}

function argValues(name) {
  const values = [];
  for (let index = 0; index < process.argv.length; index += 1) {
    if (process.argv[index] === name && process.argv[index + 1]) {
      values.push(process.argv[index + 1]);
    }
  }
  return values;
}

function firstNonEmptyArray(...values) {
  for (const value of values) {
    const normalized = Array.isArray(value) ? value.filter(Boolean) : (value ? [value] : []);
    if (normalized.length > 0) {
      return normalized;
    }
  }
  return [];
}

function hasFlag(name) {
  return process.argv.includes(name);
}

function readStdin() {
  try {
    return fs.readFileSync(0, 'utf8');
  } catch {
    return '';
  }
}

function readTaskRequest() {
  const raw = process.env.HOMEBOY_WP_CODEBOX_TASK_REQUEST || readStdin();
  if (!raw.trim()) {
    throw new Error('Task request JSON is required on stdin or HOMEBOY_WP_CODEBOX_TASK_REQUEST.');
  }
  const request = JSON.parse(raw);
  if (!request || request.schema !== 'wp-codebox/task-input/v1') {
    throw new Error('Task request must use schema wp-codebox/task-input/v1.');
  }
  return request;
}

function labWorkspaceMapping() {
  const raw = process.env.HOMEBOY_LAB_OFFLOAD_JSON;
  if (!raw) {
    return [];
  }
  try {
    const parsed = JSON.parse(raw);
    const workspaces = parsed?.workspace_mapping?.workspaces;
    return Array.isArray(workspaces) ? workspaces : [];
  } catch {
    return [];
  }
}

function remapLabWorkspacePath(candidate, componentSlug) {
  if (!candidate || fs.existsSync(candidate)) {
    return candidate;
  }
  const normalizedCandidate = String(candidate).replace(/\\/g, '/');
  const candidateName = path.basename(normalizedCandidate);
  const expectedName = componentSlug || candidateName;
  const entry = labWorkspaceMapping().find((workspace) => {
    const localName = path.basename(String(workspace?.local_path || '').replace(/\\/g, '/'));
    const remoteName = path.basename(String(workspace?.remote_path || '').replace(/\\/g, '/'));
    return workspace?.remote_path && (localName === expectedName || remoteName === expectedName);
  });
  return entry?.remote_path || candidate;
}

function runtimeComponentSlug(contract) {
  if (!contract || typeof contract !== 'object') {
    return '';
  }
  return typeof contract.slug === 'string' ? contract.slug : '';
}

function remapRuntimeComponentContract(contract) {
  const slug = runtimeComponentSlug(contract);
  if (!slug) {
    return contract;
  }
  const source = contract.path || contract.source || '';
  const remapped = remapLabWorkspacePath(source, slug);
  return remapped && remapped !== source ? { ...contract, path: remapped } : contract;
}

function secretEnvNames(request) {
  return providerCredentialSecretEnvNames(request, request.recipe, { secret_env: argValues('--secret-env') });
}

function requestWithCliSecretEnv(request) {
  return {
    ...request,
    secret_env: secretEnvNames(request),
  };
}

function parseUnixTimestamp(value) {
  const raw = String(value || '').trim();
  if (!raw) {
    return NaN;
  }
  if (/^\d+$/.test(raw)) {
    const parsed = Number.parseInt(raw, 10);
    return parsed > 100000000000 ? Math.floor(parsed / 1000) : parsed;
  }
  const parsedDate = Date.parse(raw);
  return Number.isFinite(parsedDate) ? Math.floor(parsedDate / 1000) : NaN;
}

function codexAuthGuidance() {
  return providerGuidance('codex');
}

async function codexAuthPreflightEnv(request) {
  const manifest = providerPreflightManifest(request.provider);
  if (request.provider !== 'codex' || !manifest) {
    return {};
  }

  assertProviderSecretEnvPreflight(requestWithCliSecretEnv(request), request.provider, process.env);

  if (normalizeStringArray(manifest.validation_hooks).includes('codex-token-expiration')) {
    const expiresAt = parseUnixTimestamp(process.env.AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT);
    if (!Number.isFinite(expiresAt) || expiresAt <= 0) {
      throw new Error(`Codex provider auth preflight failed: AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT is malformed. ${codexAuthGuidance()}`);
    }

    const safetyWindowSeconds = 300;
    const now = Math.floor(Date.now() / 1000);
    if (expiresAt > now + safetyWindowSeconds) {
      return {};
    }
  }

  // HBE validates that the credential contract is present. Refresh belongs to
  // WP Codebox/provider plugins, where provider-specific auth state lives.
  return {};
}

function assertRequiredSecretEnvAvailable(request) {
  const missing = secretEnvNames(request).filter((name) => !process.env[name]);
  if (missing.length > 0) {
    throw new Error(`Required WP Codebox secret environment variable missing: ${missing.join(', ')}`);
  }
}

function mountEntryFromValue(value, metadata) {
  const [source, target, mode = 'readwrite'] = value.split(':');
  if (!source || !target) {
    throw new Error(`Invalid --mount value: ${value}`);
  }
  return {
    source,
    target,
    mode,
    metadata,
  };
}

function mountEntries(request) {
  return [
    ...(request.mounts || []),
    ...argValues('--mount').map((value) => mountEntryFromValue(value, { kind: 'homeboy-audit-fanout' })),
  ];
}

function firstExistingPath(...candidates) {
  return candidates.find((candidate) => candidate && fs.existsSync(candidate)) || '';
}

function siblingPath(workspaceRoot, sibling) {
  return workspaceRoot ? path.join(path.dirname(workspaceRoot), sibling) : '';
}

function workspaceRootFromMounts(mounts) {
  const mountedWorkspace = mounts.find((mount) => mount?.metadata?.kind === 'homeboy-runtime-workspace') || mounts[0];
  return mountedWorkspace?.source || '';
}

function runtimeStackMountEntries(request) {
  return [
    ...(request.runtime_stack_mounts || []),
    ...argValues('--runtime-stack-mount').map((value) => mountEntryFromValue(value, { kind: 'homeboy-runtime-stack' })),
  ];
}

function runtimeOverlayEntries(request) {
  return [
    ...(request.runtime_overlays || []),
    ...argValues('--runtime-overlay-json').map((value) => JSON.parse(value)),
  ];
}

function realPathForContainment(filePath) {
  const resolved = path.resolve(filePath);
  if (fs.existsSync(resolved)) {
    return fs.realpathSync(resolved);
  }
  return path.join(fs.realpathSync(path.dirname(resolved)), path.basename(resolved));
}

function pathInside(parent, candidate) {
  try {
    const parentReal = realPathForContainment(parent);
    const candidateReal = realPathForContainment(candidate);
    const relative = path.relative(parentReal, candidateReal);
    return relative === '' || (Boolean(relative) && !relative.startsWith('..') && !path.isAbsolute(relative));
  } catch {
    return false;
  }
}

function requestTimeoutMs(request) {
  const seconds = Number.parseInt(request.task_timeout_seconds || request.taskTimeoutSeconds || argValue('--task-timeout-seconds'), 10);
  return Number.isFinite(seconds) && seconds > 0 ? seconds * 1000 : undefined;
}

function runtimeVersionFailurePayload(input, artifacts, preflight) {
  return {
    success: false,
    schema: 'wp-codebox/agent-task-run/v1',
    status: 'failed',
    failure_classification: 'execution_failed',
    summary: `WP Codebox ${preflight.selected.path || 'runtime'} does not satisfy required version >=${preflight.required_version}.`,
    artifacts,
    task_input: input,
    diagnostics: [{
      class: `codebox.preflight.${preflight.reason}`,
      message: preflight.remediation,
      data: { candidates: preflight.candidates, selected: preflight.selected, required_version: preflight.required_version },
    }],
    metadata: { phase: 'codebox.preflight', wp_codebox_runtime: preflight },
  };
}

function writeJsonFile(prefix, value) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  const filePath = path.join(directory, 'input.json');
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
  return filePath;
}

function writeTextFile(prefix, fileName, contents) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  const filePath = path.join(directory, fileName);
  fs.writeFileSync(filePath, contents);
  return filePath;
}

function writePreflightEvidence(artifacts, evidence) {
  try {
    fs.mkdirSync(artifacts, { recursive: true });
    const evidencePath = path.join(artifacts, 'homeboy-codebox-task-runner.json');
    fs.writeFileSync(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
    return evidencePath;
  } catch {
    return '';
  }
}

function safePluginSlug(slug, source) {
  const candidate = slug || path.basename(source || 'plugin');
  return candidate.replace(/[^A-Za-z0-9_.-]+/g, '-').replace(/^-+|-+$/g, '') || 'plugin';
}

function preparePluginDirectory(source, slug, preparedRoot, artifacts) {
  if (!source || !path.isAbsolute(source) || !fs.existsSync(source) || !fs.statSync(source).isDirectory()) {
    return { source, prepared: false };
  }

  const safeSlug = safePluginSlug(slug, source);
  if (!fs.existsSync(path.join(source, `${safeSlug}.php`))) {
    return { source, prepared: false };
  }

  const target = path.join(preparedRoot, safeSlug);
  fs.mkdirSync(preparedRoot, { recursive: true });
  if (!pathInside(preparedRoot, target)) {
    throw new Error(`Refusing to prepare plugin outside prepared root: ${target}`);
  }
  if (pathInside(target, source)) {
    return { source, prepared: false };
  }

  fs.rmSync(target, { recursive: true, force: true });
  fs.cpSync(source, target, {
    recursive: true,
    filter: (sourcePath) => !pathInside(artifacts, sourcePath),
  });
  return { source: target, prepared: true, original_source: source, slug: safeSlug };
}

function sourceDepth(source) {
  return String(source || '').split(path.sep).filter(Boolean).length;
}

function nestedPreparedPath(source, preparedItems) {
  for (const item of preparedItems) {
    if (!item.prepared || !item.original_source || item.original_source === source || !pathInside(item.original_source, source)) {
      continue;
    }
    return {
      source: path.join(item.source, path.relative(item.original_source, source)),
      prepared: true,
      original_source: source,
      nested_under: item.original_source,
    };
  }
  return null;
}

function runtimePreparationSources(taskInput) {
  return [
    ...(Array.isArray(taskInput.extra_plugins) ? taskInput.extra_plugins.map((plugin) => [plugin?.source, plugin?.slug]) : []),
    ...(Array.isArray(taskInput.provider_plugin_paths) ? taskInput.provider_plugin_paths.map((source) => [source, pluginSlugFromPath(source)]) : []),
    ...(Array.isArray(taskInput.component_contracts) ? taskInput.component_contracts.map((contract) => [contract?.path || contract?.source, contract?.slug]) : []),
    ...(plainObject(taskInput.runtime_component_paths) ? Object.entries(taskInput.runtime_component_paths).map(([, source]) => [source, '']) : []),
  ]
    .filter(([source]) => source && path.isAbsolute(source))
    .sort(([left], [right]) => sourceDepth(left) - sourceDepth(right));
}

function prepareStableTaskInput(taskInput, artifacts) {
  const preparedRoot = path.join(artifacts, 'prepared-plugins');
  const preparedBySource = new Map();
  const prepare = (source, slug) => {
    if (!source || preparedBySource.has(source)) {
      return preparedBySource.get(source) || { source, prepared: false };
    }
    const prepared = nestedPreparedPath(source, preparedBySource.values()) || preparePluginDirectory(source, slug, preparedRoot, artifacts);
    preparedBySource.set(source, prepared);
    return prepared;
  };

  for (const [source, slug] of runtimePreparationSources(taskInput)) {
    prepare(source, slug);
  }

  const preparedExtraPlugins = Array.isArray(taskInput.extra_plugins)
    ? taskInput.extra_plugins.map((plugin) => {
        const prepared = prepare(plugin?.source, plugin?.slug);
        return prepared.prepared ? { ...plugin, source: prepared.source } : plugin;
      })
    : taskInput.extra_plugins;

  const preparedComponentContracts = Array.isArray(taskInput.component_contracts)
    ? taskInput.component_contracts.map((contract) => {
        const prepared = prepare(contract?.path || contract?.source, contract?.slug);
        return prepared.prepared ? { ...contract, path: prepared.source } : contract;
      })
    : taskInput.component_contracts;

  const runtimeComponentPaths = plainObject(taskInput.runtime_component_paths)
    ? Object.fromEntries(Object.entries(taskInput.runtime_component_paths).map(([key, value]) => {
        const prepared = preparedBySource.get(value);
        return [key, prepared?.prepared ? prepared.source : value];
      }))
    : taskInput.runtime_component_paths;

  const providerPluginPaths = Array.isArray(taskInput.provider_plugin_paths)
    ? taskInput.provider_plugin_paths.map((source) => {
        const prepared = preparedBySource.get(source);
        return prepared?.prepared ? prepared.source : source;
      })
    : taskInput.provider_plugin_paths;

  return {
    input: {
      ...taskInput,
      extra_plugins: preparedExtraPlugins,
      provider_plugin_paths: providerPluginPaths,
      component_contracts: preparedComponentContracts,
      runtime_component_paths: runtimeComponentPaths,
    },
    prepared_plugins: [...preparedBySource.values()].filter((item) => item.prepared),
  };
}

function homeboyPolicyHasParentTools(taskInput) {
  const tools = taskInput?.sandbox_tool_policy?.tools;
  return Array.isArray(tools) && tools.some((tool) => (
    tool?.transport_visibility === 'parent'
      || tool?.execution_location === 'parent'
      || tool?.execution_location === 'control_plane'
  ));
}

function homeboyCallbackDataPluginSource() {
  return `<?php
/**
 * Homeboy runtime callback data helper.
 *
 * Plugin Name: Homeboy Runtime Callback Data
 * Description: Lets runtime code exchange structured callback data with the Homeboy task runner.
 *
 * @package HomeboyCodeboxRuntime
 */

if ( ! function_exists( 'homeboy_callback_data_path' ) ) {
	function homeboy_callback_data_path() {
		$path = getenv( 'HOMEBOY_CALLBACK_DATA_PATH' );
		return is_string( $path ) ? $path : '';
	}
}

if ( ! function_exists( 'homeboy_callback_data_read_all' ) ) {
	function homeboy_callback_data_read_all() {
		$path = homeboy_callback_data_path();
		if ( '' === $path || ! is_readable( $path ) ) {
			return array( 'data' => array(), 'events' => array() );
		}
		$decoded = json_decode( (string) file_get_contents( $path ), true );
		if ( ! is_array( $decoded ) ) {
			return array( 'data' => array(), 'events' => array() );
		}
		return array(
			'data'   => isset( $decoded['data'] ) && is_array( $decoded['data'] ) ? $decoded['data'] : array(),
			'events' => isset( $decoded['events'] ) && is_array( $decoded['events'] ) ? $decoded['events'] : array(),
		);
	}
}

if ( ! function_exists( 'homeboy_callback_data_write_all' ) ) {
	function homeboy_callback_data_write_all( array $payload ) {
		$path = homeboy_callback_data_path();
		if ( '' === $path ) {
			return false;
		}
		$dir = dirname( $path );
		if ( ! is_dir( $dir ) ) {
			wp_mkdir_p( $dir );
		}
		return false !== file_put_contents( $path, wp_json_encode( $payload, JSON_PRETTY_PRINT ) . "\n", LOCK_EX );
	}
}

if ( ! function_exists( 'homeboy_callback_data_get' ) ) {
	function homeboy_callback_data_get( $key = null, $default = null ) {
		$data = homeboy_callback_data_read_all()['data'];
		if ( null === $key || '' === $key ) {
			return $data;
		}
		return array_key_exists( $key, $data ) ? $data[ $key ] : $default;
	}
}

if ( ! function_exists( 'homeboy_callback_data_set' ) ) {
	function homeboy_callback_data_set( $key, $value ) {
		$payload = homeboy_callback_data_read_all();
		$payload['data'][ (string) $key ] = $value;
		return homeboy_callback_data_write_all( $payload );
	}
}

if ( ! function_exists( 'homeboy_callback_data_merge' ) ) {
	function homeboy_callback_data_merge( array $values ) {
		$payload = homeboy_callback_data_read_all();
		$payload['data'] = array_merge( $payload['data'], $values );
		return homeboy_callback_data_write_all( $payload );
	}
}

if ( ! function_exists( 'homeboy_callback_data_append' ) ) {
	function homeboy_callback_data_append( $key, $value ) {
		$payload = homeboy_callback_data_read_all();
		$key     = (string) $key;
		if ( ! isset( $payload['data'][ $key ] ) || ! is_array( $payload['data'][ $key ] ) ) {
			$payload['data'][ $key ] = array();
		}
		$payload['data'][ $key ][] = $value;
		return homeboy_callback_data_write_all( $payload );
	}
}

if ( ! function_exists( 'homeboy_callback_data_output_event' ) ) {
	function homeboy_callback_data_output_event( $name, $payload = array(), array $metadata = array() ) {
		$envelope = homeboy_callback_data_read_all();
		$envelope['events'][] = array(
			'schema'     => 'homeboy/runtime-callback-event/v1',
			'name'       => (string) $name,
			'payload'    => $payload,
			'metadata'   => $metadata,
			'created_at' => gmdate( 'c' ),
		);
		return homeboy_callback_data_write_all( $envelope );
	}
}
`;
}

function writeHomeboyCallbackDataPlugin(pluginDir, pluginFile = 'homeboy-runtime-callback-data.php') {
  fs.writeFileSync(path.join(pluginDir, pluginFile), homeboyCallbackDataPluginSource());
}

function homeboyRuntimeToolBridgeServerSource() {
  return `#!/usr/bin/env node
'use strict';
const http = require('node:http');
const { spawnSync } = require('node:child_process');

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on('data', (chunk) => chunks.push(chunk));
    req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
    req.on('error', reject);
  });
}

function unsupported(request) {
  return {
    schema: process.env.HOMEBOY_AGENT_TOOL_RESULT_SCHEMA || 'homeboy/agent-tool-result/v1',
    request_id: request.request_id || '',
    task_id: request.task_id || '',
    tool: request.tool || '',
    status: 'failed',
    output: null,
    diagnostics: [{
      class: 'agent_tool.control_plane_dispatch_unsupported',
      message: 'control-plane tool dispatch is selected by policy, but no dispatcher command is registered for this provider execution',
      data: { tool: request.tool || '' },
    }],
    metadata: { execution_location: 'control_plane' },
  };
}

function dispatch(request) {
  const command = process.env.HOMEBOY_AGENT_TOOL_DISPATCH_COMMAND || '';
  if (!command) {
    return unsupported(request);
  }
  const result = spawnSync(command, [], {
    input: JSON.stringify(request),
    encoding: 'utf8',
    shell: true,
    maxBuffer: 1024 * 1024 * 10,
    timeout: Number.parseInt(process.env.HOMEBOY_AGENT_TOOL_DISPATCH_TIMEOUT_MS || request.timeout_ms || 120000, 10),
  });
  if (result.error || result.status !== 0) {
    return {
      ...unsupported(request),
      diagnostics: [{
        class: 'agent_tool.control_plane_dispatch_failed',
        message: result.error ? result.error.message : (result.stderr || 'control-plane dispatcher command failed'),
        data: { status: result.status, signal: result.signal, tool: request.tool || '' },
      }],
    };
  }
  try {
    return JSON.parse(result.stdout || '{}');
  } catch {
    return {
      ...unsupported(request),
      diagnostics: [{
        class: 'agent_tool.control_plane_dispatch_invalid_result',
        message: 'control-plane dispatcher command returned invalid JSON',
        data: { tool: request.tool || '' },
      }],
    };
  }
}

const server = http.createServer(async (req, res) => {
  if (req.method !== 'POST') {
    res.writeHead(405, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ error: 'method_not_allowed' }));
    return;
  }
  try {
    const request = JSON.parse(await readBody(req));
    const result = dispatch(request);
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify(result));
  } catch (error) {
    res.writeHead(500, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ error: error.message }));
  }
});

server.listen(0, '127.0.0.1', () => {
  const address = server.address();
  const payload = JSON.stringify({ url: 'http://127.0.0.1:' + address.port });
  if (process.env.HOMEBOY_AGENT_TOOL_BRIDGE_READY_FILE) {
    require('node:fs').writeFileSync(process.env.HOMEBOY_AGENT_TOOL_BRIDGE_READY_FILE, payload);
  }
  process.stdout.write(payload + '\\n');
});
`;
}

function injectHomeboyRuntimeToolBridge(taskInput, artifacts) {
  return { input: taskInput, bridge: null };
}

function callbackDataConfig(taskInput) {
  const configured = taskInput.callback_data || taskInput.callbackData || taskInput.parent_request?.callback_data || taskInput.parent_request?.callbackData;
  if (configured === false || configured?.enabled === false) {
    return { enabled: false };
  }
  return { enabled: true, ...(plainObject(configured) ? configured : {}) };
}

function callbackDataPath(artifacts, config = {}) {
  return config.path || config.file || path.join(artifacts, 'homeboy-runtime-callback-data.json');
}

function injectHomeboyCallbackDataHelper(taskInput, artifacts) {
  const config = callbackDataConfig(taskInput);
  if (!config.enabled) {
    return { input: taskInput, callback_data: null };
  }

  const callbackPath = callbackDataPath(artifacts, config);
  fs.mkdirSync(path.dirname(callbackPath), { recursive: true });
  fs.writeFileSync(callbackPath, `${JSON.stringify({ data: plainObject(config.initial) ? config.initial : {}, events: [] }, null, 2)}\n`);

  const pluginSlug = safePluginSlug(`homeboy-runtime-callback-data-${taskInput.sandbox_session_id || process.pid}`, 'homeboy-runtime-callback-data');
  const pluginRoot = fs.mkdtempSync(path.join(os.tmpdir(), `${pluginSlug}-`));
  const pluginDir = path.join(pluginRoot, pluginSlug);
  fs.mkdirSync(pluginDir, { recursive: true });
  writeHomeboyCallbackDataPlugin(pluginDir, `${pluginSlug}.php`);

  return {
    input: {
      ...taskInput,
      extra_plugins: [
        ...(Array.isArray(taskInput.extra_plugins) ? taskInput.extra_plugins : []),
        {
          source: pluginDir,
          slug: pluginSlug,
          pluginFile: `${pluginSlug}/${pluginSlug}.php`,
          loadAs: 'mu-plugin',
          activate: false,
          metadata: { source: 'homeboy-runtime-callback-data' },
        },
      ],
      runtime_env: {
        ...(plainObject(taskInput.runtime_env) ? taskInput.runtime_env : {}),
        HOMEBOY_CALLBACK_DATA_PATH: callbackPath,
      },
    },
    callback_data: {
      path: callbackPath,
      plugin_dir: pluginDir,
    },
  };
}

function readCallbackData(callbackData) {
  if (!callbackData?.path) {
    return null;
  }
  const payload = readJsonIfAvailable(callbackData.path);
  if (!plainObject(payload)) {
    return null;
  }
  return {
    path: callbackData.path,
    data: plainObject(payload.data) ? payload.data : {},
    events: Array.isArray(payload.events) ? payload.events : [],
  };
}

function withCallbackDataEnvelope(payload, callbackData) {
  if (!callbackData || (Object.keys(callbackData.data).length === 0 && callbackData.events.length === 0)) {
    return payload;
  }
  const artifact = {
    id: 'homeboy-runtime-callback-data',
    kind: 'runtime-callback-data',
    path: callbackData.path,
  };
  return {
    ...payload,
    outputs: {
      ...(plainObject(payload.outputs) ? payload.outputs : {}),
      callback_data: callbackData.data,
      callback_events: callbackData.events,
    },
    artifacts: [
      ...(Array.isArray(payload.artifacts) ? payload.artifacts : []),
      artifact,
    ],
    evidence_refs: [
      ...(Array.isArray(payload.evidence_refs) ? payload.evidence_refs : []),
      { kind: artifact.kind, uri: artifact.path, label: 'Runtime callback data' },
    ],
    metadata: {
      ...(plainObject(payload.metadata) ? payload.metadata : {}),
      callback_data: callbackData.data,
      callback_events: callbackData.events,
      callback_data_path: callbackData.path,
    },
  };
}

function startHomeboyRuntimeToolBridge(bridge) {
  if (!bridge) {
    return null;
  }
  const readyFile = writeTextFile('homeboy-runtime-tool-bridge-ready-', 'ready.json', '');
  const child = spawn(process.execPath, [bridge.server_script], {
    stdio: ['ignore', 'pipe', 'inherit'],
    env: {
      ...process.env,
      HOMEBOY_AGENT_TOOL_BRIDGE_READY_FILE: readyFile,
    },
  });
  const start = Date.now();
  while (Date.now() - start < 5000) {
    const contents = fs.existsSync(readyFile) ? fs.readFileSync(readyFile, 'utf8') : '';
    if (contents.trim()) {
      const parsed = JSON.parse(contents);
      return { child, url: parsed.url };
    }
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 25);
  }
  child.kill();
  throw new Error('Timed out starting Homeboy runtime tool bridge server.');
}

function stopHomeboyRuntimeToolBridge(server) {
  if (server?.child && !server.child.killed) {
    server.child.kill();
  }
}

function secretEnvValues(secretNames) {
  return Object.fromEntries(secretNames
    .filter((name) => typeof name === 'string' && name !== '' && process.env[name])
    .map((name) => [name, process.env[name]]));
}

function redactString(value, secrets) {
  return Object.entries(secrets).reduce((redacted, [name, secret]) => {
    if (!secret) {
      return redacted;
    }
    return redacted.split(secret).join(`[REDACTED:${name}]`);
  }, String(value));
}

function redactedValue(value, secrets) {
  if (typeof value === 'string') {
    return redactString(value, secrets);
  }
  if (Array.isArray(value)) {
    return value.map((item) => redactedValue(item, secrets));
  }
  if (plainObject(value)) {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => {
      if (/secret|token|password|credential|api[_-]?key/i.test(key)) {
        return [key, item ? '[REDACTED]' : item];
      }
      return [key, redactedValue(item, secrets)];
    }));
  }
  return value;
}

function writeEvidenceFile(artifacts, fileName, contents) {
  try {
    fs.mkdirSync(artifacts, { recursive: true });
    const filePath = path.join(artifacts, fileName);
    fs.writeFileSync(filePath, contents);
    return filePath;
  } catch {
    return '';
  }
}

function readJsonIfAvailable(filePath) {
  try {
    return JSON.parse(fs.readFileSync(filePath, 'utf8'));
  } catch {
    return null;
  }
}

function pathEvidenceCandidates(...texts) {
  const candidates = new Set();
  const pattern = /(?:[A-Za-z]:)?\/[^\s'"`]+(?:wp-codebox-agent-task-recipe|homeboy-wp-codebox-agent-task-input)[^\s'"`)]*/g;
  for (const text of texts) {
    for (const match of String(text || '').matchAll(pattern)) {
      candidates.add(match[0].replace(/[.,;:]+$/, ''));
    }
  }
  return [...candidates];
}

function copiedEvidencePathName(sourcePath, index) {
  const parent = path.basename(path.dirname(sourcePath)).replace(/[^A-Za-z0-9_.-]+/g, '-');
  const base = path.basename(sourcePath).replace(/[^A-Za-z0-9_.-]+/g, '-');
  return `captured-wp-codebox-path-${index + 1}-${parent}-${base}`;
}

function copyGeneratedEvidencePaths(artifacts, candidates, secrets) {
  const copied = [];
  candidates.forEach((candidate, index) => {
    try {
      if (!fs.existsSync(candidate) || !fs.statSync(candidate).isFile()) {
        return;
      }
      const parsed = candidate.endsWith('.json') ? readJsonIfAvailable(candidate) : null;
      const contents = parsed
        ? `${JSON.stringify(redactedValue(parsed, secrets), null, 2)}\n`
        : redactString(fs.readFileSync(candidate, 'utf8'), secrets);
      const target = writeEvidenceFile(artifacts, copiedEvidencePathName(candidate, index), contents);
      if (target) {
        copied.push({ source: candidate, path: target });
      }
    } catch {
      // Best-effort evidence capture must not mask the actual WP Codebox failure.
    }
  });
  return copied;
}

function preserveWpCodeboxFailureEvidence({ artifacts, inputPath, result, command, args, secretNames }) {
  const secrets = secretEnvValues(secretNames);
  const stdoutPath = result.stdout
    ? writeEvidenceFile(artifacts, 'wp-codebox-command-stdout.txt', redactString(result.stdout, secrets))
    : '';
  const stderrPath = result.stderr
    ? writeEvidenceFile(artifacts, 'wp-codebox-command-stderr.txt', redactString(result.stderr, secrets))
    : '';
  const stableInput = readJsonIfAvailable(inputPath);
  const inputEvidencePath = stableInput
    ? writeEvidenceFile(artifacts, 'wp-codebox-agent-task-input.redacted.json', `${JSON.stringify(redactedValue(stableInput, secrets), null, 2)}\n`)
    : '';
  const generatedPathCandidates = pathEvidenceCandidates(result.stdout, result.stderr);
  const copiedGeneratedPaths = copyGeneratedEvidencePaths(artifacts, generatedPathCandidates, secrets);
  const summary = {
    schema: 'homeboy/wp-codebox-command-evidence/v1',
    command,
    args,
    status: result.status,
    signal: result.signal,
    error: result.error ? result.error.message : '',
    input_path: inputPath,
    input_evidence_path: inputEvidencePath,
    stdout_path: stdoutPath,
    stderr_path: stderrPath,
    generated_path_candidates: generatedPathCandidates,
    copied_generated_paths: copiedGeneratedPaths,
  };
  const summaryPath = writeEvidenceFile(artifacts, 'wp-codebox-command-evidence.json', `${JSON.stringify(summary, null, 2)}\n`);
  return {
    ...summary,
    summary_path: summaryPath,
  };
}

function evidenceArtifacts(evidence) {
  return [
    { id: 'wp-codebox-command-evidence', kind: 'codebox-command-evidence', path: evidence.summary_path },
    { id: 'wp-codebox-agent-task-input', kind: 'codebox-agent-task-input', path: evidence.input_evidence_path },
    { id: 'wp-codebox-command-stdout', kind: 'codebox-command-log', path: evidence.stdout_path },
    { id: 'wp-codebox-command-stderr', kind: 'codebox-command-log', path: evidence.stderr_path },
    ...evidence.copied_generated_paths.map((item, index) => ({
      id: `wp-codebox-generated-evidence-${index + 1}`,
      kind: 'codebox-generated-input',
      path: item.path,
      metadata: { source: item.source },
    })),
  ].filter((artifact) => artifact.path);
}

function attachFailureEvidence(payload, evidence) {
  const artifacts = evidenceArtifacts(evidence);
  const diagnostics = evidence.summary_path ? [{
    class: 'wp-codebox.command.evidence_preserved',
    message: 'WP Codebox command stdout, stderr, and redacted task input were preserved for failure diagnosis.',
    data: {
      evidence_path: evidence.summary_path,
      stdout_path: evidence.stdout_path,
      stderr_path: evidence.stderr_path,
      input_evidence_path: evidence.input_evidence_path,
      generated_path_candidates: evidence.generated_path_candidates,
      copied_generated_paths: evidence.copied_generated_paths,
    },
  }] : [];
  let payloadArtifacts = [];
  if (Array.isArray(payload.artifacts)) {
    payloadArtifacts = payload.artifacts;
  } else if (payload.artifacts) {
    payloadArtifacts = [{
      id: 'wp-codebox-artifacts',
      kind: 'codebox-artifact-directory',
      path: payload.artifacts,
    }];
  }
  return {
    ...payload,
    artifacts: [...payloadArtifacts, ...artifacts],
    evidence_refs: [
      ...(Array.isArray(payload.evidence_refs) ? payload.evidence_refs : []),
      ...artifacts.map((artifact) => ({ kind: artifact.kind, uri: artifact.path, label: artifact.kind.replace(/-/g, ' ') })),
    ],
    diagnostics: [...(Array.isArray(payload.diagnostics) ? payload.diagnostics : []), ...diagnostics],
    metadata: {
      ...(plainObject(payload.metadata) ? payload.metadata : {}),
      wp_codebox_command_evidence: evidence.summary_path,
    },
  };
}

function requestAgentBundle(request) {
  if (request.agent_bundle && typeof request.agent_bundle === 'object') {
    return request.agent_bundle;
  }
  return {};
}

function requestRuntimeComponents(request, mounts = []) {
  const explicit = request.runtime_component_paths && typeof request.runtime_component_paths === 'object'
    ? request.runtime_component_paths
    : {};
  const contractPaths = runtimeComponentPathsFromContracts(requestComponentContracts(request));
  const agentRuntime = remapLabWorkspacePath(explicit.agent_runtime || contractPaths.agent_runtime);
  return Object.fromEntries(Object.entries({
    ...contractPaths,
    ...explicit,
    agent_runtime: agentRuntime,
    agent_runtime_tools: remapLabWorkspacePath(explicit.agent_runtime_tools || contractPaths.agent_runtime_tools),
  }).filter(([, value]) => value !== '' && value !== undefined));
}

function runtimeComponentPathsFromContracts(contracts) {
  if (!Array.isArray(contracts)) {
    return {};
  }
  const slugToKey = new Map();
  return Object.fromEntries(contracts
    .map((contract) => [slugToKey.get(contract?.slug), contract?.path || contract?.source])
    .filter(([key, value]) => key && value));
}

function uniqueComponentContracts(contracts) {
  const seen = new Set();
  return contracts.filter((contract) => {
    if (!contract || typeof contract !== 'object' || Array.isArray(contract)) {
      return false;
    }
    const key = `${contract.slug || ''}:${contract.path || contract.source || ''}`;
    if (seen.has(key)) {
      return false;
    }
    seen.add(key);
    return true;
  });
}

function uniqueStrings(values) {
  const seen = new Set();
  return values.filter((value) => {
    const normalized = typeof value === 'string' ? value.trim() : '';
    if (!normalized || seen.has(normalized)) {
      return false;
    }
    seen.add(normalized);
    return true;
  });
}

function runnerInput(request, artifacts) {
  assertProviderCredentialBoundaryNamesOnly(request);
  assertProviderCredentialBoundaryNamesOnly(request.recipe || {});
  const mounts = mountEntries(request);
  const runtimeComponentPaths = {
    ...requestRuntimeComponents(request, mounts),
  };
  const runtimeOverlayProof = request.runtime_overlay_proof === true || request.runtimeOverlayProof === true;
  const finalOverlayProfiles = request.runtime_overlay_profiles || request.runtimeOverlayProfiles || [];
  const finalOverlayValidation = validateRuntimeOverlayProfiles(
    finalOverlayProfiles,
    runtimeOverlayEntries(request),
    { proofBearing: runtimeOverlayProof }
  );
  const finalOverlayDiagnostics = runtimeOverlayProfileReadinessDiagnostics(finalOverlayValidation.profiles);
  if (finalOverlayDiagnostics.length > 0) {
    throw new RuntimeOverlayProfileError(finalOverlayDiagnostics);
  }
  return Object.fromEntries(Object.entries({
    parent_request: request,
    agent: argValue('--agent') || request.agent || '',
    mode: argValue('--mode') || request.mode || 'sandbox',
    provider: argValue('--provider') || request.provider || '',
    model: argValue('--model') || request.model || '',
    provider_plugin_paths: uniqueStrings(firstNonEmptyArray(
      wpCodeboxProviderPluginPathsFromEnv(),
      argValues('--provider-plugin-path'),
      request.provider_plugin_paths,
    )),
    runtime_overlay_profiles: finalOverlayValidation.profiles,
    runtime_overlay_proof: runtimeOverlayProof,
    ...providerCredentialRequestFields({ secret_env: secretEnvNames(request) }),
    mounts,
    runtime_stack_mounts: runtimeStackMountEntries(request),
    runtime_overlays: finalOverlayValidation.overlays,
    runtime_env: request.runtime_env || request.runtimeEnv || {},
    ability_tools: request.ability_tools || request.abilityTools || [],
    runtime_state_mounts: request.runtime_state_mounts || request.runtimeStateMounts || [],
    runtime_config_mounts: request.runtime_config_mounts || request.runtimeConfigMounts || [],
    callback_data: request.callback_data,
    max_turns: Number.parseInt(argValue('--max-turns') || request.max_turns || 0, 10) || undefined,
    task_timeout_seconds: Number.parseInt(argValue('--task-timeout-seconds') || request.task_timeout_seconds || 0, 10) || undefined,
    sandbox_session_id: request.sandbox_session_id || '',
    orchestrator: request.orchestrator || {},
    recipe: request.recipe || {},
    runtime_task: request.runtime_task,
    structured_artifacts: request.structured_artifacts || [],
    artifact_declarations: request.artifact_declarations || [],
    agent_bundles: request.agent_bundles || [],
    artifacts_path: artifacts,
    wp_codebox_bin: argValue('--wp-codebox-bin') || request.wp_codebox_bin || '',
    runtime_component_paths: runtimeComponentPaths,
    component_contracts: componentContracts(request),
    homeboy_path: argValue('--homeboy') || request.homeboy_path || request.homeboy || '',
    homeboy_extensions_path: argValue('--homeboy-extensions') || request.homeboy_extensions_path || request.homeboy_extensions || path.resolve(__dirname, '..', '..'),
    wp_version: request.wordpress_runtime_version || request.wordpress_version || request.wp_codebox_wordpress_version || request.wp_version || request.wp || undefined,
    agent_bundle: requestAgentBundle(request),
  }).filter(([, value]) => value !== '' && value !== undefined && !(Array.isArray(value) && value.length === 0)));
}

function runtimeComponentExtraPlugins() {
  return [];
}

function pluginSlugFromPath(pluginPath) {
  const source = String(pluginPath || '');
  try {
    const composer = JSON.parse(fs.readFileSync(path.join(source, 'composer.json'), 'utf8'));
    const packageName = typeof composer.name === 'string' ? composer.name.split('/').pop() : '';
    if (packageName) {
      return packageName.replace(/[^A-Za-z0-9_-]/g, '-').replace(/^-+|-+$/g, '');
    }
  } catch {
    // Composer metadata is optional for provider plugin directories.
  }
  return path.basename(source).split('@')[0].replace(/[^A-Za-z0-9_-]/g, '-').replace(/^-+|-+$/g, '');
}

function phpPluginHeaderFile(filePath) {
  try {
    const contents = fs.readFileSync(filePath, 'utf8').slice(0, 8192);
    return /Plugin\s+Name\s*:/i.test(contents);
  } catch {
    return false;
  }
}

function providerPluginSource(entry) {
  return typeof entry === 'string' ? entry : entry?.source || entry?.path || '';
}

function providerPluginExplicitFile(entry) {
  if (!entry || typeof entry === 'string' || typeof entry !== 'object' || Array.isArray(entry)) {
    return '';
  }
  return entry.pluginFile || entry.plugin_file || entry.file || '';
}

function providerPluginEntryFile(source, slug, explicitFile = '') {
  if (explicitFile) {
    return explicitFile.includes('/') || explicitFile.includes('\\') ? explicitFile : `${slug}/${explicitFile}`;
  }
  if (!source || !fs.existsSync(source) || !fs.statSync(source).isDirectory()) {
    return '';
  }

  const rootFiles = fs.readdirSync(source)
    .filter((entry) => entry.toLowerCase().endsWith('.php'))
    .sort();
  const headerFile = rootFiles.find((entry) => phpPluginHeaderFile(path.join(source, entry)));
  const selected = headerFile
    || (rootFiles.includes('plugin.php') ? 'plugin.php' : '')
    || (rootFiles.includes(`${slug}.php`) ? `${slug}.php` : '');
  return selected ? `${slug}/${selected}` : '';
}

function providerPluginEntries(input) {
  return (input.provider_plugin_paths || []).flatMap((entry) => {
    const source = providerPluginSource(entry);
    const slug = pluginSlugFromPath(source);
    const pluginFile = providerPluginEntryFile(source, slug, providerPluginExplicitFile(entry));
    return source && slug ? [{
      source,
      slug,
      ...(pluginFile ? { pluginFile } : {}),
      loadAs: 'plugin',
      activate: true,
      metadata: { kind: 'provider-plugin-path', provider: input.provider || '' },
    }] : [];
  });
}

function componentContracts(input) {
  const runtimeContracts = runtimeComponentExtraPlugins(input).map((plugin) => ({
    slug: plugin.slug,
    path: plugin.source,
    loadAs: plugin.loadAs || 'mu-plugin',
    activate: Boolean(plugin.activate),
  }));
  return uniqueComponentContracts([
    ...requestComponentContracts(input).map(remapRuntimeComponentContract),
    ...requestComponentContracts(input.parent_request).map(remapRuntimeComponentContract),
    ...requestComponentContracts(input.parent_request?.parent_request).map(remapRuntimeComponentContract),
    ...runtimeContracts,
  ]);
}

function requestComponentContracts(request) {
  if (!request || typeof request !== 'object') {
    return [];
  }
  return uniqueComponentContracts([
    ...(Array.isArray(request.component_contracts) ? request.component_contracts : []),
    ...(Array.isArray(request.runtime_requirements?.component_contracts) ? request.runtime_requirements.component_contracts.map((contract) => ({
      ...contract,
      metadata: {
        ...(contract.metadata && typeof contract.metadata === 'object' && !Array.isArray(contract.metadata) ? contract.metadata : {}),
        source: 'runtime_requirements.component_contracts',
      },
    })) : []),
    ...(Array.isArray(request.runtime_requirements?.extra_plugins) ? request.runtime_requirements.extra_plugins : []),
    ...runtimeRequirementDependencyContracts(request.runtime_requirements),
  ]);
}

function runtimeRequirementDependencyContracts(runtimeRequirements) {
  if (!runtimeRequirements || typeof runtimeRequirements !== 'object') {
    return [];
  }
  return [
    ...runtimeRequirementDependencyEntries(runtimeRequirements.components, 'mu-plugin'),
    ...runtimeRequirementDependencyEntries(runtimeRequirements.mu_plugins, 'mu-plugin'),
    ...runtimeRequirementDependencyEntries(runtimeRequirements.plugins, 'plugin'),
  ];
}

function runtimeRequirementDependencyEntries(entries, loadAs) {
  if (!Array.isArray(entries)) {
    return [];
  }
  return entries.flatMap((entry) => {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
      return [];
    }
    const source = entry.source || entry.path;
    if (!entry.slug || !source) {
      return [];
    }
    return [{
      slug: entry.slug,
      path: entry.path || source,
      source,
      pluginFile: entry.pluginFile || entry.plugin_file,
      loadAs: entry.loadAs || entry.load_as || loadAs,
      activate: entry.activate,
    }];
  });
}

function verifySteps(input) {
  // Verification gates can come from the request directly or from the parent
  // request/task. Each entry is a WP Codebox recipe step.
  // that runs after the agent finishes; a non-zero exit fails the run.
  const candidates = [
    input.verify_steps,
    input.parent_request?.verify_steps,
    input.parent_request?.task?.verify_steps,
  ];
  for (const candidate of candidates) {
    if (Array.isArray(candidate) && candidate.length > 0) {
      return candidate.filter((step) => step && typeof step === 'object' && typeof step.command === 'string' && step.command !== '');
    }
  }
  return [];
}

function extraPlugins(input) {
  const explicit = input.parent_request?.extra_plugins || input.parent_request?.extraPlugins || [];
  const plugins = [
    ...runtimeComponentExtraPlugins(input),
    ...providerPluginEntries(input),
    ...(Array.isArray(explicit) ? explicit : []),
  ];
  const seen = new Set();
  return plugins.filter((plugin) => {
    const key = `${plugin.slug || ''}:${plugin.source || ''}`;
    if (seen.has(key)) {
      return false;
    }
    seen.add(key);
    return true;
  });
}

function stableTaskInput(input) {
  const allowedTools = input.parent_request?.allowed_tools || input.parent_request?.task?.allowed_tools || [];
  const providerCredentialFields = providerCredentialRequestFields(input, input.parent_request, input.recipe);
  return Object.fromEntries(Object.entries({
    schema: 'wp-codebox/task-input/v1',
    version: 1,
    goal: input.parent_request?.goal || input.parent_request?.task?.prompt || input.parent_request?.task?.goal || '',
    target: input.parent_request?.target || input.parent_request?.task?.target || {},
    allowed_tools: allowedTools,
    ability_requirements: abilityRequirements(input),
    expected_artifacts: input.parent_request?.expected_artifacts || input.parent_request?.task?.expected_artifacts || [],
    artifact_declarations: input.artifact_declarations || input.parent_request?.artifact_declarations || input.parent_request?.task?.artifact_declarations || [],
    structured_artifacts: input.structured_artifacts || [],
    agent_bundles: input.agent_bundles || [],
    sandbox_tool_policy: sandboxToolPolicy(input, allowedTools),
    policy: input.parent_request?.policy || input.parent_request?.task?.policy || {},
    context: input.parent_request?.context || input.parent_request?.task?.context || {},
    recipe: input.recipe || input.parent_request?.recipe || {},
    agent: input.agent,
    provider: input.provider,
    model: input.model,
    provider_plugin_paths: input.provider_plugin_paths || [],
    extra_plugins: extraPlugins(input),
    // WP Codebox 0.8.0 reads runtime component plugins from
    // `component_contracts`; keep `runtime_component_paths` as neutral
    // orchestration metadata and avoid emitting legacy runtime path aliases.
    component_contracts: componentContracts(input),
    // Post-agent verification gate. WP Codebox emits these as recipe
    // `workflow.after` steps that run once the agent finishes editing; any
    // non-zero exit fails the whole run, so the orchestrator cannot report
    // success until the supplied gates (e.g. the repo's smoke/test suite) pass.
    verify_steps: verifySteps(input),
    runtime_overlay_profiles: input.runtime_overlay_profiles || [],
    runtime_overlay_proof: input.runtime_overlay_proof === true,
    ...providerCredentialFields,
    mounts: input.mounts || [],
    workspaces: input.parent_request?.workspaces || [],
    runtime_stack_mounts: input.runtime_stack_mounts || [],
    runtime_overlays: input.runtime_overlays || [],
    runtime_env: input.runtime_env || {},
    ability_tools: input.ability_tools || [],
    runtime_state_mounts: input.runtime_state_mounts || [],
    runtime_config_mounts: input.runtime_config_mounts || [],
    callback_data: input.callback_data || input.parent_request?.callback_data,
    max_turns: input.max_turns,
    task_timeout_seconds: input.task_timeout_seconds,
    sandbox_session_id: input.sandbox_session_id,
    session_id: input.parent_request?.session_id || '',
    group_key: input.parent_request?.group_key || input.parent_request?.context?.group_key || '',
    audit_findings: input.parent_request?.audit_findings || input.parent_request?.context?.audit_findings || [],
    orchestrator: input.orchestrator || {},
    artifacts_path: input.artifacts_path,
    wp_codebox_bin: input.wp_codebox_bin,
    runtime_component_paths: input.runtime_component_paths || {},
    wp: input.wp_version,
    agent_bundle: isAgentBundle(input) ? agentBundleConfig(input, input.agent_bundle || {}) : {},
    runtime_task: runtimeTask(input),
    parent_request: input.parent_request,
  }).filter(([, value]) => value !== '' && value !== undefined && !(Array.isArray(value) && value.length === 0)));
}

function plainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

function artifactDeclarationName(declaration) {
  return artifactNameFromDeclaration(declaration);
}

function requiredArtifactDeclarations(input, config = {}) {
  const declarations = [
    input.artifact_declarations,
    input.parent_request?.artifact_declarations,
    input.parent_request?.task?.artifact_declarations,
    config.artifact_declarations,
  ].find((candidate) => Array.isArray(candidate) && candidate.length > 0) || [];
  return declarations.filter((declaration) => plainObject(declaration) && declaration.required === true && artifactDeclarationName(declaration));
}

function missingRequiredTypedArtifactDiagnostic(input, workload, config = {}) {
  const required = requiredArtifactDeclarations(input, config);
  if (required.length === 0) {
    return null;
  }
  const typedArtifacts = plainObject(workload?.outputs?.typed_artifacts) ? workload.outputs.typed_artifacts : {};
  const missing = required
    .map((declaration) => ({
      name: artifactDeclarationName(declaration),
      type: declaration.type || declaration.kind || declaration.artifact_type || declaration.artifactType || '',
      artifact_schema: declaration.artifact_schema || declaration.artifactSchema || declaration.schema || '',
    }))
    .filter((declaration) => !typedArtifacts[declaration.name]);
  if (missing.length === 0) {
    return null;
  }
  return {
    class: 'wp-codebox.required_typed_artifacts_missing',
    message: `WP Codebox agent task did not produce required typed artifacts: ${missing.map((declaration) => declaration.name).join(', ')}.`,
    data: { reason: 'missing_required_typed_artifacts', missing },
  };
}

function mergeTypedArtifactOutputs(outputs, ...candidates) {
  const typedArtifacts = Object.assign({}, ...candidates.map(normalizeTypedArtifacts));
  if (Object.keys(typedArtifacts).length === 0) {
    return outputs;
  }
  return {
    ...outputs,
    typed_artifacts: {
      ...(plainObject(outputs.typed_artifacts) ? outputs.typed_artifacts : {}),
      ...typedArtifacts,
    },
  };
}

function sandboxToolPolicy(input, allowedTools) {
  const explicit = input.parent_request?.sandbox_tool_policy
    || input.parent_request?.sandboxToolPolicy
    || input.parent_request?.task?.sandbox_tool_policy
    || input.parent_request?.task?.sandboxToolPolicy;
  if (plainObject(explicit) && Array.isArray(explicit.tools) && explicit.tools.length > 0) {
    return explicit;
  }

  const tools = Array.isArray(allowedTools) ? allowedTools.filter((tool) => typeof tool === 'string' && tool.trim() !== '') : [];
  return {
    schema: 'wp-codebox/sandbox-tool-policy/v1',
    version: 1,
    tools: tools.length > 0
      ? tools.map((tool) => {
          const id = tool.trim();
          return {
            id,
            runtime_tool_id: id.replace(/^[^/]+\//, '').replace(/[^A-Za-z0-9_]+/g, '_'),
            execution_location: 'sandbox',
            transport_visibility: 'sandbox',
            allowed: true,
            runtime: { environment: 'runtime_local', capability_scope: 'runtime_local' },
            metadata: { source: 'homeboy_allowed_tools' },
          };
        })
      : [{
          id: 'homeboy/no-runtime-tools',
          runtime_tool_id: 'homeboy_no_runtime_tools',
          execution_location: 'external',
          transport_visibility: 'hidden',
          allowed: false,
          runtime: { environment: 'control_plane', capability_scope: 'control_plane' },
          metadata: { source: 'homeboy_default_empty_policy' },
        }],
    metadata: { source: 'homeboy-wp-codebox-task-runner' },
  };
}

function isAgentBundle(input) {
  return Boolean(input.agent_bundle && Object.keys(input.agent_bundle).length > 0);
}

function runtimeTask(input) {
  if (plainObject(input.runtime_task)) {
    return input.runtime_task;
  }
  if (isAgentBundle(input)) {
    return agentBundleRuntimeTask(input, input.agent_bundle || {});
  }
  return undefined;
}

function abilityRequirements(input) {
  return uniqueStrings([
    runtimeTask(input)?.ability,
    ...(Array.isArray(input.parent_request?.ability_requirements) ? input.parent_request.ability_requirements : []),
    ...(Array.isArray(input.parent_request?.abilityRequirements) ? input.parent_request.abilityRequirements : []),
    ...(Array.isArray(input.parent_request?.parent_request?.ability_requirements) ? input.parent_request.parent_request.ability_requirements : []),
    ...(Array.isArray(input.parent_request?.parent_request?.abilityRequirements) ? input.parent_request.parent_request.abilityRequirements : []),
  ]);
}

function validateRuntimeTaskAbilityContract(input) {
  const task = runtimeTask(input);
  if (!plainObject(task)) {
    return null;
  }
  const ability = typeof task.ability === 'string' ? task.ability.trim() : '';
  const declared = abilityRequirements(input);
  if (!ability || declared.includes(ability)) {
    return null;
  }
  const message = `WP Codebox runtime task ability "${ability}" is not declared in ability_requirements before sandbox dispatch.`;
  return {
    success: false,
    status: 'failed',
    failure_classification: 'provider',
    summary: message,
    diagnostics: [{
      class: 'wp-codebox.preflight.runtime_task_ability_not_declared',
      message,
      data: {
        phase: 'wp-codebox.preflight',
        runtime_task_ability: ability,
        ability_requirements: declared,
        required_contract: 'wp-codebox/task-input/v1 ability_requirements must include runtime_task.ability',
      },
    }],
    metadata: {
      phase: 'wp-codebox.preflight',
      runtime_task_ability: ability,
      ability_requirements: declared,
    },
  };
}

function parentAgentTaskConfig(input) {
  return input.parent_request?.parent_request?.executor?.config || input.parent_request?.executor?.config || {};
}

function isRuntimeTask(input) {
  return plainObject(input.runtime_task) || plainObject(input.parent_request?.runtime_task) || plainObject(input.parent_request?.runtimeTask);
}

function agentBundleConfig(input, bundleConfig = {}) {
  const runtimeComponentPaths = input.runtime_component_paths || {};
  return Object.fromEntries(Object.entries({
    ...bundleConfig,
    prompt: bundleConfig.prompt || input.parent_request?.task?.prompt || input.parent_request?.goal || '',
    provider: bundleConfig.provider || input.provider || '',
    model: bundleConfig.model || input.model || '',
    provider_plugin_paths: bundleConfig.provider_plugin_paths || input.provider_plugin_paths || [],
    wp_codebox_artifacts_dir: input.artifacts_path,
    wp_codebox_components: runtimeComponentPaths,
  }).filter(([, value]) => value !== '' && value !== undefined && !(Array.isArray(value) && value.length === 0)));
}

function agentBundleRuntimeTask(input, bundleConfig = {}) {
  const config = agentBundleConfig(input, bundleConfig);
  const source = config.source || config.bundle_path || config.bundle_host_path || '';
  const runtimeBundles = Array.isArray(config.runtime_bundles) ? config.runtime_bundles : [];
  const ability = config.runtime_task_ability || config.runtime_bundle_ability || input.runtime_task_ability || input.runtime_bundle_ability;
  if (!ability) {
    throw new Error('agent_bundle requires runtime_task_ability or runtime_bundle_ability.');
  }
  return {
    ability,
    input: Object.fromEntries(Object.entries({
      ...config,
      source,
      wait_for_completion: config.wait_for_completion ?? true,
      runtime_bundles: runtimeBundles,
    }).filter(([, value]) => value !== '' && value !== undefined && !(Array.isArray(value) && value.length === 0))),
  };
}

function resultExecutions(result) {
  if (Array.isArray(result.executions) && result.executions.length > 0) {
    return result.executions;
  }
  if (Array.isArray(result.run?.executions)) {
    return result.run.executions;
  }
  return [];
}

function publicArtifactResultPayload(result) {
  const envelope = plainObject(result?.artifact_result) ? result.artifact_result : (plainObject(result?.artifactResult) ? result.artifactResult : null);
  if (!envelope || envelope.schema !== WP_CODEBOX_ARTIFACT_RESULT_ENVELOPE_SCHEMA || !plainObject(envelope.result)) {
    return null;
  }
  return envelope.result;
}

function normalizeAgentTaskRun(input, result) {
  if (!isAgentBundle(input) && !isRuntimeTask(input)) {
    return result;
  }

  const executions = resultExecutions(result);
  const execution = executions.find((item) => item?.recipeCommand === 'wp-codebox.agent-sandbox-run') || executions[0] || null;
  const config = isAgentBundle(input) ? agentBundleConfig(input, input.agent_bundle || {}) : parentAgentTaskConfig(input);
  const stdoutWorkload = agentRuntimeWorkloadFromExecutionStdout(execution, config);
  const fallbackAgentResult = publicArtifactResultPayload(result) || {};
  let agentResult = hasSemanticWorkload(stdoutWorkload) ? stdoutWorkload : fallbackAgentResult;
  if (plainObject(agentResult)) {
    agentResult = {
      ...agentResult,
      outputs: plainObject(agentResult.outputs) ? agentResult.outputs : {},
    };
  }
  const bundleValidation = isAgentBundle(input) ? validateAgentRuntimeWorkload(agentResult, config) : validateRuntimeTaskWorkload(agentResult, config);
  const artifactValidation = missingRequiredTypedArtifactDiagnostic(input, agentResult, config);
  const runtimeFailure = agentRuntimeFailureDiagnostic(agentResult);
  const success = !bundleValidation && !artifactValidation && !runtimeFailure;
  const diagnostics = [
    ...(result.diagnostics || []),
    ...(bundleValidation ? [bundleValidation] : []),
    ...(artifactValidation ? [artifactValidation] : []),
    ...(runtimeFailure ? [runtimeFailure] : []),
    ...(agentRuntimeDiagnostics(agentResult) || []),
  ];

  const artifactResult = plainObject(result.artifact_result)
    ? {
        ...result.artifact_result,
        result: {
          ...(plainObject(result.artifact_result.result) ? result.artifact_result.result : {}),
          outputs: plainObject(agentResult.outputs) ? agentResult.outputs : result.outputs,
        },
      }
    : undefined;

  return {
    ...result,
    success,
    status: success ? 'completed' : result.status,
    ...(artifactResult ? { artifact_result: artifactResult } : {}),
    outputs: plainObject(agentResult.outputs) ? agentResult.outputs : result.outputs,
    summary: success ? 'WP Codebox agent task succeeded.' : (artifactValidation?.message || bundleValidation?.message || runtimeFailure?.message || result.summary || 'WP Codebox agent task failed.'),
    session: result.session ? {
      ...result.session,
      status: success ? 'completed' : 'failed',
    } : result.session,
    diagnostics,
  };
}

function parseJsonObject(value) {
  if (!value || typeof value !== 'string') {
    return null;
  }
  try {
    const parsed = JSON.parse(value);
    return parsed && typeof parsed === 'object' ? parsed : null;
  } catch {
    return null;
  }
}

function agentRuntimeWorkloadFromExecutionStdout(execution, config) {
  const wrapper = parseJsonObject(execution?.stdout || '');
  const workload = parseJsonObject(wrapper?.output || '') || parseJsonObject(execution?.stdout || '');
  if (!workload) {
    return null;
  }
  const bundleRun = workload.agent_runtime?.result || workload.result || workload;
  if (bundleRun?.schema && String(bundleRun.schema).endsWith('/agent-bundle-run/v1')) {
    return agentRuntimeWorkloadFromBundleRun(bundleRun, config);
  }
  if (plainObject(workload.agent_runtime)) {
    return agentRuntimeWorkloadFromRuntimeTask(workload.agent_runtime, config);
  }
  if (Array.isArray(workload.scenarios)) {
    return workload;
  }
  if (isSingleResultWorkload(workload)) {
    return agentRuntimeWorkloadFromSingleResult(workload, config);
  }
  if (workload.metadata || workload.metrics) {
    return {
      scenarios: [{
        id: config.workload_id || config.agent_slug || config.flow_slug || 'agent-bundle',
        metrics: workload.metrics || {},
        metadata: workload.metadata || {},
      }],
    };
  }
  return null;
}

function agentRuntimeWorkloadFromRuntimeTask(agentRuntime, config) {
  const outputs = outputMappingsFromSource(agentRuntime, runtimeOutputProjections(config));
  return Object.fromEntries(Object.entries({
    id: config.workload_id || config.ability || config.ability_name || 'runtime-task',
    success: agentRuntime.success,
    status: agentRuntime.success === false ? 'failed' : 'completed',
    summary: agentRuntime.success === false
      ? (agentRuntime.error?.message || 'Runtime task failed.')
      : 'Runtime task completed.',
    outputs,
    diagnostics: agentRuntime.error ? [{
      class: agentRuntime.error.code || 'runtime_task.failed',
      message: agentRuntime.error.message || 'Runtime task failed.',
      data: agentRuntime.error.data || {},
    }] : [],
    metadata: {
      input: agentRuntime.input,
      result: agentRuntime.result,
    },
  }).filter(([, value]) => value !== undefined && !(Array.isArray(value) && value.length === 0)));
}

function runtimeOutputProjections(config) {
  return firstPlainObject(config.runtime_output_projections, config.runtimeOutputProjections, config.output_mappings, config.outputMappings) || {};
}

function requiredOutputProjections(config) {
  return firstPlainObject(config.runtime_output_projections, config.runtimeOutputProjections, config.engine_data_outputs, config.engineDataOutputs) || {};
}

function configuredEvidenceProjections(config) {
  const projections = [];
  if (Array.isArray(config.evidence_projections)) {
    projections.push(...config.evidence_projections);
  }
  if (Array.isArray(config.evidenceProjections)) {
    projections.push(...config.evidenceProjections);
  }
  if (Array.isArray(config.tool_recorders)) {
    projections.push(...config.tool_recorders);
  }
  if (Array.isArray(config.toolRecorders)) {
    projections.push(...config.toolRecorders);
  }
  return projections;
}

function firstPlainObject(...candidates) {
  return candidates.find(plainObject);
}

function outputMappingsFromSource(source, mappings) {
  if (!plainObject(mappings)) {
    return {};
  }
  const outputs = {};
  const typedArtifacts = {};
  for (const [name, dottedPath] of Object.entries(mappings)) {
    if (typeof dottedPath !== 'string' || dottedPath === '') {
      continue;
    }
    const value = pathValue(source, dottedPath);
    if (value === undefined || value === null || value === '') {
      continue;
    }
    outputs[name] = value;
    if (plainObject(value) && (value.schema || value.artifact_type || value.artifactType)) {
      typedArtifacts[name] = {
        name,
        type: value.artifact_type || value.artifactType || name,
        artifact_schema: value.schema,
        payload: value,
        provenance: plainObject(value.provenance) ? value.provenance : {},
      };
    }
  }
  return mergeTypedArtifactOutputs(outputs, typedArtifacts);
}

function isSingleResultWorkload(workload) {
  return plainObject(workload) && (
    plainObject(workload.outputs)
      || plainObject(workload.output)
      || Array.isArray(workload.diagnostics)
      || typeof workload.summary === 'string'
  );
}

function agentRuntimeWorkloadFromSingleResult(workload, config) {
  let outputs = {};
  if (plainObject(workload.outputs)) {
    outputs = workload.outputs;
  } else if (plainObject(workload.output)) {
    outputs = workload.output;
  }
  outputs = mergeTypedArtifactOutputs(outputs, workload.typed_artifacts, workload.typedArtifacts, outputs.typed_artifacts, outputs.typedArtifacts);
  return Object.fromEntries(Object.entries({
    id: config.workload_id || config.agent_slug || config.flow_slug || 'agent-bundle',
    success: workload.success,
    status: workload.status,
    summary: workload.summary || workload.message,
    outputs,
    diagnostics: Array.isArray(workload.diagnostics) ? workload.diagnostics : [],
    metrics: workload.metrics || {},
    metadata: workload.metadata || {},
  }).filter(([, value]) => value !== undefined && !(Array.isArray(value) && value.length === 0)));
}

function agentRuntimeWorkloadFromBundleRun(bundleRun, config) {
  return normalizeRuntimeAgentBundleResult(bundleRun, config, {
    legacyProjectionOutputs: evidenceProjectionOutputs,
    mergeTypedArtifactOutputs,
  });
}

function hasScenarios(value) {
  return Array.isArray(value?.scenarios) && value.scenarios.length > 0;
}

function hasSemanticWorkload(value) {
  return hasScenarios(value) || plainObject(value?.outputs) || isSingleResultWorkload(value);
}

function pathValue(source, dottedPath) {
  return String(dottedPath || '').split('.').filter(Boolean).reduce((value, key) => (value && typeof value === 'object' ? value[key] : undefined), source);
}

function stepDataPackets(engineData) {
  const packetsByStep = plainObject(engineData?.direct_step_data_packets) ? engineData.direct_step_data_packets : {};
  return Object.values(packetsByStep).flatMap((packets) => (Array.isArray(packets) ? packets : []));
}

function evidenceProjectionOutputs(engineData, config) {
  if (!plainObject(engineData)) {
    return {};
  }

  const projections = configuredEvidenceProjections(config);
  if (projections.length === 0) {
    return {};
  }

  const outputs = {};
  const packets = stepDataPackets(engineData);
  for (const projection of projections) {
    if (!plainObject(projection)) {
      continue;
    }

    const operation = projection.operation || projection.tool || projection.provider_operation;
    if (typeof operation !== 'string' || operation === '') {
      continue;
    }

    const record = plainObject(projection.record) ? projection.record : {};
    const fields = firstPlainObject(projection.outputs, projection.fields, record.outputs, record.fields) || {};
    if (Object.keys(fields).length === 0) {
      continue;
    }

    const packet = packets.find((candidate) => {
      const metadata = plainObject(candidate?.metadata) ? candidate.metadata : {};
      return metadata.tool_name === operation && metadata.step_execution_success === true;
    });
    if (!packet) {
      continue;
    }

    const metadata = plainObject(packet.metadata) ? packet.metadata : {};
    const sources = [
      metadata.tool_result_data,
      metadata.tool_result_envelope,
      metadata.tool_result_envelope?.result,
    ].filter(plainObject);

    for (const [outputName, resultPath] of Object.entries(fields)) {
      if (outputs[outputName] !== undefined || typeof resultPath !== 'string') {
        continue;
      }
      for (const source of sources) {
        const value = pathValue(source, resultPath);
        if (value !== undefined && value !== null && value !== '') {
          outputs[outputName] = value;
          break;
        }
      }
    }
  }

  return outputs;
}

function validateAgentRuntimeWorkload(workload, config) {
  const scenarios = Array.isArray(workload?.scenarios) ? workload.scenarios : [];
  const failedScenario = scenarios.find((scenario) => scenario?.metadata?.error || scenario?.metadata?.error_message);
  if (failedScenario) {
    return {
      class: 'agent_runtime.workload.failed',
      message: failedScenario.metadata.error || failedScenario.metadata.error_message,
      data: { reason: 'scenario_error', scenario_id: failedScenario.id, metadata: failedScenario.metadata },
    };
  }

  const outputs = requiredOutputProjections(config);
  const missing = [];
  for (const [name, outputPath] of Object.entries(outputs)) {
    if (workload?.outputs?.[name] !== undefined && workload.outputs[name] !== null && workload.outputs[name] !== '') {
      continue;
    }
    const present = scenarios.some((scenario) => {
      const value = pathValue(scenario, outputPath);
      return value !== undefined && value !== null && value !== '';
    });
    if (!present) {
      missing.push({ name, path: outputPath });
    }
  }

  if (missing.length > 0) {
    return {
      class: 'agent_runtime.workload.incomplete',
      message: `Agent bundle workload did not produce required semantic outputs: ${missing.map((item) => item.name).join(', ')}.`,
      data: { reason: 'missing_runtime_output_projections', missing },
    };
  }

  if (scenarios.length === 0 && (!plainObject(workload?.outputs) || Object.keys(workload.outputs).length === 0)) {
    return {
      class: 'agent_runtime.workload.incomplete',
      message: 'Agent bundle workload did not produce scenarios or semantic outputs.',
      data: { reason: 'missing_semantic_outputs' },
    };
  }

  return null;
}

function agentRuntimeFailureCandidates(workload) {
  const scenarios = Array.isArray(workload?.scenarios) ? workload.scenarios : [];
  return [
    workload,
    workload?.outputs,
    workload?.metadata,
    ...scenarios,
    ...scenarios.map((scenario) => scenario?.metadata),
    ...scenarios.map((scenario) => scenario?.outputs),
  ].filter(plainObject);
}

function agentRuntimeCandidateFailed(candidate) {
  const terminalStatus = String(candidate.terminal_status || candidate.terminalStatus || '').toLowerCase();
  const status = String(candidate.status || candidate.outcome || '').toLowerCase();
  const completionStatus = String(candidate.completion_outcome?.status || candidate.completionOutcome?.status || '').toLowerCase();
  return candidate.success === false
    || candidate.completion_outcome?.success === false
    || candidate.completionOutcome?.success === false
    || terminalStatus === 'failed'
    || terminalStatus.startsWith('failed ')
    || status === 'failed'
    || completionStatus === 'failed'
    || Boolean(candidate.error_reason || candidate.errorReason || candidate.error_step_id || candidate.errorStepId);
}

function agentRuntimeFailureReason(candidate) {
  const terminalStatus = String(candidate.terminal_status || candidate.terminalStatus || '');
  return candidate.error_reason
    || candidate.errorReason
    || candidate.reason
    || candidate.completion_outcome?.reason
    || candidate.completionOutcome?.reason
    || (terminalStatus.startsWith('failed - ') ? terminalStatus.slice('failed - '.length).trim() : '')
    || candidate.status
    || '';
}

function agentRuntimeFailureDiagnostic(workload) {
  const failure = agentRuntimeFailureCandidates(workload).find(agentRuntimeCandidateFailed);
  if (!failure) {
    return null;
  }
  const reason = agentRuntimeFailureReason(failure);
  const message = failure.error_message
    || failure.errorMessage
    || failure.message
    || failure.summary
    || (reason ? `Embedded agent runtime failed: ${reason}.` : 'Embedded agent runtime failed.');
  return {
    class: 'agent_runtime.failed',
    message,
    data: {
      reason,
      status: failure.status,
      terminal_status: failure.terminal_status || failure.terminalStatus,
      error_reason: failure.error_reason || failure.errorReason,
      error_step_id: failure.error_step_id || failure.errorStepId,
    },
  };
}

function validateRuntimeTaskWorkload(workload, config) {
  if (workload?.success === false) {
    return {
      class: 'runtime_task.failed',
      message: workload.summary || 'Runtime task failed.',
      data: { reason: 'runtime_task_failed', diagnostics: workload.diagnostics || [] },
    };
  }
  const outputs = requiredOutputProjections(config);
  const missing = [];
  for (const name of Object.keys(outputs)) {
    if (workload?.outputs?.[name] !== undefined && workload.outputs[name] !== null && workload.outputs[name] !== '') {
      continue;
    }
    missing.push(name);
  }
  if (missing.length > 0) {
    return {
      class: 'runtime_task.outputs_missing',
      message: `Runtime task did not produce required outputs: ${missing.join(', ')}.`,
      data: { reason: 'missing_runtime_task_outputs', missing },
    };
  }
  return null;
}

function agentRuntimeDiagnostics(workload) {
  const workloadDiagnostics = Array.isArray(workload?.diagnostics) ? workload.diagnostics.map((diagnostic) => ({
    class: diagnostic.class || diagnostic.kind || 'agent_runtime.workload',
    message: diagnostic.message || String(diagnostic),
    data: diagnostic.data || {},
  })) : [];
  const scenarios = Array.isArray(workload?.scenarios) ? workload.scenarios : [];
  const diagnostics = scenarios
    .filter((scenario) => scenario?.metadata?.error || scenario?.metadata?.error_message)
    .map((scenario) => ({
      class: 'agent_runtime.workload',
      message: scenario.metadata.error || scenario.metadata.error_message,
      data: { scenario_id: scenario.id, metadata: scenario.metadata },
    }));
  const allDiagnostics = [...workloadDiagnostics, ...diagnostics];
  return allDiagnostics.length > 0 ? allDiagnostics : null;
}

function timeoutPayload(timeoutMs, artifacts, evidencePath, inputPath, command, args) {
  const artifact = evidencePath ? [{
    id: 'homeboy-codebox-task-runner-preflight',
    kind: 'codebox-task-runner-preflight',
    path: evidencePath,
    metadata: { inputPath, artifacts, command, args },
  }] : [];
  return {
    success: false,
    timeout: true,
    summary: `WP Codebox agent-task-run timed out after ${timeoutMs}ms.`,
    artifacts: artifact,
    evidence_refs: evidencePath ? [{
      kind: 'codebox-task-runner-preflight',
      uri: evidencePath,
      label: 'WP Codebox task runner preflight evidence',
    }] : [],
    diagnostics: [{
      class: 'codebox.run_agent_task.timeout',
      message: 'wp-codebox agent-task-run exceeded the configured task timeout.',
      data: { timeout_ms: timeoutMs, inputPath, artifacts },
    }],
    metadata: { timeout_ms: timeoutMs, inputPath, artifacts, command, args },
  };
}

function commandFailurePayload(result, artifacts, evidence) {
  const message = result.stderr || result.stdout || result.error?.message || 'WP Codebox agent-task-run failed.';
  return attachFailureEvidence({
    success: false,
    status: 'failed',
    summary: message.split('\n').find((line) => line.trim() !== '') || 'WP Codebox agent-task-run failed.',
    artifacts: [],
    evidence_refs: [],
    diagnostics: [{
      class: 'wp-codebox.agent_task_run_failed',
      message: message.trim() || 'WP Codebox agent-task-run failed.',
      data: {
        status: result.status,
        signal: result.signal,
        error: result.error ? result.error.message : '',
        artifacts,
      },
    }],
    metadata: {
      status: result.status,
      signal: result.signal,
      error: result.error ? result.error.message : '',
    },
  }, evidence);
}

function emptyJsonPayload(payload) {
  if (!plainObject(payload)) {
    return false;
  }
  return !payload.schema
    && payload.success !== false
    && !payload.status
    && !payload.run
    && !payload.session
    && !payload.summary
    && (!Array.isArray(payload.diagnostics) || payload.diagnostics.length === 0)
    && (!Array.isArray(payload.artifacts) || payload.artifacts.length === 0)
    && (!Array.isArray(payload.evidence_refs) || payload.evidence_refs.length === 0);
}

function emptyOutputFailurePayload(payload, result, artifacts, message, diagnosticClass) {
  return {
    ...payload,
    success: false,
    status: 'failed',
    summary: message,
    diagnostics: [
      ...(Array.isArray(payload.diagnostics) ? payload.diagnostics : []),
      {
        class: diagnosticClass,
        message,
        data: {
          status: result.status,
          signal: result.signal,
          artifacts,
        },
      },
    ],
    metadata: {
      ...(plainObject(payload.metadata) ? payload.metadata : {}),
      empty_output: true,
      status: result.status,
      signal: result.signal,
    },
  };
}

function emptyJsonPayloadFailure(payload, result, artifacts) {
  return emptyOutputFailurePayload(
    payload,
    result,
    artifacts,
    'WP Codebox agent-task-run returned an empty JSON payload.',
    'wp-codebox.agent_task_run_empty_json',
  );
}

function emptyStdoutPayloadFailure(result, artifacts) {
  return emptyOutputFailurePayload(
    {},
    result,
    artifacts,
    'WP Codebox agent-task-run exited successfully without stdout.',
    'wp-codebox.agent_task_run_empty_stdout',
  );
}

function runWpCodeboxParentTask(request, envOverrides = {}) {
  const explicitArtifacts = argValue('--artifacts') || request.artifacts_path || '';
  const artifacts = explicitArtifacts || fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-artifacts-'));
  if (explicitArtifacts) {
    const riskyMount = mountEntries(request).find((mount) => pathInside(mount.source, explicitArtifacts));
    if (riskyMount) {
      console.error(`Warning: WP Codebox artifact directory is inside mounted source ${riskyMount.source} and may be captured recursively: ${explicitArtifacts}`);
    }
  }

  const input = runnerInput(request, artifacts);
  const runtimePreflight = preflightWpCodeboxRuntime({ bin: input.wp_codebox_bin, strictBin: Boolean(input.wp_codebox_bin) });
  if (!runtimePreflight.ready) {
    process.stdout.write(`${JSON.stringify(runtimeVersionFailurePayload(input, artifacts, runtimePreflight), null, 2)}\n`);
    return 1;
  }

  const callbackInput = injectHomeboyCallbackDataHelper(stableTaskInput(input), artifacts);
  const bridgedInput = injectHomeboyRuntimeToolBridge(callbackInput.input, artifacts);
  const runtimeTaskAbilityPreflight = validateRuntimeTaskAbilityContract({ ...input, parent_request: bridgedInput.input });
  if (runtimeTaskAbilityPreflight) {
    process.stdout.write(`${JSON.stringify(runtimeTaskAbilityPreflight, null, 2)}\n`);
    return 1;
  }
  const bridgeServer = startHomeboyRuntimeToolBridge(bridgedInput.bridge);
  if (bridgeServer && bridgedInput.bridge?.plugin_dir) {
  }
  const preparedInput = prepareStableTaskInput({
    ...bridgedInput.input,
    runtime_env: {
      ...(plainObject(bridgedInput.input.runtime_env) ? bridgedInput.input.runtime_env : {}),
      ...(bridgeServer ? { HOMEBOY_AGENT_TOOL_BRIDGE_URL: bridgeServer.url } : {}),
    },
  }, artifacts);
  const invocation = codeboxRunAgentTaskInvocation({
    taskInput: preparedInput.input,
    artifactsPath: artifacts,
    previewHold: argValue('--preview-hold'),
    previewPublicUrl: argValue('--preview-public-url'),
  });
  const inputPath = writeJsonFile(
    'homeboy-wp-codebox-run-agent-task-',
    invocation.input
  );
  const args = invocation.args.map((arg) => arg === '--input-file={{input_file}}' ? `--input-file=${inputPath}` : arg);

  const resolved = wpCodeboxResolveCommand(runtimePreflight.selected.path, args);
  const timeoutMs = requestTimeoutMs(request);
  const evidencePath = writePreflightEvidence(artifacts, {
    schema: 'homeboy/wp-codebox-task-runner-preflight/v1',
    inputPath,
    artifacts,
    command: resolved.command,
    args: resolved.args,
    prepared_plugins: preparedInput.prepared_plugins,
    run_agent_task_contract: invocation.contract,
    run_agent_task_implementation: invocation.implementation,
    expected_result_schema: invocation.result_schema,
    timeout_ms: timeoutMs,
    task_id: request.orchestrator?.agent_task_id,
    sandbox_session_id: request.sandbox_session_id,
    runtime_overlay_proof: input.runtime_overlay_proof === true,
    runtime_overlay_profiles: input.runtime_overlay_profiles || [],
  });

  if (hasFlag('--print-command')) {
    console.error(JSON.stringify({ command: resolved.command, args: resolved.args, input }, null, 2));
  }

  const debug = runtimeAgentDebugEnabled();
  let result;
  try {
    result = spawnSync(resolved.command, resolved.args, {
      encoding: 'utf8',
      env: {
        ...process.env,
        ...envOverrides,
      },
      maxBuffer: 1024 * 1024 * 20,
      timeout: timeoutMs,
      // In debug mode inherit the CLI's stderr so it streams live to the job
      // log (crash/timeout-proof); stdout stays piped to capture the JSON
      // outcome. Stdin stays an empty pipe as before (CLI reads --input-file).
      ...(debug ? { stdio: ['pipe', 'pipe', 'inherit'] } : {}),
    });
  } finally {
    stopHomeboyRuntimeToolBridge(bridgeServer);
  }
  writeRuntimeRawOutputFiles(result, request);
  if (debug && result.stdout) {
    // stdout is the JSON return channel for this process, so the captured
    // CLI stdout is not otherwise visible; mirror it to stderr for the log.
    process.stderr.write(`[wp-codebox-cli stdout]\n${String(result.stdout)}\n`);
  }
  const shouldPreserveEvidence = Boolean(result.error) || result.status !== 0;
  const failureEvidence = shouldPreserveEvidence ? preserveWpCodeboxFailureEvidence({
    artifacts,
    inputPath,
    result,
    command: resolved.command,
    args: resolved.args,
    secretNames: input.secret_env || [],
  }) : null;

  if (result.error && result.error.code === 'ETIMEDOUT') {
    process.stdout.write(`${JSON.stringify(timeoutPayload(timeoutMs, artifacts, evidencePath, inputPath, resolved.command, resolved.args), null, 2)}\n`);
    return 1;
  }

  if (result.stdout) {
    try {
      const payload = normalizeAgentTaskRun(input, JSON.parse(result.stdout));
      const emptyPayload = emptyJsonPayload(payload);
      const normalizedPayload = emptyPayload ? emptyJsonPayloadFailure(payload, result, artifacts) : payload;
      const payloadFailed = payload.success === false || payload.status === 'failed' || payload.session?.status === 'failed';
      const payloadEvidence = failureEvidence || (emptyPayload || payloadFailed ? preserveWpCodeboxFailureEvidence({
        artifacts,
        inputPath,
        result,
        command: resolved.command,
        args: resolved.args,
        secretNames: input.secret_env || [],
      }) : null);
      const callbackPayload = withCallbackDataEnvelope(normalizedPayload, readCallbackData(callbackInput.callback_data));
      const enrichedPayload = payloadEvidence ? attachFailureEvidence(callbackPayload, payloadEvidence) : callbackPayload;
      process.stdout.write(`${JSON.stringify(enrichedPayload, null, 2)}\n`);
      return callbackPayload.success === false ? 1 : 0;
    } catch {
      if (failureEvidence) {
        process.stdout.write(`${JSON.stringify(commandFailurePayload(result, artifacts, failureEvidence), null, 2)}\n`);
        return result.status ?? 1;
      }
      process.stdout.write(result.stdout);
    }
  }
  if (result.stderr) {
    if (failureEvidence) {
      process.stdout.write(`${JSON.stringify(commandFailurePayload(result, artifacts, failureEvidence), null, 2)}\n`);
      return result.status ?? 1;
    }
    process.stderr.write(result.stderr);
  }
  if (result.error) {
    process.stderr.write(`${result.error.message}\n`);
  }
  if ((result.status ?? 0) === 0) {
    const evidence = preserveWpCodeboxFailureEvidence({
      artifacts,
      inputPath,
      result,
      command: resolved.command,
      args: resolved.args,
      secretNames: input.secret_env || [],
    });
    process.stdout.write(`${JSON.stringify(attachFailureEvidence(emptyStdoutPayloadFailure(result, artifacts), evidence), null, 2)}\n`);
    return 1;
  }
  return result.status ?? 1;
}

(async () => {
try {
  const request = readTaskRequest();
  await codexAuthPreflightEnv(request);
  if (request.provider === 'claude-code') {
    assertProviderSecretEnvPreflight(requestWithCliSecretEnv(request), request.provider, process.env);
  }
  assertRequiredSecretEnvAvailable(request);
  process.exitCode = runWpCodeboxParentTask(request);
} catch (error) {
  console.error(error && error.message ? error.message : String(error));
  process.exitCode = 1;
}
})();
