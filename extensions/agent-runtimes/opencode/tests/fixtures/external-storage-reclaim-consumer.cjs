'use strict';

const KEYS = ['schema', 'provider_id', 'generation', 'reclaimed_item_ids', 'reclaimed_bytes'];

function consumeReclaimReceipt(receipt) {
	if (!receipt || typeof receipt !== 'object' || Array.isArray(receipt)) throw new Error('reclaim receipt must be an object');
	const keys = Object.keys(receipt).sort();
	if (keys.join('\0') !== [...KEYS].sort().join('\0')) throw new Error('reclaim receipt contains unknown or missing fields');
	if (receipt.schema !== 'homeboy/external-storage-retention/v1' || typeof receipt.provider_id !== 'string' || typeof receipt.generation !== 'string') throw new Error('reclaim receipt identity is invalid');
	if (!Array.isArray(receipt.reclaimed_item_ids) || !Number.isSafeInteger(receipt.reclaimed_bytes) || receipt.reclaimed_bytes < 0) throw new Error('reclaim receipt values are invalid');
	return receipt;
}

module.exports = { consumeReclaimReceipt };
