#!/usr/bin/env node
// Deploy an exact commit through a remote checkout's own deploy command.
//
// Homeboy supplies the clean component HEAD as the source revision. This provider
// refuses unless that revision is the tip of the configured branch, pushes it to a
// remote checkout over SSH, checks it out there, and runs the host's deploy command,
// waiting out a shared deploy lock. Evidence records the revision and whatever the
// deploy command reports through the success pattern's named groups.

import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';

const SCHEMA = 'homeboy/ssh-checkout-deploy-result/v1';
const PAYLOAD_SCHEMA = 'homeboy/deployment-provider-payload/v1';
const POLICY_PATH = 'homeboy.json#/deployment_provider/policy';
const SHA = /^[0-9a-f]{40}$/;
const REMOTE_PATH = /^\/[A-Za-z0-9._\/-]+$/;
const HOST = /^[A-Za-z0-9._@-]+$/;
const REF = /^refs\/heads\/[A-Za-z0-9._\/-]+$/;
const BRANCH = /^[A-Za-z0-9._\/-]+$/;
const OUTPUT_TAIL_LINES = 20;
const MAX_PATHS = 20;

class StageError extends Error {
	constructor( stage, code, message, remediation = [] ) {
		super( message );
		this.stage = stage;
		this.code = code;
		this.remediation = remediation;
	}
}

async function main() {
	const dryRun = process.argv.includes( '--dry-run' );
	const contract = normalize( JSON.parse( await readFile( requiredArgument( '--contract' ), 'utf8' ) ) );
	const root = process.env.HOMEBOY_COMPONENT_PATH;
	const result = {
		schema: SCHEMA,
		mode: dryRun ? 'dry_run' : 'apply',
		status: 'running',
		source: { component: contract.source.component, revision: contract.source.revision },
		target: { host: contract.target.ssh_host, path: contract.target.remote_path },
		stages: [],
		lock_waits: [],
		deploy: null,
		failure: null,
		remediation: [],
	};

	try {
		if ( ! root ) {
			throw new StageError( 'source', 'component_path_missing', 'HOMEBOY_COMPONENT_PATH is not set.' );
		}
		await stage( result, 'source', () => verifySource( contract, root ) );
		await stage( result, 'freshness', () => verifyFreshness( contract, root ) );
		await stage( result, 'remote_preflight', () => remotePreflight( contract ) );
		if ( dryRun ) {
			result.status = 'validated';
		} else {
			await stage( result, 'sync', () => syncRemote( contract, root ) );
			await stage( result, 'deploy', () => deploy( contract, result ) );
			result.status = 'succeeded';
		}
	} catch ( error ) {
		result.status = 'failed';
		result.failure = {
			stage: error.stage || 'unknown',
			code: error.code || 'deployment_failed',
			message: error.message,
		};
		if ( error.paths ) {
			result.failure.paths = error.paths;
		}
		result.remediation.push( ...( error.remediation || [] ) );
	}

	process.stdout.write( `${ JSON.stringify( result, null, 2 ) }\n` );
	process.exitCode = [ 'succeeded', 'validated' ].includes( result.status ) ? 0 : 1;
}

function normalize( input ) {
	if ( input?.schema !== PAYLOAD_SCHEMA ) {
		throw new Error( `Unsupported deployment payload schema; expected ${ PAYLOAD_SCHEMA }.` );
	}
	assertKeys( input, [ 'schema', 'policy', 'target', 'source' ], 'payload' );
	const policy = input.policy?.value;
	const reference = input.policy?.reference;
	const target = input.target;
	const source = input.source;
	if ( ! isObject( policy ) || ! isObject( reference ) || ! isObject( target ) || ! isObject( source ) ) {
		throw new Error( 'Deployment payload requires policy, target, and source objects.' );
	}
	if ( reference.component !== source.component || reference.path !== POLICY_PATH ) {
		throw new Error( 'Deployment policy reference is invalid.' );
	}
	if ( createHash( 'sha256' ).update( canonicalJson( policy ) ).digest( 'hex' ) !== reference.digest ) {
		throw new Error( 'Deployment policy digest does not match the declared policy.' );
	}
	if ( ! SHA.test( source.revision || '' ) ) {
		throw new Error( 'Source revision must be a full commit SHA.' );
	}

	assertKeys( policy, [ 'branch', 'deploy_command', 'busy_pattern', 'success_pattern', 'running_probe', 'lock_retry', 'deploy_ref', 'timeout_ms' ], 'policy' );
	assertKeys( target, [ 'ssh_host', 'remote_path', 'freshness_remote', 'freshness_git_config' ], 'target' );

	const normalized = {
		source,
		policy: {
			branch: required( policy, 'branch', BRANCH ),
			deploy_command: requiredString( policy, 'deploy_command' ),
			busy: regex( policy.busy_pattern, 'busy_pattern' ),
			success: regex( requiredString( policy, 'success_pattern' ), 'success_pattern' ),
			running_probe: policy.running_probe === undefined ? null : requiredString( policy, 'running_probe' ),
			deploy_ref: policy.deploy_ref === undefined ? 'refs/heads/homeboy-deploy' : required( policy, 'deploy_ref', REF ),
			attempts: positiveInteger( policy.lock_retry?.attempts ?? 1, 'lock_retry.attempts' ),
			delay_ms: positiveInteger( policy.lock_retry?.delay_ms ?? 45000, 'lock_retry.delay_ms' ),
			timeout_ms: positiveInteger( policy.timeout_ms ?? 1800000, 'timeout_ms' ),
		},
		target: {
			ssh_host: required( target, 'ssh_host', HOST ),
			remote_path: required( target, 'remote_path', REMOTE_PATH ),
			freshness_remote: requiredString( target, 'freshness_remote' ),
			freshness_git_config: target.freshness_git_config ?? {},
		},
	};
	if ( ! isObject( normalized.target.freshness_git_config ) ) {
		throw new Error( 'target.freshness_git_config must be an object of git config keys to values.' );
	}
	return normalized;
}

async function stage( result, id, fn ) {
	const started = Date.now();
	try {
		const detail = await fn();
		result.stages.push( { id, status: 'succeeded', elapsed_ms: Date.now() - started, ...( detail || {} ) } );
	} catch ( error ) {
		result.stages.push( { id, status: 'failed', elapsed_ms: Date.now() - started } );
		if ( ! error.stage ) {
			error.stage = id;
		}
		throw error;
	}
}

async function verifySource( contract, root ) {
	const head = ( await run( 'git', [ 'rev-parse', 'HEAD' ], { cwd: root } ) ).stdout.trim();
	if ( head !== contract.source.revision ) {
		throw new StageError( 'source', 'source_moved', `Component checkout is at ${ head }, not the planned ${ contract.source.revision }.` );
	}
	const status = ( await run( 'git', [ 'status', '--porcelain' ], { cwd: root } ) ).stdout;
	if ( status.trim() !== '' ) {
		throw new StageError( 'source', 'source_dirty', 'Component checkout has uncommitted changes.' );
	}
}

async function verifyFreshness( contract, root ) {
	const { branch } = contract.policy;
	const config = Object.entries( contract.target.freshness_git_config ).flatMap( ( [ key, value ] ) => [ '-c', `${ key }=${ value }` ] );
	const output = await run( 'git', [ ...config, 'ls-remote', contract.target.freshness_remote, `refs/heads/${ branch }` ], { cwd: root, timeout: 120000 } );
	const tip = output.stdout.split( /\s+/ )[ 0 ];
	if ( ! SHA.test( tip || '' ) ) {
		throw new StageError( 'freshness', 'branch_tip_unknown', `Could not read the tip of ${ branch } from the freshness remote.` );
	}
	if ( tip !== contract.source.revision ) {
		throw new StageError(
			'freshness',
			'source_not_branch_tip',
			`Source ${ contract.source.revision } is not the tip of ${ branch } (${ tip }).`,
			[ `Fast-forward the component checkout to ${ branch } and retry, so the deploy never rolls back newer commits.` ]
		);
	}
	return { branch, tip };
}

async function remotePreflight( contract ) {
	const { ssh_host: host, remote_path: path } = contract.target;
	const status = await ssh( contract, `cd ${ quote( path ) } && git status --porcelain` );
	if ( status.stdout.trim() !== '' ) {
		const listing = boundedPaths( status.stdout );
		const error = new StageError(
			'remote_preflight',
			'remote_checkout_dirty',
			`The remote checkout has local changes: ${ listing.text }`,
			[ `Inspect ${ path } on ${ host } and commit, stash, or discard those changes deliberately before deploying.` ]
		);
		// Keep the evidence bounded and limited to porcelain path entries.
		error.paths = listing.paths;
		throw error;
	}
	if ( contract.policy.running_probe ) {
		const probe = await ssh( contract, `pgrep -af ${ quote( contract.policy.running_probe ) } | grep -v pgrep || true` );
		if ( probe.stdout.trim() !== '' ) {
			throw new StageError(
				'remote_preflight',
				'deploy_in_progress',
				'A deploy from this remote checkout is already running.',
				[ 'Wait for the running deploy to finish; do not start a second one.' ]
			);
		}
	}
}

async function syncRemote( contract, root ) {
	const { ssh_host: host, remote_path: path } = contract.target;
	const revision = contract.source.revision;
	await run( 'git', [ 'push', '--quiet', '--force', `ssh://${ host }${ path }`, `${ revision }:${ contract.policy.deploy_ref }` ], {
		cwd: root,
		timeout: contract.policy.timeout_ms,
		env: { GIT_SSH_COMMAND: 'ssh -o BatchMode=yes' },
	} );
	await ssh( contract, `cd ${ quote( path ) } && git -c advice.detachedHead=false checkout --quiet ${ revision }` );
	await assertRemoteAt( contract, 'sync' );
	return { deploy_ref: contract.policy.deploy_ref };
}

async function assertRemoteAt( contract, stageId ) {
	const { ssh_host: host, remote_path: path } = contract.target;
	const state = await ssh( contract, `cd ${ quote( path ) } && git rev-parse HEAD && git status --porcelain` );
	const [ head, ...changes ] = state.stdout.trim().split( '\n' );
	const status = changes.filter( ( line ) => line.trim() !== '' ).join( '\n' );
	if ( head !== contract.source.revision || status !== '' ) {
		const listing = boundedPaths( status );
		const details = [
			head !== contract.source.revision ? `HEAD is ${ head }, expected ${ contract.source.revision }` : null,
			status !== '' ? `local changes: ${ listing.text }` : null,
		].filter( Boolean ).join( '; ' );
		const error = new StageError( stageId, 'remote_not_at_revision', `The remote checkout is not a clean checkout of the source revision (${ details }).` );
		if ( status !== '' ) {
			error.paths = listing.paths;
			error.remediation = [ `Inspect ${ path } on ${ host } and commit, stash, or discard those changes deliberately before deploying.` ];
		}
		throw error;
	}
}

async function deploy( contract, result ) {
	const { attempts, delay_ms: delay, busy, success } = contract.policy;
	for ( let attempt = 1; attempt <= attempts; attempt++ ) {
		if ( attempt > 1 ) {
			await assertRemoteAt( contract, 'deploy' );
		}
		let output;
		try {
			output = await ssh( contract, `bash -lc ${ quote( `shopt -s expand_aliases; eval ${ quote( contract.policy.deploy_command ) }` ) }`, {
				timeout: contract.policy.timeout_ms,
				allowFailure: true,
			} );
		} catch ( error ) {
			throw new StageError(
				'deploy',
				'connection_lost',
				`The deploy command did not finish: ${ error.message }`,
				[ 'The deploy may still be running remotely. Check the running-deploy probe before retrying; never start a second deploy.' ]
			);
		}
		const text = `${ output.stdout }\n${ output.stderr }`;
		const done = text.match( success );
		if ( done ) {
			result.deploy = { attempt, ...( done.groups || {} ) };
			return { attempts: attempt };
		}
		const busyMatch = busy ? text.match( busy ) : null;
		if ( busyMatch && attempt < attempts ) {
			result.lock_waits.push( { attempt, holder: busyMatch.groups?.holder ?? null, waited_ms: delay } );
			await new Promise( ( resume ) => setTimeout( resume, delay ) );
			continue;
		}
		throw new StageError(
			'deploy',
			busyMatch ? 'deploy_lock_busy' : 'deploy_failed',
			busyMatch
				? `The deploy lock was still held after ${ attempts } attempts.`
				: `The deploy command did not report success (exit ${ output.code }). Last output:\n${ tail( text ) }`,
			busyMatch ? [ 'Retry later; the shared deploy lock is held by another deploy.' ] : []
		);
	}
}

function ssh( contract, command, options = {} ) {
	return run( 'ssh', [ '-o', 'BatchMode=yes', contract.target.ssh_host, command ], { timeout: 300000, ...options } );
}

function run( command, args, { cwd, timeout = 60000, env = {}, allowFailure = false } = {} ) {
	return new Promise( ( resolveRun, rejectRun ) => {
		const child = spawn( command, args, { cwd, env: { ...process.env, ...env }, stdio: [ 'ignore', 'pipe', 'pipe' ] } );
		let stdout = '';
		let stderr = '';
		const timer = setTimeout( () => child.kill( 'SIGTERM' ), timeout );
		child.stdout.on( 'data', ( chunk ) => ( stdout += chunk ) );
		child.stderr.on( 'data', ( chunk ) => ( stderr += chunk ) );
		child.on( 'error', ( error ) => {
			clearTimeout( timer );
			rejectRun( error );
		} );
		child.on( 'close', ( code, signal ) => {
			clearTimeout( timer );
			// 255 is ssh's own connection failure, never a remote command's result.
			if ( signal || ( command === 'ssh' && code === 255 ) ) {
				rejectRun( new Error( signal ? `${ command } timed out` : `ssh connection failed: ${ tail( stderr, 3 ) }` ) );
				return;
			}
			if ( code !== 0 && ! allowFailure ) {
				rejectRun( new Error( `${ command } exited ${ code }: ${ tail( stderr || stdout, 3 ) }` ) );
				return;
			}
			resolveRun( { code, stdout, stderr } );
		} );
	} );
}

function tail( text, lines = OUTPUT_TAIL_LINES ) {
	return String( text ).trimEnd().split( '\n' ).slice( -lines ).join( '\n' );
}

function boundedPaths( status ) {
	const entries = String( status ).split( '\n' ).map( ( line ) => line.trimEnd() ).filter( ( line ) => line.trim() !== '' );
	const paths = entries.slice( 0, MAX_PATHS );
	const remaining = entries.length - paths.length;
	return {
		paths,
		text: `${ paths.join( ', ' ) }${ remaining > 0 ? ` (and ${ remaining } more)` : '' }`,
	};
}

function quote( value ) {
	return `'${ String( value ).replaceAll( "'", `'\\''` ) }'`;
}

function regex( value, key ) {
	if ( value === undefined || value === null ) {
		return null;
	}
	try {
		return new RegExp( value );
	} catch {
		throw new Error( `policy.${ key } is not a valid regular expression.` );
	}
}

function required( object, key, pattern ) {
	const value = requiredString( object, key );
	if ( ! pattern.test( value ) ) {
		throw new Error( `${ key } has an unsupported value.` );
	}
	return value;
}

function requiredString( object, key ) {
	if ( typeof object[ key ] !== 'string' || object[ key ].trim() === '' ) {
		throw new Error( `${ key } is required.` );
	}
	return object[ key ];
}

function positiveInteger( value, key ) {
	if ( ! Number.isInteger( value ) || value < 1 ) {
		throw new Error( `${ key } must be a positive integer.` );
	}
	return value;
}

function assertKeys( object, allowed, label ) {
	const unknown = Object.keys( object ).filter( ( key ) => ! allowed.includes( key ) );
	if ( unknown.length ) {
		throw new Error( `Unknown ${ label } field(s): ${ unknown.join( ', ' ) }.` );
	}
}

function isObject( value ) {
	return value !== null && typeof value === 'object' && ! Array.isArray( value );
}

function canonicalJson( value ) {
	if ( Array.isArray( value ) ) {
		return `[${ value.map( canonicalJson ).join( ',' ) }]`;
	}
	if ( isObject( value ) ) {
		return `{${ Object.keys( value ).sort().map( ( key ) => `${ JSON.stringify( key ) }:${ canonicalJson( value[ key ] ) }` ).join( ',' ) }}`;
	}
	return JSON.stringify( value );
}

function requiredArgument( name ) {
	const index = process.argv.indexOf( name );
	if ( index === -1 || ! process.argv[ index + 1 ] ) {
		throw new Error( `${ name } is required.` );
	}
	return process.argv[ index + 1 ];
}

main().catch( ( error ) => {
	process.stdout.write( `${ JSON.stringify( { schema: SCHEMA, status: 'failed', failure: { stage: 'input', code: 'invalid_input', message: error.message } }, null, 2 ) }\n` );
	process.exitCode = 1;
} );
