'use strict';

const assert = require('node:assert/strict');

const {
	HOMEBOY_FINALIZATION_EVIDENCE_POLICY_SCHEMA,
	HOMEBOY_PUBLIC_CONTRACT_EVIDENCE_SCHEMA,
	createHomeboyPublicContractEvidence,
	toFinalizationEvidencePolicy,
} = require('../lib/homeboy-public-contract-evidence-adapter');

assert.equal(createHomeboyPublicContractEvidence(), null);

const contract = createHomeboyPublicContractEvidence({
	changed_public_contracts: [{ id: 'rest:/example/v1/items', type: 'rest_route' }],
	testing_instructions: [
		{ number: 4, command: 'npm run test:api -- --filter items' },
		{ number: 7, command: 'npm run test:e2e -- --spec cart' },
	],
});

assert.equal(contract.schema, HOMEBOY_PUBLIC_CONTRACT_EVIDENCE_SCHEMA);
assert.deepEqual(contract.changed_public_contracts, [{ id: 'rest:/example/v1/items', type: 'rest_route' }]);
assert.deepEqual(contract.required_evidence.map((evidence) => evidence.role), [
	'compatibility-impact',
	'external-consumer-impact',
	'external-usage',
]);
assert.deepEqual(contract.testing_instructions, [
	{ number: 4, command: 'npm run test:api -- --filter items' },
	{ number: 7, command: 'npm run test:e2e -- --spec cart' },
]);

assert.deepEqual(createHomeboyPublicContractEvidence({
	changed_public_contracts: [{ id: 'rest:/example/v1/items', summary: 'The item collection response changed.' }],
}).testing_instructions, []);

const policy = toFinalizationEvidencePolicy(contract);
assert.equal(policy.schema, HOMEBOY_FINALIZATION_EVIDENCE_POLICY_SCHEMA);
assert.deepEqual(policy.changed_public_contracts, contract.changed_public_contracts);
assert.deepEqual(policy.required_evidence, contract.required_evidence);
assert.deepEqual(policy.testing_instructions, contract.testing_instructions);
assert.equal(toFinalizationEvidencePolicy({ schema: 'other/schema' }), null);

console.log('Homeboy public contract evidence adapter smoke passed.');
