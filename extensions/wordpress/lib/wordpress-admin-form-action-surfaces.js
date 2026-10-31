'use strict';

/**
 * Internal dependencies
 */
const { assertPlainObject, isPlainObject } = require('./shared');

const WORDPRESS_ADMIN_FORM_ACTION_SURFACE_DISCOVERY_SCHEMA = 'homeboy/wordpress-admin-form-action-surface-discovery/v1';

const DESTRUCTIVE_ACTION_TERMS = new Set([
	'delete',
	'destroy',
	'disable',
	'drop',
	'remove',
	'reset',
	'restore',
	'trash',
	'uninstall',
]);

function normalizeWordPressAdminFormActionSurfaceDiscovery(input = {}, options = {}) {
	assertPlainObject(input, 'input');
	assertPlainObject(options, 'options');
	const pages = arrayFrom(input.admin_pages || input.adminPages || input.pages);
	const surfaces = [
		...pages.map((page, index) => adminPageSurface(page, index)),
		...arrayFrom(input.ajax_actions || input.ajaxActions || input.ajax).map((action, index) => ajaxActionSurface(action, index)),
		...arrayFrom(input.admin_post_actions || input.adminPostActions || input.admin_post || input.adminPost).map((action, index) => adminPostSurface(action, index)),
	].filter(Boolean);

	return {
		schema: WORDPRESS_ADMIN_FORM_ACTION_SURFACE_DISCOVERY_SCHEMA,
		id: input.id || input.discovery_id || input.discoveryId || 'wordpress-admin-form-action-surfaces',
		label: input.label || 'WordPress admin form/action surfaces',
		surfaces,
		totals: surfaces.reduce((totals, surface) => {
			totals.total += 1;
			totals.by_type[surface.type] = (totals.by_type[surface.type] || 0) + 1;
			if (surface.destructive === true || reasonList(surface.destructive_reasons).length > 0) {
				totals.destructive += 1;
			}
			return totals;
		}, { total: 0, destructive: 0, by_type: {} }),
		metadata: {
			...(isPlainObject(input.metadata) ? input.metadata : {}),
			source_schema: input.schema,
			mapper: 'homeboy/wordpress-admin-form-action-surfaces/v1',
		},
	};
}

function adminPageSurface(page, index) {
	if (!isPlainObject(page)) {
		return undefined;
	}
	const path = page.path || page.url || page.href || adminPagePath(page);
	const forms = arrayFrom(page.forms).map((form, formIndex) => adminFormInteraction(form, formIndex));
	const actions = [
		...arrayFrom(page.actions || page.controls).map((action, actionIndex) => adminControlInteraction(action, actionIndex)),
		...bulkActionInteractions(page.list_table || page.listTable || page.bulk_actions || page.bulkActions),
	].filter(Boolean);

	return stripUndefined({
		id: page.id || page.page_hook || page.pageHook || page.menu_slug || page.menuSlug || `admin-page-${index + 1}`,
		label: page.label || page.title || page.menu_title || page.menuTitle,
		type: 'admin-page',
		path,
		method: normalizeMethod(page.method || 'GET'),
		capability: page.capability,
		forms,
		actions,
		destructive: page.destructive === true,
		destructive_reasons: destructiveReasons(page, { action: page.action }),
		metadata: {
			admin_page: stripUndefined({
				page_hook: page.page_hook || page.pageHook,
				menu_slug: page.menu_slug || page.menuSlug,
				parent_slug: page.parent_slug || page.parentSlug,
			}),
		},
	});
}

function ajaxActionSurface(action, index) {
	const descriptor = descriptorObject(action, index, 'ajax-action');
	const actionName = descriptor.action || descriptor.name || descriptor.hook || descriptor.id;
	return stripUndefined({
		id: descriptor.id || `ajax:${actionName || index + 1}`,
		label: descriptor.label,
		type: 'ajax-action',
		path: '/wp-admin/admin-ajax.php',
		method: normalizeMethod(descriptor.method || 'POST'),
		action: actionName,
		capability: descriptor.capability,
		destructive: descriptor.destructive === true,
		destructive_reasons: destructiveReasons(descriptor, { action: actionName }),
		nonce_context: nonceContext(descriptor),
		input_descriptors: inputDescriptors(descriptor),
	});
}

function adminPostSurface(action, index) {
	const descriptor = descriptorObject(action, index, 'admin-post');
	const actionName = descriptor.action || descriptor.name || descriptor.hook || descriptor.id;
	return stripUndefined({
		id: descriptor.id || `admin-post:${actionName || index + 1}`,
		label: descriptor.label,
		type: 'admin-page',
		path: '/wp-admin/admin-post.php',
		method: normalizeMethod(descriptor.method || 'POST'),
		action: actionName,
		capability: descriptor.capability,
		destructive: descriptor.destructive === true,
		destructive_reasons: destructiveReasons(descriptor, { action: actionName }),
		nonce_context: nonceContext(descriptor),
		input_descriptors: inputDescriptors(descriptor),
	});
}

function adminFormInteraction(form, index) {
	const descriptor = descriptorObject(form, index, 'form');
	const actionPath = descriptor.action_path || descriptor.actionPath || descriptor.action_url || descriptor.actionUrl || descriptor.action;
	const submitControls = submitControlDescriptors(descriptor);
	return stripUndefined({
		id: descriptor.id || descriptor.name || `form-${index + 1}`,
		name: descriptor.name,
		selector: descriptor.selector,
		method: normalizeMethod(descriptor.method || 'POST'),
		action: descriptor.action_name || descriptor.actionName || descriptor.admin_post_action || descriptor.adminPostAction || descriptor.ajax_action || descriptor.ajaxAction,
		action_path: actionPath,
		fields: inputDescriptors(descriptor),
		input_descriptors: inputDescriptors(descriptor),
		submit_controls: submitControls,
		capability: descriptor.capability,
		capability_context: capabilityContext(descriptor),
		nonce_context: nonceContext(descriptor),
		safety: safetyDescriptor(descriptor, submitControls),
	});
}

function adminControlInteraction(action, index) {
	const descriptor = descriptorObject(action, index, 'action');
	return stripUndefined({
		id: descriptor.id || descriptor.name || descriptor.action || `action-${index + 1}`,
		name: descriptor.name,
		selector: descriptor.selector,
		method: normalizeMethod(descriptor.method || 'GET'),
		action: descriptor.action || descriptor.value,
		action_path: descriptor.action_path || descriptor.actionPath || descriptor.href || descriptor.url,
		fields: inputDescriptors(descriptor),
		input_descriptors: inputDescriptors(descriptor),
		submit_controls: submitControlDescriptors(descriptor),
		capability: descriptor.capability,
		capability_context: capabilityContext(descriptor),
		nonce_context: nonceContext(descriptor),
		safety: safetyDescriptor(descriptor, submitControlDescriptors(descriptor)),
	});
}

function bulkActionInteractions(input) {
	const listTable = isPlainObject(input) ? input : { bulk_actions: input };
	return arrayFrom(listTable.bulk_actions || listTable.bulkActions || listTable.actions).map((action, index) => {
		const descriptor = descriptorObject(action, index, 'bulk-action');
		return stripUndefined({
			id: descriptor.id || descriptor.value || descriptor.action || `bulk-action-${index + 1}`,
			name: descriptor.name,
			method: normalizeMethod(descriptor.method || 'POST'),
			action: descriptor.action || descriptor.value || descriptor.id,
			action_path: descriptor.action_path || descriptor.actionPath,
			bulk_action: stripUndefined({ value: descriptor.value || descriptor.action || descriptor.id, label: descriptor.label }),
			capability: descriptor.capability || listTable.capability,
			capability_context: capabilityContext({ ...listTable, ...descriptor }),
			nonce_context: nonceContext(descriptor) || nonceContext(listTable),
			safety: safetyDescriptor(descriptor),
		});
	});
}

function adminPagePath(page) {
	if (page.menu_slug || page.menuSlug) {
		return `/wp-admin/admin.php?page=${encodeURIComponent(page.menu_slug || page.menuSlug)}`;
	}
	return '/wp-admin/admin.php';
}

function safetyDescriptor(descriptor, submitControls = []) {
	const reasons = destructiveReasons(descriptor, { action: descriptor.action || descriptor.value, submitControls });
	const method = normalizeMethod(descriptor.method || 'GET');
	const mutates = descriptor.mutates === true || descriptor.destructive === true || !['GET', 'HEAD'].includes(method);
	let level = 'safe';
	if (reasons.length > 0) {
		level = 'destructive';
	} else if (mutates) {
		level = 'mutating';
	}
	return stripUndefined({
		level,
		mutates,
		rollback_required: mutates,
		reason_codes: reasons,
	});
}

function destructiveReasons(descriptor, context = {}) {
	const declared = reasonList(descriptor.destructive_reasons || descriptor.destructiveReasons || descriptor.destructive_reason || descriptor.destructiveReason);
	const terms = [descriptor.id, descriptor.name, descriptor.value, context.action, ...arrayFrom(context.submitControls).flatMap((control) => [control.name, control.value, control.action])];
	const inferred = terms.some((term) => DESTRUCTIVE_ACTION_TERMS.has(normalizeToken(term))) ? ['destructive_action_term'] : [];
	return [...new Set([...(descriptor.destructive === true ? ['declared_destructive'] : []), ...declared, ...inferred])].sort();
}

function nonceContext(descriptor) {
	const nonce = descriptor.nonce_context || descriptor.nonceContext || descriptor.nonce;
	if (isPlainObject(nonce)) {
		return stripUndefined({
			required: nonce.required !== false,
			action: nonce.action || nonce.nonce_action || nonce.nonceAction,
			field: nonce.field || nonce.name || nonce.nonce_field || nonce.nonceField || '_wpnonce',
		});
	}
	if (descriptor.nonce_action || descriptor.nonceAction || descriptor.nonce_field || descriptor.nonceField) {
		return stripUndefined({
			required: true,
			action: descriptor.nonce_action || descriptor.nonceAction,
			field: descriptor.nonce_field || descriptor.nonceField || '_wpnonce',
		});
	}
	return undefined;
}

function capabilityContext(descriptor) {
	const context = descriptor.capability_context || descriptor.capabilityContext;
	if (isPlainObject(context)) {
		return context;
	}
	return descriptor.capability ? { required: [String(descriptor.capability)] } : undefined;
}

function inputDescriptors(descriptor) {
	return arrayFrom(descriptor.inputs || descriptor.input_descriptors || descriptor.inputDescriptors || descriptor.fields).map((input, index) => {
		const item = descriptorObject(input, index, 'input');
		return stripUndefined({
			name: item.name || item.id,
			type: item.type,
			required: item.required === true,
			value: item.value,
			default: item.default,
			options: item.options,
		});
	});
}

function submitControlDescriptors(descriptor) {
	return arrayFrom(descriptor.submit_controls || descriptor.submitControls || descriptor.submits).map((control, index) => {
		const item = descriptorObject(control, index, 'submit');
		return stripUndefined({
			name: item.name || item.id,
			value: item.value,
			label: item.label,
			action: item.action,
			destructive: item.destructive === true,
		});
	});
}

function descriptorObject(value, index, prefix) {
	if (isPlainObject(value)) {
		return value;
	}
	return { id: `${prefix}-${index + 1}`, value: value === undefined || value === null ? undefined : String(value) };
}

function normalizeMethod(value) {
	return String(value || 'GET').trim().toUpperCase();
}

function normalizeToken(value) {
	return String(value || '').trim().toLowerCase().replace(/[^a-z0-9]+/g, '_').replace(/^_+|_+$/g, '');
}

function reasonList(value) {
	if (value === undefined || value === null) {
		return [];
	}
	return [...new Set((Array.isArray(value) ? value : [value]).map(String).filter(Boolean))].sort();
}

function arrayFrom(value) {
	return Array.isArray(value) ? value : [];
}

function stripUndefined(value) {
	return Object.fromEntries(Object.entries(value).filter(([, item]) => item !== undefined));
}

module.exports = {
	WORDPRESS_ADMIN_FORM_ACTION_SURFACE_DISCOVERY_SCHEMA,
	normalizeWordPressAdminFormActionSurfaceDiscovery,
};
