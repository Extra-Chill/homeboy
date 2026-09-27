#!/usr/bin/env node
// End-to-end coverage with real git repositories. A fake `ssh` on PATH runs each
// remote command locally, so the push, remote checkout, lock retry, and deploy
// stub all execute for real against a bare origin, a component checkout, and a
// "remote" checkout.

import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import { join, resolve } from 'node:path';

const script = resolve( import.meta.dirname, '../scripts/ssh-checkout-deploy.mjs' );
const root = mkdtempSync( join( os.tmpdir(), 'ssh-checkout-deploy-test-' ) );
const bin = join( root, 'bin' );
const origin = join( root, 'origin.git' );
const local = join( root, 'local' );
const remote = join( root, 'remote' );
const attemptsFile = join( root, 'deploy-attempts' );
mkdirSync( bin );

const git = ( cwd, ...args ) => {
	const out = spawnSync( 'git', args, { cwd, encoding: 'utf8' } );
	assert.equal( out.status, 0, `git ${ args.join( ' ' ) }: ${ out.stderr }` );
	return out.stdout.trim();
};
const identity = [ '-c', 'user.email=test@example.test', '-c', 'user.name=Test' ];

git( root, 'init', '--quiet', '--bare', '--initial-branch=main', origin );
git( root, 'clone', '--quiet', origin, local );
writeFileSync( join( local, 'app.txt' ), 'v1\n' );
git( local, 'add', '.' );
git( local, ...identity, 'commit', '--quiet', '-m', 'v1' );
git( local, 'push', '--quiet', 'origin', 'HEAD:main' );
git( root, 'clone', '--quiet', origin, remote );
const revision = git( local, 'rev-parse', 'HEAD' );

// Fake ssh: drop options and the host, then run the remote command locally.
writeFileSync( join( bin, 'ssh' ), `#!/bin/bash
while [ "\${1#-}" != "$1" ]; do [ "$1" = "-o" ] && shift; shift; done
shift
if [ -n "$SSH_DROP_DEPLOY" ] && [[ "$*" == *"bash -lc"* ]]; then echo "Connection reset" >&2; exit 255; fi
exec bash -c "$*"
` );
chmodSync( join( bin, 'ssh' ), 0o755 );

// Deploy stub: busy for the first BUSY_ATTEMPTS calls, then report success.
const deployStub = join( bin, 'deploy-stub' );
writeFileSync( deployStub, `#!/bin/bash
n=$(( $(cat "${ attemptsFile }" 2>/dev/null || echo 0) + 1 ))
echo $n > "${ attemptsFile }"
if [ -n "$DEPLOY_FAIL" ]; then echo "rsync: connection unexpectedly closed"; exit 12; fi
if [ "$n" -le "\${BUSY_ATTEMPTS:-0}" ]; then echo "Deploy lock held by alice (try again later)"; exit 1; fi
echo "Revision 4242 successfully deployed to 12 servers in 7 seconds"
` );
chmodSync( deployStub, 0o755 );

const policy = {
	branch: 'main',
	deploy_command: deployStub,
	busy_pattern: 'Deploy lock held by (?<holder>\\S+)',
	success_pattern: 'Revision (?<revision>\\d+) successfully deployed to (?<servers>\\d+) servers in (?<seconds>\\d+) seconds',
	lock_retry: { attempts: 3, delay_ms: 1 },
	timeout_ms: 60000,
};
const target = { ssh_host: 'build-host', remote_path: remote, freshness_remote: origin };

function payload( overrides = {} ) {
	const value = overrides.policy ?? policy;
	return {
		schema: 'homeboy/deployment-provider-payload/v1',
		policy: {
			value,
			reference: {
				component: 'app',
				path: 'homeboy.json#/deployment_provider/policy',
				digest: overrides.digest ?? createHash( 'sha256' ).update( canonical( value ) ).digest( 'hex' ),
			},
		},
		target: overrides.target ?? target,
		source: { component: 'app', revision: overrides.revision ?? revision },
	};
}

async function provider( body, { env = {}, args = [] } = {} ) {
	rmSync( attemptsFile, { force: true } );
	const contract = join( root, 'contract.json' );
	writeFileSync( contract, JSON.stringify( body ) );
	return new Promise( ( done ) => {
		const child = spawn( process.execPath, [ script, ...args, '--contract', contract ], {
			env: { ...process.env, PATH: `${ bin }:${ process.env.PATH }`, HOMEBOY_COMPONENT_PATH: local, ...env },
		} );
		let stdout = '';
		child.stdout.on( 'data', ( chunk ) => ( stdout += chunk ) );
		child.on( 'close', ( code ) => done( { code, result: JSON.parse( stdout ), attempts: existsSync( attemptsFile ) ? Number( readFileSync( attemptsFile, 'utf8' ) ) : 0 } ) );
	} );
}

const remoteRef = () => spawnSync( 'git', [ 'rev-parse', '--verify', '--quiet', 'refs/heads/homeboy-deploy' ], { cwd: remote, encoding: 'utf8' } ).stdout.trim();
const reset = () => {
	spawnSync( 'git', [ 'checkout', '--quiet', 'main' ], { cwd: remote } );
	spawnSync( 'git', [ 'branch', '--quiet', '-D', 'homeboy-deploy' ], { cwd: remote } );
};

// Dry run validates source, freshness, and the remote without pushing or deploying.
{
	const { code, result, attempts } = await provider( payload(), { args: [ '--dry-run' ] } );
	assert.equal( code, 0, JSON.stringify( result ) );
	assert.equal( result.status, 'validated' );
	assert.deepEqual( result.stages.map( ( s ) => s.id ), [ 'source', 'freshness', 'remote_preflight' ] );
	assert.equal( remoteRef(), '', 'dry run must not push' );
	assert.equal( attempts, 0, 'dry run must not deploy' );
}

// Apply waits out one busy lock, deploys the exact revision, and records evidence.
{
	const { code, result, attempts } = await provider( payload(), { env: { BUSY_ATTEMPTS: '1' } } );
	assert.equal( code, 0, JSON.stringify( result ) );
	assert.equal( result.status, 'succeeded' );
	assert.equal( attempts, 2 );
	assert.deepEqual( result.lock_waits.map( ( w ) => w.holder ), [ 'alice' ] );
	assert.deepEqual( { revision: result.deploy.revision, servers: result.deploy.servers, seconds: result.deploy.seconds }, { revision: '4242', servers: '12', seconds: '7' } );
	assert.equal( git( remote, 'rev-parse', 'HEAD' ), revision );
	assert.equal( remoteRef(), revision );
	reset();
}

// A held lock that never clears fails without claiming success.
{
	const { code, result, attempts } = await provider( payload(), { env: { BUSY_ATTEMPTS: '99' } } );
	assert.equal( code, 1 );
	assert.equal( result.failure.code, 'deploy_lock_busy' );
	assert.equal( attempts, 3 );
	reset();
}

// A deploy command that reports no success fails with its output tail.
{
	const { result } = await provider( payload(), { env: { DEPLOY_FAIL: '1' } } );
	assert.equal( result.failure.code, 'deploy_failed' );
	assert.match( result.failure.message, /connection unexpectedly closed/ );
	reset();
}

// A dropped connection during deploy fails closed and is not retried.
{
	const { result } = await provider( payload(), { env: { SSH_DROP_DEPLOY: '1', BUSY_ATTEMPTS: '99' } } );
	assert.equal( result.failure.code, 'connection_lost' );
	assert.match( result.remediation.join( ' ' ), /never start a second deploy/ );
	reset();
}

// A dirty remote checkout is refused before anything is pushed or deployed.
{
	writeFileSync( join( remote, 'leftover.txt' ), 'uncommitted\n' );
	const { result, attempts } = await provider( payload() );
	assert.equal( result.failure.code, 'remote_checkout_dirty' );
	assert.equal( remoteRef(), '' );
	assert.equal( attempts, 0 );
	rmSync( join( remote, 'leftover.txt' ) );
}

// A running deploy on the remote blocks a second one.
{
	const marker = `ssh-checkout-deploy-test-${ process.pid }`;
	const sleeper = spawn( process.execPath, [ '-e', 'setTimeout(() => {}, 30000)', marker ] );
	const { result } = await provider( payload( { policy: { ...policy, running_probe: marker } } ) );
	sleeper.kill();
	assert.equal( result.failure.code, 'deploy_in_progress' );
	assert.equal( remoteRef(), '' );
}

// A source that is not the branch tip is refused, so a deploy never rolls back newer commits.
{
	const ahead = join( root, 'ahead' );
	git( root, 'clone', '--quiet', origin, ahead );
	writeFileSync( join( ahead, 'app.txt' ), 'v2\n' );
	git( ahead, ...identity, 'commit', '--quiet', '-am', 'v2' );
	git( ahead, 'push', '--quiet', 'origin', 'HEAD:main' );
	const { result } = await provider( payload() );
	assert.equal( result.failure.code, 'source_not_branch_tip' );
	assert.equal( remoteRef(), '' );
	git( local, 'pull', '--quiet', '--ff-only' );
}

// A tampered policy digest is rejected as invalid input.
{
	const { code, result } = await provider( payload( { revision: git( local, 'rev-parse', 'HEAD' ), digest: '0'.repeat( 64 ) } ) );
	assert.equal( code, 1 );
	assert.equal( result.failure.code, 'invalid_input' );
}

// A local checkout that moved away from the planned revision is refused.
{
	const { result } = await provider( payload( { revision } ) );
	assert.equal( result.failure.code, 'source_moved' );
}

rmSync( root, { recursive: true, force: true } );
console.log( 'ssh-checkout-deploy tests passed' );

function canonical( value ) {
	if ( Array.isArray( value ) ) {
		return `[${ value.map( canonical ).join( ',' ) }]`;
	}
	if ( value && typeof value === 'object' ) {
		return `{${ Object.keys( value ).sort().map( ( key ) => `${ JSON.stringify( key ) }:${ canonical( value[ key ] ) }` ).join( ',' ) }}`;
	}
	return JSON.stringify( value );
}
