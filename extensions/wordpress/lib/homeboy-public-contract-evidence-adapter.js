'use strict';

const HOMEBOY_PUBLIC_CONTRACT_EVIDENCE_SCHEMA = 'homeboy/public-contract-evidence/v1';
const HOMEBOY_FINALIZATION_EVIDENCE_POLICY_SCHEMA = 'homeboy/finalization-evidence-policy/v1';

function createHomeboyPublicContractEvidence(input = {}) {
	const changedPublicContracts = Array.isArray(input.changed_public_contracts)
		? input.changed_public_contracts
		: [];
	if (changedPublicContracts.length === 0) {
		return null;
	}

	return {
		schema: HOMEBOY_PUBLIC_CONTRACT_EVIDENCE_SCHEMA,
		changed_public_contracts: changedPublicContracts,
		required_evidence: [
			{
				role: 'compatibility-impact',
				reviewer_facing: true,
				durable_url_required: true,
				fields: [{ name: 'statement', min_length: 1 }],
			},
			{
				role: 'external-consumer-impact',
				reviewer_facing: true,
				durable_url_required: true,
				fields: [{ name: 'statement', min_length: 1 }],
			},
			{
				role: 'external-usage',
				reviewer_facing: true,
				durable_url_required: true,
				fields: [
					{ name: 'status', values: ['completed', 'unavailable_manual_review'] },
					{ name: 'source', min_length: 1 },
					{ name: 'limitations', min_length: 1 },
				],
			},
		],
		testing_instructions: numberedTestingInstructions(input.testing_instructions),
	};
}

function toFinalizationEvidencePolicy(contract) {
	if (!contract || contract.schema !== HOMEBOY_PUBLIC_CONTRACT_EVIDENCE_SCHEMA) {
		return null;
	}

	return {
		schema: HOMEBOY_FINALIZATION_EVIDENCE_POLICY_SCHEMA,
		changed_public_contracts: contract.changed_public_contracts,
		required_evidence: contract.required_evidence,
		testing_instructions: contract.testing_instructions,
	};
}

function numberedTestingInstructions(instructions) {
	return (Array.isArray(instructions) ? instructions : []).flatMap((instruction) => {
		const command = typeof instruction?.command === 'string' ? instruction.command.trim() : '';
		const number = instruction?.number;
		return command && Number.isInteger(number) && number > 0 ? [{ number, command }] : [];
	});
}

module.exports = {
	HOMEBOY_FINALIZATION_EVIDENCE_POLICY_SCHEMA,
	HOMEBOY_PUBLIC_CONTRACT_EVIDENCE_SCHEMA,
	createHomeboyPublicContractEvidence,
	toFinalizationEvidencePolicy,
};
