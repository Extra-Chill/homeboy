'use strict';

function scenarioResultsFromOutcome(outcome = {}) {
	return { scenarios: outcome.metadata?.fixture?.scenarios || [] };
}

module.exports = { scenarioResultsFromOutcome };
