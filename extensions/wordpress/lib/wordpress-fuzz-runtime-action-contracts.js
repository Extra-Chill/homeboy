'use strict';

const RANDOM_WALK_RUNTIME_CONTRACT_UNAVAILABLE_REASON = 'wp-codebox-random-walk-runtime-contract-unavailable';
const STATEFUL_SEQUENCE_RUNTIME_CONTRACT_UNAVAILABLE_REASON = 'wp-codebox-stateful-sequence-runtime-contract-unavailable';

function declaredOnlyRuntimeActionFields() {
	return {
		executable: false,
		execution_tier: 'plan_only',
		planned: true,
		declared_only: true,
	};
}

module.exports = {
	RANDOM_WALK_RUNTIME_CONTRACT_UNAVAILABLE_REASON,
	STATEFUL_SEQUENCE_RUNTIME_CONTRACT_UNAVAILABLE_REASON,
	declaredOnlyRuntimeActionFields,
};
