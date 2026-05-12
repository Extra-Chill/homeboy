'use strict';

const WORDPRESS_RESOURCE_INCLUDE = Object.freeze([
	'/wp-json/',
	'?rest_route=',
	'/wp-admin/',
	'/wp-content/',
	'/wp-includes/',
]);

function isPlainObject(value) {
	return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function assertPlainObject(value, name) {
	if (!isPlainObject(value)) {
		throw new TypeError(`${name} must be an object`);
	}
}

function sleep(ms) {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

module.exports = {
	WORDPRESS_RESOURCE_INCLUDE,
	assertPlainObject,
	isPlainObject,
	sleep,
};
