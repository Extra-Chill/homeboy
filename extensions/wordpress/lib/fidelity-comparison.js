'use strict';

/* eslint-disable no-bitwise */

/**
 * External dependencies
 */
const { readFile, writeFile } = require('node:fs/promises');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const zlib = require('node:zlib');

const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

function asArray(value) {
	return Array.isArray(value) ? value : [];
}

function stringArray(value) {
	return asArray(value).filter((item) => typeof item === 'string' && item.trim() !== '');
}

function safeSlug(value, fallback) {
	const slug = String(value || fallback || 'target')
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, '-')
		.replace(/^-+|-+$/g, '')
		.slice(0, 80);
	return slug || 'target';
}

function comparisonTargets(importReport) {
	return asArray(importReport?.report?.visual_fidelity?.comparison_targets).filter((target) => target && typeof target === 'object');
}

function semanticComparisonTargets(importReport) {
	const semanticTargets = asArray(importReport?.report?.semantic_fidelity?.comparison_targets).filter((target) => target && typeof target === 'object');
	return semanticTargets.length ? semanticTargets : comparisonTargets(importReport);
}

function resolveSourceStaticFile(sourceFile, reportPath, sitePath) {
	if (!sourceFile) {
		return '';
	}
	if (path.isAbsolute(sourceFile)) {
		const wordpressRoot = '/wordpress';
		if (sitePath && (sourceFile === wordpressRoot || sourceFile.startsWith(`${wordpressRoot}/`))) {
			return path.join(sitePath, sourceFile.slice(wordpressRoot.length));
		}
		return sourceFile;
	}
	return path.resolve(path.dirname(reportPath), sourceFile);
}

function surfaceUrl(target, surface, reportPath, sitePath) {
	const surfaces = target?.comparison_hooks?.render_surfaces || {};
	const configured = surfaces[surface]?.url || '';
	if (surface === 'source_static') {
		const sourceFile = configured || target?.source_file || '';
		return sourceFile ? pathToFileURL(resolveSourceStaticFile(sourceFile, reportPath, sitePath)).toString() : '';
	}
	if (surface === 'wordpress_frontend') {
		return configured || target?.wordpress_url || '';
	}
	if (surface === 'wordpress_editor') {
		if (configured) {
			return configured;
		}
		const postId = Number(target?.wordpress_page_id || target?.home_page_id || target?.front_page_id || 0);
		const frontendUrl = surfaceUrl(target, 'wordpress_frontend', reportPath, sitePath);
		if (!postId || !frontendUrl) {
			return '';
		}
		const url = new URL(frontendUrl);
		url.pathname = '/studio-auto-login';
		url.search = '';
		url.searchParams.set('redirect_to', `/wp-admin/post.php?post=${postId}&action=edit`);
		return url.toString();
	}
	return configured;
}

function targetSelectorGroups(target, options = {}) {
	const hooks = target?.comparison_hooks || {};
	const layoutProbes = hooks.layout_probes && typeof hooks.layout_probes === 'object' ? hooks.layout_probes : {};
	const groups = [];
	const seen = new Set();
	function add(name, selectors) {
		const normalizedSelectors = stringArray(selectors);
		if (!normalizedSelectors.length || seen.has(name)) {
			return;
		}
		seen.add(name);
		groups.push({ name, selectors: normalizedSelectors });
	}
	for (const [name, probe] of Object.entries(layoutProbes)) {
		add(name, probe?.selectors);
	}
	add(options.heroGroupName || 'hero_probe', hooks.hero);
	add('visible_chrome', hooks.visible_chrome);
	add('footer_chrome', ['footer', '.site-footer', '[class*=footer]']);
	if (options.semantic === true) {
		add('brand_hooks', ['[class*=brand]', '[class*=logo]', '[class*=wordmark]']);
		add('interaction_hooks', ['a', 'button', '[role=button]', '[role=link]']);
	}
	return groups;
}

function visualProbeGroups(target) {
	return targetSelectorGroups(target, { heroGroupName: 'hero_probe' });
}

function semanticTargetSelectorGroups(target) {
	return targetSelectorGroups(target, { heroGroupName: 'hero', semantic: true });
}

function visualSurfaceTotals(groups) {
	return groups.reduce(
		(totals, group) => {
			totals.selector_count += Number(group.selector_count || 0);
			totals.missing_selector_count += Number(group.missing_selector_count || 0);
			totals.errored_selector_count += Number(group.errored_selector_count || 0);
			totals.matched_selector_count += Number(group.matched_selector_count || 0);
			totals.visible_selector_count += Number(group.visible_selector_count || 0);
			totals.nonzero_bounding_box_selector_count += Number(group.nonzero_bounding_box_selector_count || 0);
			return totals;
		},
		{ selector_count: 0, missing_selector_count: 0, errored_selector_count: 0, matched_selector_count: 0, visible_selector_count: 0, nonzero_bounding_box_selector_count: 0 }
	);
}

function visualMismatchReason(sourceSelector, frontendSelector) {
	if (sourceSelector?.error || frontendSelector?.error) {
		return 'selector_error';
	}
	if ((sourceSelector.visible_count === 0 && frontendSelector.visible_count === 0) || (sourceSelector.count === 0 && frontendSelector.count === 0)) {
		return 'missing_on_both_surfaces';
	}
	if (sourceSelector.count === 0) {
		return 'missing_from_source_static';
	}
	if (frontendSelector.count === 0) {
		return 'missing_from_wordpress_frontend';
	}
	const sourceVisible = sourceSelector.visible_count > 0;
	const frontendVisible = frontendSelector.visible_count > 0;
	if (sourceVisible !== frontendVisible) {
		return sourceVisible ? 'hidden_on_wordpress_frontend' : 'hidden_on_source_static';
	}
	const sourceNonzero = sourceSelector.nonzero_bounding_box_count > 0;
	const frontendNonzero = frontendSelector.nonzero_bounding_box_count > 0;
	if (sourceNonzero !== frontendNonzero) {
		return sourceNonzero ? 'zero_sized_on_wordpress_frontend' : 'zero_sized_on_source_static';
	}
	return '';
}

function visualMismatchSeverity(reason) {
	return {
		selector_error: 100,
		missing_from_wordpress_frontend: 90,
		missing_from_source_static: 80,
		hidden_on_wordpress_frontend: 60,
		hidden_on_source_static: 50,
		zero_sized_on_wordpress_frontend: 40,
		zero_sized_on_source_static: 30,
	}[reason] || 0;
}

function visualSelectorSummary(selector) {
	const firstMatch = selector?.first_match || null;
	return {
		count: Number(selector?.count || 0),
		visible_count: Number(selector?.visible_count || 0),
		nonzero_bounding_box_count: Number(selector?.nonzero_bounding_box_count || 0),
		first_bounding_box: firstMatch?.boundingBox || null,
		first_visible: firstMatch?.visible === true,
		first_visible_text: firstMatch?.text || '',
		error: selector?.error || '',
	};
}

function visualSelectorComparisonDetails(result) {
	const sourceGroups = result.surfaces?.source_static?.probes || [];
	const frontendGroups = result.surfaces?.wordpress_frontend?.probes || [];
	const frontendGroupsByName = new Map(frontendGroups.map((group) => [group.name, group]));
	const mismatches = [];
	const optionalProbeAbsences = [];
	for (const sourceGroup of sourceGroups) {
		const frontendGroup = frontendGroupsByName.get(sourceGroup.name);
		if (!frontendGroup) {
			continue;
		}
		const frontendSelectors = new Map(frontendGroup.selectors.map((selector) => [selector.selector, selector]));
		for (const sourceSelector of sourceGroup.selectors) {
			const frontendSelector = frontendSelectors.get(sourceSelector.selector);
			if (!frontendSelector) {
				continue;
			}
			const reason = visualMismatchReason(sourceSelector, frontendSelector);
			if (!reason) {
				continue;
			}
			const detail = {
				group: sourceGroup.name,
				selector: sourceSelector.selector,
				reason,
				severity: visualMismatchSeverity(reason),
				source: visualSelectorSummary(sourceSelector),
				frontend: visualSelectorSummary(frontendSelector),
				screenshots: {},
			};
			if (reason === 'missing_on_both_surfaces') {
				optionalProbeAbsences.push(detail);
			} else {
				mismatches.push(detail);
			}
		}
	}
	mismatches.sort((a, b) => b.severity - a.severity || a.group.localeCompare(b.group) || a.selector.localeCompare(b.selector));
	optionalProbeAbsences.sort((a, b) => a.group.localeCompare(b.group) || a.selector.localeCompare(b.selector));
	return { mismatches, optionalProbeAbsences };
}

function visualGroupMismatchSummary(groupName, mismatches) {
	const reasons = {};
	for (const mismatch of mismatches) {
		reasons[mismatch.reason] = (reasons[mismatch.reason] || 0) + 1;
	}
	return {
		group: groupName,
		mismatch_count: mismatches.length,
		reasons,
		top_selectors: mismatches.slice(0, 5).map((mismatch) => ({
			selector: mismatch.selector,
			reason: mismatch.reason,
			source_count: mismatch.source.count,
			frontend_count: mismatch.frontend.count,
			source_visible_count: mismatch.source.visible_count,
			frontend_visible_count: mismatch.frontend.visible_count,
		})),
	};
}

function visualParity(sourceGroups, frontendGroups) {
	const frontendByName = new Map(frontendGroups.map((group) => [group.name, group]));
	const groupComparisons = [];
	let missingSelectorCount = 0;
	let visibilityMismatchCount = 0;
	let nonzeroBoundingBoxMismatchCount = 0;
	let simpleProbeParityMismatchCount = 0;
	const simpleProbeFamilies = { nav: new Set(['nav_chrome']), footer: new Set(['footer_chrome']), hero: new Set(['hero_region', 'hero_probe']) };
	const simpleProbeNames = new Set(Object.values(simpleProbeFamilies).flatMap((names) => [...names]));
	const simpleProbeMismatches = { nav: 0, footer: 0, hero: 0 };
	for (const sourceGroup of sourceGroups) {
		const frontendGroup = frontendByName.get(sourceGroup.name);
		if (!frontendGroup) {
			continue;
		}
		const frontendSelectors = new Map(frontendGroup.selectors.map((selector) => [selector.selector, selector]));
		for (const sourceSelector of sourceGroup.selectors) {
			const frontendSelector = frontendSelectors.get(sourceSelector.selector);
			if (!frontendSelector) {
				continue;
			}
			const sourceVisible = sourceSelector.visible_count > 0;
			const frontendVisible = frontendSelector.visible_count > 0;
			const sourceNonzero = sourceSelector.nonzero_bounding_box_count > 0;
			const frontendNonzero = frontendSelector.nonzero_bounding_box_count > 0;
			if (sourceSelector.count === 0 || frontendSelector.count === 0) {
				missingSelectorCount++;
			}
			if (sourceVisible !== frontendVisible) {
				visibilityMismatchCount++;
			}
			if (sourceNonzero !== frontendNonzero) {
				nonzeroBoundingBoxMismatchCount++;
			}
		}
		const sourceGroupVisible = sourceGroup.visible_selector_count > 0;
		const frontendGroupVisible = frontendGroup.visible_selector_count > 0;
		const simpleProbeMismatch = simpleProbeNames.has(sourceGroup.name) && sourceGroupVisible !== frontendGroupVisible;
		if (simpleProbeMismatch) {
			simpleProbeParityMismatchCount++;
			for (const [family, names] of Object.entries(simpleProbeFamilies)) {
				if (names.has(sourceGroup.name)) {
					simpleProbeMismatches[family]++;
				}
			}
		}
		groupComparisons.push({
			name: sourceGroup.name,
			source_visible: sourceGroupVisible,
			frontend_visible: frontendGroupVisible,
			source_nonzero_bounding_box: sourceGroup.nonzero_bounding_box_selector_count > 0,
			frontend_nonzero_bounding_box: frontendGroup.nonzero_bounding_box_selector_count > 0,
			simple_probe_parity: simpleProbeNames.has(sourceGroup.name) ? !simpleProbeMismatch : null,
		});
	}
	return {
		missing_selector_count: missingSelectorCount,
		visibility_mismatch_count: visibilityMismatchCount,
		nonzero_bounding_box_mismatch_count: nonzeroBoundingBoxMismatchCount,
		simple_probe_parity_mismatch_count: simpleProbeParityMismatchCount,
		simple_probe_mismatches: simpleProbeMismatches,
		groups: groupComparisons,
	};
}

function buildVisualDiagnostics(results, artifactPath) {
	const allMismatches = [];
	const allOptionalProbeAbsences = [];
	const targets = [];
	for (const result of results) {
		const mismatches = asArray(result.diagnostics?.mismatches);
		const optionalProbeAbsences = asArray(result.diagnostics?.optional_probe_absences);
		const byGroup = new Map();
		for (const mismatch of mismatches) {
			if (!byGroup.has(mismatch.group)) {
				byGroup.set(mismatch.group, []);
			}
			byGroup.get(mismatch.group).push(mismatch);
			allMismatches.push({ target: result.source_filename || String(result.wordpress_page_id || ''), ...mismatch });
		}
		for (const absence of optionalProbeAbsences) {
			allOptionalProbeAbsences.push({ target: result.source_filename || String(result.wordpress_page_id || ''), ...absence });
		}
		targets.push({
			source_filename: result.source_filename || '',
			wordpress_page_id: result.wordpress_page_id || null,
			mismatch_count: mismatches.length,
			optional_probe_absent_count: optionalProbeAbsences.length,
			top_failing_groups: [...byGroup.entries()].map(([groupName, groupMismatches]) => visualGroupMismatchSummary(groupName, groupMismatches)).sort((a, b) => b.mismatch_count - a.mismatch_count || a.group.localeCompare(b.group)).slice(0, 5),
			top_failing_selectors: mismatches.slice(0, 10).map((mismatch) => ({
				group: mismatch.group,
				selector: mismatch.selector,
				reason: mismatch.reason,
				source_count: mismatch.source.count,
				frontend_count: mismatch.frontend.count,
				source_first_bounding_box: mismatch.source.first_bounding_box,
				frontend_first_bounding_box: mismatch.frontend.first_bounding_box,
				source_first_visible_text: mismatch.source.first_visible_text,
				frontend_first_visible_text: mismatch.frontend.first_visible_text,
				screenshots: mismatch.screenshots,
			})),
		});
	}
	const topFailingGroups = new Map();
	for (const mismatch of allMismatches) {
		const key = `${mismatch.target || 'target'}:${mismatch.group}`;
		if (!topFailingGroups.has(key)) {
			topFailingGroups.set(key, []);
		}
		topFailingGroups.get(key).push(mismatch);
	}
	return {
		artifact: artifactPath,
		mismatch_count: allMismatches.length,
		optional_probe_absent_count: allOptionalProbeAbsences.length,
		top_failing_groups: [...topFailingGroups.entries()].map(([, mismatches]) => ({ target: mismatches[0]?.target || '', ...visualGroupMismatchSummary(mismatches[0]?.group || '', mismatches) })).sort((a, b) => b.mismatch_count - a.mismatch_count || a.group.localeCompare(b.group)).slice(0, 10),
		targets,
		mismatches: allMismatches,
		optional_probe_absences: allOptionalProbeAbsences,
	};
}

function semanticFingerprintExtractor(groups) {
	const meaningfulHookPattern = /(brand|logo|wordmark|nav|menu|footer|header|hero|card|panel|cta|button|price|plan|feature|testimonial|avatar|badge|label|eyebrow|status|icon)/i;
	function normalizeText(value, limit = 180) {
		return String(value || '').trim().replace(/\s+/g, ' ').slice(0, limit);
	}
	function visible(element) {
		const rect = element.getBoundingClientRect();
		const style = window.getComputedStyle(element);
		return rect.width > 0 && rect.height > 0 && style.display !== 'none' && style.visibility !== 'hidden' && Number(style.opacity || '1') > 0;
	}
	function roleOf(element) {
		const explicit = element.getAttribute('role');
		if (explicit) {
			return explicit.toLowerCase();
		}
		const tag = element.tagName.toLowerCase();
		if (tag === 'a' && element.getAttribute('href')) {
			return 'link';
		}
		if (tag === 'button') {
			return 'button';
		}
		if (['input', 'select', 'textarea'].includes(tag)) {
			return 'form-control';
		}
		if (tag === 'summary') {
			return 'button';
		}
		if (['header', 'nav', 'main', 'footer', 'section', 'aside'].includes(tag)) {
			return tag;
		}
		return 'group';
	}
	function classTokens(element) {
		return [...element.classList].filter((token) => meaningfulHookPattern.test(token)).sort();
	}
	function boxOf(element) {
		const rect = element.getBoundingClientRect();
		return { x: Math.round(rect.x), y: Math.round(rect.y), width: Math.round(rect.width), height: Math.round(rect.height), area: Math.round(rect.width * rect.height) };
	}
	function regionOf(element) {
		const region = element.closest('footer,header,nav,main,section,aside,[role=banner],[role=navigation],[role=main],[role=contentinfo]');
		if (!region) {
			return 'body';
		}
		const role = roleOf(region);
		if (role === 'banner') {
			return 'header';
		}
		if (role === 'navigation') {
			return 'nav';
		}
		if (role === 'contentinfo') {
			return 'footer';
		}
		return region.tagName.toLowerCase();
	}
	function conceptForElement(element) {
		const haystack = [element.className || '', normalizeText(element.textContent), element.getAttribute('aria-label') || ''].join(' ');
		const match = haystack.match(meaningfulHookPattern);
		return match ? match[1].toLowerCase() : '';
	}
	function clickableDescendants(element) {
		return [...element.querySelectorAll('a[href],button,[role=button],[role=link],input,select,textarea,summary')].filter(visible);
	}
	function elementSummary(element, extra = {}) {
		const clickable = clickableDescendants(element);
		return {
			tag: element.tagName.toLowerCase(),
			role: roleOf(element),
			text: normalizeText(element.textContent),
			href: element.getAttribute('href') || '',
			descendant_href: clickable.find((item) => item.getAttribute('href'))?.getAttribute('href') || '',
			own_classes: classTokens(element),
			child_classes: [...element.querySelectorAll('[class]')].flatMap((child) => classTokens(child)).filter((token, index, values) => values.indexOf(token) === index).sort(),
			contains_logo: Boolean(element.querySelector('img,svg,picture') || /\b(?:brand|logo|wordmark)\b/i.test(element.className || '') || /\b(?:logo|brand|wordmark)\b/i.test(element.getAttribute('aria-label') || '')),
			contains_wordmark: Boolean(/\bwordmark\b/i.test(element.className || '') || element.querySelector('[class*=wordmark]') || normalizeText(element.textContent).length > 0),
			contains_image: Boolean(element.querySelector('img,picture,video')),
			contains_svg: Boolean(element.querySelector('svg')),
			clickable_descendant_count: clickable.length + (['link', 'button', 'form-control'].includes(roleOf(element)) ? 1 : 0),
			child_visual_part_count: [...element.children].filter((child) => visible(child)).length,
			wraps_multiple_visual_parts: [...element.children].filter((child) => visible(child)).length >= 2,
			ancestor_region: regionOf(element),
			bounding_box: boxOf(element),
			...extra,
		};
	}
	const landmarks = {};
	for (const [name, selector] of Object.entries({ header: 'header,[role=banner]', nav: 'nav,[role=navigation]', main: 'main,[role=main]', footer: 'footer,[role=contentinfo]', section: 'section', aside: 'aside,[role=complementary]' })) {
		const matches = [...document.querySelectorAll(selector)].filter(visible);
		landmarks[name] = { count: matches.length, visible_count: matches.length, first_text: normalizeText(matches[0]?.textContent || '') };
	}
	const classOwners = [...document.querySelectorAll('[class]')].filter(visible).map((element) => {
		const classes = classTokens(element);
		return classes.length ? elementSummary(element, { selector_signature: `.${classes[0]}`, concept: conceptForElement(element) }) : null;
	}).filter(Boolean);
	const interactions = [...document.querySelectorAll('a[href],button,[role=button],[role=link],input,select,textarea,summary')].filter(visible).map((element) => elementSummary(element, { concept: conceptForElement(element) }));
	const regions = {};
	for (const name of ['header', 'nav', 'main', 'footer', 'section', 'aside', 'body']) {
		regions[name] = { link_count: 0, button_count: 0, clickable_area: 0, media_count: 0, brand_present: false, logo_present: false, text: '' };
	}
	for (const interaction of interactions) {
		const region = regions[interaction.ancestor_region] || regions.body;
		if (interaction.role === 'link') {
			region.link_count++;
		}
		if (interaction.role === 'button') {
			region.button_count++;
		}
		region.clickable_area += interaction.bounding_box.area;
	}
	for (const [name, region] of Object.entries(regions)) {
		const root = name === 'body' ? document.body : document.querySelector(name);
		if (root) {
			region.media_count = root.querySelectorAll('img,svg,picture,video').length;
			region.brand_present = Boolean(root.querySelector('[class*=brand],[class*=wordmark]'));
			region.logo_present = Boolean(root.querySelector('img,svg,picture,[class*=logo]'));
			region.text = normalizeText(root.textContent, 260);
		}
	}
	const selectorGroups = [];
	for (const group of groups || []) {
		selectorGroups.push({
			name: group.name,
			selectors: (group.selectors || []).map((selector) => {
				try {
					const matches = [...document.querySelectorAll(selector)].filter(visible);
					return { selector, count: matches.length, first: matches[0] ? elementSummary(matches[0], { concept: conceptForElement(matches[0]) }) : null };
				} catch (error) {
					return { selector, count: 0, first: null, error: error instanceof Error ? error.message : String(error) };
				}
			}),
		});
	}
	return {
		url: window.location.href,
		title: document.title,
		landmarks,
		class_owners: classOwners,
		interactions,
		regions,
		repeated: {
			card: document.querySelectorAll('[class*=card],article').length,
			list_item: document.querySelectorAll('li').length,
			feature: document.querySelectorAll('[class*=feature]').length,
			plan: document.querySelectorAll('[class*=plan],[class*=price]').length,
			testimonial: document.querySelectorAll('[class*=testimonial]').length,
		},
		selector_groups: selectorGroups,
	};
}

async function evaluateSemanticSurface(page, groups) {
	return page.evaluate(semanticFingerprintExtractor, groups);
}

function semanticSurfaceTotals(fingerprint) {
	return Object.values(fingerprint?.regions || {}).reduce((totals, region) => {
		totals.region_link_count += Number(region?.link_count || 0);
		totals.clickable_area += Number(region?.clickable_area || 0);
		return totals;
	}, { region_link_count: 0, clickable_area: 0 });
}

function semanticRole(owner) {
	return owner?.role || owner?.tag || '';
}

function semanticPrimaryClassKey(owner) {
	return owner?.own_classes?.[0] || '';
}

function semanticTextTokens(value) {
	return String(value || '').toLowerCase().replace(/[^a-z0-9\s]+/g, ' ').split(/\s+/).filter((token) => token.length >= 3);
}

function semanticAllowsLinkPreservingWrapper(sourceOwner, frontendOwner) {
	if (semanticRole(sourceOwner) !== 'link' || semanticRole(frontendOwner) === 'link' || Number(frontendOwner.clickable_descendant_count || 0) < 1) {
		return false;
	}
	const sourceHref = String(sourceOwner.href || '');
	const frontendHref = String(frontendOwner.href || frontendOwner.descendant_href || '');
	if (sourceHref && sourceHref !== frontendHref) {
		return false;
	}
	const frontendTokens = new Set(semanticTextTokens(frontendOwner.text));
	return semanticTextTokens(sourceOwner.text).every((token) => frontendTokens.has(token));
}

function semanticAllowsNavigationClassRoleChange(sourceOwner, frontendOwner) {
	const sourceRole = semanticRole(sourceOwner);
	const frontendRole = semanticRole(frontendOwner);
	const key = semanticPrimaryClassKey(sourceOwner);
	return /nav|menu/i.test(key) && ['group', 'list'].includes(sourceRole) && frontendRole === 'nav' && Number(sourceOwner.clickable_descendant_count || 0) === Number(frontendOwner.clickable_descendant_count || 0) && semanticTextTokens(sourceOwner.text).every((token) => new Set(semanticTextTokens(frontendOwner.text)).has(token));
}

function semanticMismatch(type, reason, source, frontend, extra = {}) {
	return { type, reason, region: source?.ancestor_region || frontend?.ancestor_region || extra.region || '', concept: source?.concept || frontend?.concept || extra.concept || '', selector_signature: source?.selector_signature || frontend?.selector_signature || extra.selector_signature || '', source, generated: frontend, ...extra };
}

function compareSemanticFingerprints(source, frontend) {
	const mismatches = [];
	const optionalSelectorAbsences = [];
	const counts = { role_mismatch_count: 0, class_owner_changed_count: 0, interaction_group_split_count: 0, interaction_group_merged_count: 0, link_text_delta_count: 0, landmark_mismatch_count: 0, repeated_count_delta_count: 0, brand_logo_missing_count: 0 };
	for (const landmark of ['header', 'nav', 'main', 'footer']) {
		const sourceCount = Number(source?.landmarks?.[landmark]?.visible_count || 0);
		const frontendCount = Number(frontend?.landmarks?.[landmark]?.visible_count || 0);
		if (sourceCount > 0 && frontendCount === 0) {
			counts.landmark_mismatch_count++;
			mismatches.push(semanticMismatch('landmark', 'landmark_disappeared', source?.landmarks?.[landmark], frontend?.landmarks?.[landmark], { region: landmark, concept: landmark }));
		}
	}
	const frontendOwnersByClass = new Map();
	for (const owner of frontend?.class_owners || []) {
		const key = semanticPrimaryClassKey(owner);
		if (key && !frontendOwnersByClass.has(key)) {
			frontendOwnersByClass.set(key, owner);
		}
	}
	for (const sourceOwner of source?.class_owners || []) {
		const key = semanticPrimaryClassKey(sourceOwner);
		const frontendOwner = key ? frontendOwnersByClass.get(key) : null;
		if (!frontendOwner) {
			continue;
		}
		const sourceRole = semanticRole(sourceOwner);
		const frontendRole = semanticRole(frontendOwner);
		const sourceInteractive = ['link', 'button', 'form-control'].includes(sourceRole);
		const frontendInteractive = ['link', 'button', 'form-control'].includes(frontendRole);
		const sourceClickable = Number(sourceOwner.clickable_descendant_count || 0);
		const frontendClickable = Number(frontendOwner.clickable_descendant_count || 0);
		if (sourceRole === 'link' && frontendRole !== 'link' && !semanticAllowsLinkPreservingWrapper(sourceOwner, frontendOwner)) {
			counts.role_mismatch_count++;
			counts.class_owner_changed_count++;
			if (frontendClickable > sourceClickable) {
				counts.interaction_group_split_count++;
			}
			mismatches.push(semanticMismatch('class_owner', 'classed_link_became_non_link', sourceOwner, frontendOwner, { selector_signature: `.${key}` }));
			continue;
		}
		if (sourceRole !== frontendRole && !semanticAllowsNavigationClassRoleChange(sourceOwner, frontendOwner) && !semanticAllowsLinkPreservingWrapper(sourceOwner, frontendOwner) && (sourceInteractive || frontendInteractive || sourceOwner.concept || frontendOwner.concept)) {
			counts.role_mismatch_count++;
			counts.class_owner_changed_count++;
			mismatches.push(semanticMismatch('class_owner', 'meaningful_class_moved_role', sourceOwner, frontendOwner, { selector_signature: `.${key}` }));
		}
		if (sourceInteractive && frontendClickable > sourceClickable + 1) {
			counts.interaction_group_split_count++;
			mismatches.push(semanticMismatch('interaction_group', 'source_interaction_group_split', sourceOwner, frontendOwner, { selector_signature: `.${key}` }));
		} else if (!sourceInteractive && frontendInteractive && sourceClickable > frontendClickable + 1) {
			counts.interaction_group_merged_count++;
			mismatches.push(semanticMismatch('interaction_group', 'source_interaction_group_merged', sourceOwner, frontendOwner, { selector_signature: `.${key}` }));
		}
		const sourceTokens = semanticTextTokens(sourceOwner.text);
		const frontendTokens = new Set(semanticTextTokens(frontendOwner.text));
		const missingTokens = sourceTokens.filter((token) => !frontendTokens.has(token));
		if ((sourceRole === 'link' || sourceRole === 'button') && sourceTokens.length && missingTokens.length === sourceTokens.length) {
			counts.link_text_delta_count++;
			mismatches.push(semanticMismatch('interaction_text', 'link_or_button_text_disappeared', sourceOwner, frontendOwner, { selector_signature: `.${key}`, missing_text_tokens: missingTokens }));
		}
	}
	for (const [regionName, sourceRegion] of Object.entries(source?.regions || {})) {
		const frontendRegion = frontend?.regions?.[regionName];
		if (frontendRegion && ['header', 'footer'].includes(regionName) && sourceRegion.brand_present && sourceRegion.logo_present && !frontendRegion.logo_present) {
			counts.brand_logo_missing_count++;
			mismatches.push(semanticMismatch('brand_media', 'brand_or_logo_image_disappeared', sourceRegion, frontendRegion, { region: regionName, concept: 'brand' }));
		}
	}
	for (const [name, sourceCount] of Object.entries(source?.repeated || {})) {
		const frontendCount = Number(frontend?.repeated?.[name] || 0);
		if ((Number(sourceCount || 0) >= 3 || frontendCount >= 3) && Math.abs(Number(sourceCount || 0) - frontendCount) >= Math.max(3, Math.ceil(Number(sourceCount || 0) * 0.35))) {
			counts.repeated_count_delta_count++;
			mismatches.push(semanticMismatch('repeated_structure', 'repeated_structure_count_changed_materially', { count: sourceCount }, { count: frontendCount }, { concept: name }));
		}
	}
	const frontendSelectorGroups = new Map((frontend?.selector_groups || []).map((group) => [group.name, group]));
	for (const sourceGroup of source?.selector_groups || []) {
		const frontendGroup = frontendSelectorGroups.get(sourceGroup.name);
		if (!frontendGroup) {
			continue;
		}
		const frontendSelectors = new Map((frontendGroup.selectors || []).map((selector) => [selector.selector, selector]));
		for (const sourceSelector of sourceGroup.selectors || []) {
			const frontendSelector = frontendSelectors.get(sourceSelector.selector);
			if (frontendSelector && sourceSelector.count === 0 && frontendSelector.count === 0) {
				optionalSelectorAbsences.push({ group: sourceGroup.name, selector: sourceSelector.selector });
			}
		}
	}
	const sourceTotals = semanticSurfaceTotals(source);
	const frontendTotals = semanticSurfaceTotals(frontend);
	const sourceArea = sourceTotals.clickable_area;
	return { mismatch_count: mismatches.length, ...counts, region_link_count_delta: frontendTotals.region_link_count - sourceTotals.region_link_count, clickable_area_delta_ratio: Number((sourceArea > 0 ? Math.abs(sourceArea - frontendTotals.clickable_area) / sourceArea : 0).toFixed(4)), mismatches, optional_selector_absences: optionalSelectorAbsences };
}

function buildSemanticArtifact(results, artifactPath) {
	const mismatches = [];
	const targets = [];
	for (const result of results) {
		const comparison = result.comparison || {};
		for (const mismatch of comparison.mismatches || []) {
			mismatches.push({ target: result.source_filename || String(result.wordpress_page_id || ''), ...mismatch });
		}
		targets.push({
			source_filename: result.source_filename || '',
			wordpress_page_id: result.wordpress_page_id || null,
			mismatch_count: Number(comparison.mismatch_count || 0),
			role_mismatch_count: Number(comparison.role_mismatch_count || 0),
			class_owner_changed_count: Number(comparison.class_owner_changed_count || 0),
			interaction_group_split_count: Number(comparison.interaction_group_split_count || 0),
			interaction_group_merged_count: Number(comparison.interaction_group_merged_count || 0),
			link_text_delta_count: Number(comparison.link_text_delta_count || 0),
			landmark_mismatch_count: Number(comparison.landmark_mismatch_count || 0),
			repeated_count_delta_count: Number(comparison.repeated_count_delta_count || 0),
			brand_logo_missing_count: Number(comparison.brand_logo_missing_count || 0),
			region_link_count_delta: Number(comparison.region_link_count_delta || 0),
			clickable_area_delta_ratio: Number(comparison.clickable_area_delta_ratio || 0),
			optional_selector_absent_count: Number(comparison.optional_selector_absences?.length || 0),
		});
	}
	return { artifact: artifactPath, target_count: results.length, mismatch_count: mismatches.length, targets, mismatches, results };
}

function semanticTargetMetric(semanticComparison, key) {
	return (semanticComparison?.diagnostics?.targets || semanticComparison?.targets || []).reduce((sum, target) => sum + Number(target?.[key] || 0), 0);
}

function semanticMismatchFailureDetails(semanticComparison) {
	const mismatches = semanticComparison?.diagnostics?.mismatches || semanticComparison?.mismatches || [];
	return mismatches.map((mismatch) => {
		const concept = mismatch.concept || mismatch.type || 'unknown';
		const sourceCount = mismatch.source && Object.hasOwn(mismatch.source, 'count') ? ` source=${mismatch.source.count}` : '';
		const generatedCount = mismatch.generated && Object.hasOwn(mismatch.generated, 'count') ? ` generated=${mismatch.generated.count}` : '';
		const reason = mismatch.reason ? ` reason=${mismatch.reason}` : '';
		return `semantic mismatch: ${concept}${sourceCount}${generatedCount}${reason}`;
	});
}

function crc32(buffer) {
	if (!crc32.table) {
		crc32.table = Array.from({ length: 256 }, (_, index) => {
			let value = index;
			for (let bit = 0; bit < 8; bit++) {
				value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
			}
			return value >>> 0;
		});
	}
	let crc = 0xffffffff;
	for (const byte of buffer) {
		crc = crc32.table[(crc ^ byte) & 0xff] ^ (crc >>> 8);
	}
	return (crc ^ 0xffffffff) >>> 0;
}

function pngChunk(type, data = Buffer.alloc(0)) {
	const typeBuffer = Buffer.from(type, 'ascii');
	const chunk = Buffer.concat([typeBuffer, data]);
	const output = Buffer.alloc(12 + data.length);
	output.writeUInt32BE(data.length, 0);
	typeBuffer.copy(output, 4);
	data.copy(output, 8);
	output.writeUInt32BE(crc32(chunk), 8 + data.length);
	return output;
}

function encodePng({ width, height, data }) {
	const header = Buffer.alloc(13);
	header.writeUInt32BE(width, 0);
	header.writeUInt32BE(height, 4);
	header[8] = 8;
	header[9] = 6;
	const rows = Buffer.alloc((width * 4 + 1) * height);
	for (let y = 0; y < height; y++) {
		const rowOffset = y * (width * 4 + 1);
		rows[rowOffset] = 0;
		Buffer.from(data.buffer, data.byteOffset + y * width * 4, width * 4).copy(rows, rowOffset + 1);
	}
	return Buffer.concat([PNG_SIGNATURE, pngChunk('IHDR', header), pngChunk('IDAT', zlib.deflateSync(rows)), pngChunk('IEND')]);
}

function decodePng(buffer) {
	if (!buffer.subarray(0, PNG_SIGNATURE.length).equals(PNG_SIGNATURE)) {
		throw new Error('Unsupported PNG signature.');
	}
	let offset = PNG_SIGNATURE.length;
	let width = 0;
	let height = 0;
	let bitDepth = 0;
	let colorType = 0;
	const idat = [];
	while (offset < buffer.length) {
		const length = buffer.readUInt32BE(offset);
		const type = buffer.toString('ascii', offset + 4, offset + 8);
		const data = buffer.subarray(offset + 8, offset + 8 + length);
		offset += 12 + length;
		if (type === 'IHDR') {
			width = data.readUInt32BE(0);
			height = data.readUInt32BE(4);
			bitDepth = data[8];
			colorType = data[9];
		} else if (type === 'IDAT') {
			idat.push(data);
		} else if (type === 'IEND') {
			break;
		}
	}
	if (bitDepth !== 8 || ![0, 2, 4, 6].includes(colorType)) {
		throw new Error(`Unsupported PNG format: bitDepth=${bitDepth} colorType=${colorType}.`);
	}
	const channels = { 0: 1, 2: 3, 4: 2, 6: 4 }[colorType];
	const stride = width * channels;
	const inflated = zlib.inflateSync(Buffer.concat(idat));
	const imageData = new Uint8ClampedArray(width * height * 4);
	let inputOffset = 0;
	let previous = Buffer.alloc(stride);
	for (let y = 0; y < height; y++) {
		const filter = inflated[inputOffset++];
		const row = Buffer.from(inflated.subarray(inputOffset, inputOffset + stride));
		inputOffset += stride;
		for (let x = 0; x < stride; x++) {
			const left = x >= channels ? row[x - channels] : 0;
			const up = previous[x] || 0;
			const upperLeft = x >= channels ? previous[x - channels] || 0 : 0;
			let value = row[x];
			if (filter === 1) {
				value += left;
			} else if (filter === 2) {
				value += up;
			} else if (filter === 3) {
				value += Math.floor((left + up) / 2);
			} else if (filter === 4) {
				const p = left + up - upperLeft;
				const pa = Math.abs(p - left);
				const pb = Math.abs(p - up);
				const pc = Math.abs(p - upperLeft);
				if (pa <= pb && pa <= pc) {
					value += left;
				} else if (pb <= pc) {
					value += up;
				} else {
					value += upperLeft;
				}
			} else if (filter !== 0) {
				throw new Error(`Unsupported PNG row filter: ${filter}.`);
			}
			row[x] = value & 0xff;
		}
		for (let x = 0; x < width; x++) {
			const source = x * channels;
			const target = (y * width + x) * 4;
			imageData[target] = row[source];
			if (colorType === 0 || colorType === 4) {
				imageData[target + 1] = row[source];
				imageData[target + 2] = row[source];
			} else {
				imageData[target + 1] = row[source + 1];
				imageData[target + 2] = row[source + 2];
			}
			if (colorType === 4) {
				imageData[target + 3] = row[source + 1];
			} else if (colorType === 6) {
				imageData[target + 3] = row[source + 3];
			} else {
				imageData[target + 3] = 255;
			}
		}
		previous = row;
	}
	return { width, height, data: imageData };
}

function normalizePng(image, width, height) {
	if (image.width === width && image.height === height) {
		return image.data;
	}
	const normalized = new Uint8ClampedArray(width * height * 4);
	normalized.fill(255);
	for (let y = 0; y < image.height; y++) {
		for (let x = 0; x < image.width; x++) {
			const source = (y * image.width + x) * 4;
			const target = (y * width + x) * 4;
			normalized[target] = image.data[source];
			normalized[target + 1] = image.data[source + 1];
			normalized[target + 2] = image.data[source + 2];
			normalized[target + 3] = image.data[source + 3];
		}
	}
	return normalized;
}

function comparePixels(sourceData, targetData, diffData) {
	let mismatched = 0;
	for (let index = 0; index < sourceData.length; index += 4) {
		const delta = Math.max(Math.abs(sourceData[index] - targetData[index]), Math.abs(sourceData[index + 1] - targetData[index + 1]), Math.abs(sourceData[index + 2] - targetData[index + 2]), Math.abs(sourceData[index + 3] - targetData[index + 3]));
		if (delta > 26) {
			mismatched++;
			diffData[index] = 255;
			diffData[index + 1] = 0;
			diffData[index + 2] = 0;
			diffData[index + 3] = 255;
		} else {
			const gray = Math.round((sourceData[index] + sourceData[index + 1] + sourceData[index + 2]) / 3);
			diffData[index] = gray;
			diffData[index + 1] = gray;
			diffData[index + 2] = gray;
			diffData[index + 3] = 80;
		}
	}
	return mismatched;
}

async function comparePngScreenshots(sourcePath, targetPath, diffPath) {
	const sourceImage = decodePng(await readFile(sourcePath));
	const targetImage = decodePng(await readFile(targetPath));
	const width = Math.max(sourceImage.width, targetImage.width);
	const height = Math.max(sourceImage.height, targetImage.height);
	const sourceData = normalizePng(sourceImage, width, height);
	const targetData = normalizePng(targetImage, width, height);
	const diffData = new Uint8ClampedArray(width * height * 4);
	const mismatchedPixels = comparePixels(sourceData, targetData, diffData);
	await writeFile(diffPath, encodePng({ width, height, data: diffData }));
	return { diff_path: diffPath, height, mismatched_pixels: mismatchedPixels, pixel_count: width * height, ratio: width > 0 && height > 0 ? mismatchedPixels / (width * height) : 1, width };
}

module.exports = {
	asArray,
	buildSemanticArtifact,
	buildVisualDiagnostics,
	comparePngScreenshots,
	compareSemanticFingerprints,
	comparisonTargets,
	decodePng,
	encodePng,
	evaluateSemanticSurface,
	resolveSourceStaticFile,
	safeSlug,
	semanticComparisonTargets,
	semanticFingerprintExtractor,
	semanticMismatchFailureDetails,
	semanticTargetMetric,
	semanticTargetSelectorGroups,
	stringArray,
	surfaceUrl,
	targetSelectorGroups,
	visualMismatchReason,
	visualParity,
	visualProbeGroups,
	visualSelectorComparisonDetails,
	visualSurfaceTotals,
};
