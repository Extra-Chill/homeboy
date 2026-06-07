'use strict';

/**
 * External dependencies
 */
const { spawn } = require('node:child_process');

/**
 * Internal dependencies
 */
const { isPlainObject } = require('./shared');

const DEFAULT_POST_TYPES = ['page', 'post', 'wp_template', 'wp_template_part'];
const DEFAULT_POST_STATUSES = ['any'];
const DEFAULT_CONTENT_PREVIEW_BYTES = 2000;

function wordpressBlockQualityProbeCode(options = {}) {
 const config = encodeProbeConfig(normalizeBlockQualityOptions(options));
 return phpProbe(`
$homeboy_config = json_decode( base64_decode( '${config}' ), true );
$post_types = isset( $homeboy_config['post_types'] ) && is_array( $homeboy_config['post_types'] ) ? $homeboy_config['post_types'] : array( 'page', 'post', 'wp_template', 'wp_template_part' );
$post_statuses = isset( $homeboy_config['post_statuses'] ) && is_array( $homeboy_config['post_statuses'] ) ? $homeboy_config['post_statuses'] : array( 'any' );
$fallback_option_names = isset( $homeboy_config['fallback_option_names'] ) && is_array( $homeboy_config['fallback_option_names'] ) ? $homeboy_config['fallback_option_names'] : array();
$target_post_ids = isset( $homeboy_config['target_post_ids'] ) && is_array( $homeboy_config['target_post_ids'] ) ? array_map( 'absint', $homeboy_config['target_post_ids'] ) : array();
$target_post_titles = isset( $homeboy_config['target_post_titles'] ) && is_array( $homeboy_config['target_post_titles'] ) ? array_map( 'strval', $homeboy_config['target_post_titles'] ) : array();
if ( ! empty( $homeboy_config['include_front_page_target'] ) ) {
    $front_page_id = (int) get_option( 'page_on_front', 0 );
    if ( $front_page_id > 0 && ! in_array( $front_page_id, $target_post_ids, true ) ) {
        $target_post_ids[] = $front_page_id;
    }
}

$counts = homeboy_wordpress_empty_block_quality_counts();
$counts['fallback_count'] = homeboy_wordpress_fallback_option_total( $fallback_option_names );

$posts = get_posts( array(
    'post_type'   => $post_types,
    'post_status' => $post_statuses,
    'numberposts' => -1,
) );

foreach ( $posts as $post ) {
    $before = homeboy_wordpress_block_quality_target_snapshot( $counts );
    homeboy_wordpress_add_post_block_quality( $counts, $post );
    if ( homeboy_wordpress_is_target_block_quality_post( $post, $target_post_ids, $target_post_titles ) ) {
        homeboy_wordpress_add_target_post_block_quality( $counts, $post, $before );
    }
}

$counts['core_html_without_fallback'] = max( 0, $counts['core_html_blocks'] - $counts['fallback_count'] );
$counts['target_core_html_without_fallback'] = max( 0, $counts['target_core_html_blocks'] - $counts['fallback_count'] );
$counts['target_core_html_without_bfb_fallback'] = $counts['target_core_html_without_fallback'];

echo wp_json_encode( $counts, JSON_PRETTY_PRINT ) . PHP_EOL;
`);
}

function wordpressPostBlockQualityProbeCode(postId, options = {}) {
 const config = encodeProbeConfig({
  ...normalizeBlockQualityOptions(options),
  post_id: requiredPositiveInteger(postId, 'postId'),
  content_preview_bytes: normalizeContentPreviewBytes(options.contentPreviewBytes),
 });
 return phpProbe(`
$homeboy_config = json_decode( base64_decode( '${config}' ), true );
$post_id = absint( $homeboy_config['post_id'] );
$fallback_option_names = isset( $homeboy_config['fallback_option_names'] ) && is_array( $homeboy_config['fallback_option_names'] ) ? $homeboy_config['fallback_option_names'] : array();
$content_preview_bytes = isset( $homeboy_config['content_preview_bytes'] ) ? absint( $homeboy_config['content_preview_bytes'] ) : 2000;

$post = get_post( $post_id );
if ( ! $post ) {
    fwrite( STDERR, 'WordPress post not found: ' . $post_id );
    exit( 1 );
}

$counts = homeboy_wordpress_empty_block_quality_counts();
$counts['fallback_count'] = homeboy_wordpress_fallback_option_total( $fallback_option_names );
homeboy_wordpress_add_post_block_quality( $counts, $post );
$content = (string) $post->post_content;
$counts['post_id'] = $post_id;
$counts['post_type'] = (string) $post->post_type;
$counts['post_title'] = (string) $post->post_title;
$counts['stored_content_hash'] = hash( 'sha256', $content );
$counts['stored_content_bytes'] = strlen( $content );
$counts['stored_content_preview'] = substr( $content, 0, $content_preview_bytes );
$counts['core_html_without_fallback'] = max( 0, $counts['core_html_blocks'] - $counts['fallback_count'] );

echo wp_json_encode( $counts, JSON_PRETTY_PRINT ) . PHP_EOL;
`);
}

async function probeWordPressBlockQuality(sitePath, options = {}) {
 const result = await runWordPressEval(wordpressBlockQualityProbeCode(options), sitePath, options);
 return parseWordPressBlockQualityProbeOutput(result.stdout);
}

async function probeWordPressPostBlockQuality(sitePath, postId, options = {}) {
 const result = await runWordPressEval(wordpressPostBlockQualityProbeCode(postId, options), sitePath, options);
 return parseWordPressBlockQualityProbeOutput(result.stdout);
}

function parseWordPressBlockQualityProbeOutput(stdout) {
 const text = String(stdout || '');
 const jsonStart = text.indexOf('{');
 if (jsonStart === -1) {
  throw new Error(`WordPress block quality probe did not emit JSON: ${text.slice(0, 1000)}`);
 }
 return JSON.parse(text.slice(jsonStart));
}

async function runWordPressEval(code, sitePath, options = {}) {
 const command = `eval ${quoteShell(code)}`;
 const runCli = options.runCli || defaultRunCli;
 const result = normalizeCliResult(await runCli(command, {
  ...options,
  role: options.role || 'wordpress-block-quality-probe',
  sitePath,
 }));
 if (result.exitCode !== 0) {
  throw new Error(`WordPress block quality probe failed with exit code ${result.exitCode}: ${result.stderr || result.stdout || 'no output'}`);
 }
 return result;
}

function defaultRunCli(command, options = {}) {
 const cliPath = options.cliPath || 'wp';
 const args = [normalizeCliCommand(command)];
 if (options.sitePath) {
  args.push(`--path=${quoteShell(options.sitePath)}`);
 }
 return runShellCommand(`${cliPath} ${args.filter(Boolean).join(' ')}`, {
  cwd: options.cwd || options.sitePath || process.cwd(),
  env: options.env,
 });
}

function runShellCommand(command, options = {}) {
 return new Promise((resolve) => {
  const child = spawn(command, {
   cwd: options.cwd,
   env: { ...process.env, ...(options.env || {}) },
   shell: true,
   stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = '';
  let stderr = '';
  child.stdout.on('data', (chunk) => {
   stdout += chunk.toString();
  });
  child.stderr.on('data', (chunk) => {
   stderr += chunk.toString();
  });
  child.on('error', (error) => {
   resolve({ exitCode: 1, stdout, stderr: stderr || error.message, error });
  });
  child.on('close', (code, signal) => {
   resolve({ exitCode: code ?? 1, signal, stdout, stderr });
  });
 });
}

function normalizeCliResult(result) {
	if (result === undefined || result === null) {
		return { exitCode: 0, stdout: '', stderr: '' };
	}
	if (typeof result === 'string') {
		return { exitCode: 0, stdout: result, stderr: '' };
	}
	let exitCode = 0;
	if (Number.isInteger(result.exitCode)) {
		exitCode = result.exitCode;
	} else if (Number.isInteger(result.code)) {
		exitCode = result.code;
	}
	return {
		exitCode,
		stdout: String(result.stdout || ''),
		stderr: String(result.stderr || ''),
		signal: result.signal,
	};
}

function normalizeCliCommand(command) {
 const trimmed = String(command || '').trim();
 return trimmed.startsWith('wp ') ? trimmed.slice(3).trim() : trimmed;
}

function normalizeBlockQualityOptions(options = {}) {
 if (!isPlainObject(options)) {
  throw new TypeError('WordPress block quality probe options must be an object');
 }
 return {
  post_types: normalizeStringList(options.postTypes, DEFAULT_POST_TYPES),
  post_statuses: normalizeStringList(options.postStatuses, DEFAULT_POST_STATUSES),
  fallback_option_names: normalizeStringList(options.fallbackOptionNames, []),
  target_post_ids: normalizePositiveIntegerList(options.targetPostIds),
  target_post_titles: normalizeStringList(options.targetPostTitles, []),
  include_front_page_target: Boolean(options.includeFrontPageTarget),
 };
}

function normalizeStringList(value, defaultValue) {
 if (value === undefined || value === null) {
  return [...defaultValue];
 }
 const list = Array.isArray(value) ? value : [value];
 return list.map((item) => String(item || '').trim()).filter(Boolean);
}

function normalizeContentPreviewBytes(value) {
 if (value === undefined || value === null) {
  return DEFAULT_CONTENT_PREVIEW_BYTES;
 }
 const number = Number(value);
 if (!Number.isFinite(number) || number < 0) {
  throw new TypeError('contentPreviewBytes must be a non-negative number');
 }
 return Math.floor(number);
}

function normalizePositiveIntegerList(value) {
 if (value === undefined || value === null) {
  return [];
 }
 const list = Array.isArray(value) ? value : [value];
 return list
  .map((item) => Number(item))
  .filter((item) => Number.isInteger(item) && item > 0);
}

function requiredPositiveInteger(value, label) {
 const number = Number(value);
 if (!Number.isInteger(number) || number <= 0) {
  throw new TypeError(`${label} must be a positive integer`);
 }
 return number;
}

function encodeProbeConfig(config) {
 return Buffer.from(JSON.stringify(config)).toString('base64');
}

function quoteShell(value) {
 return `'${String(value).replace(/'/g, `'\\''`)}'`;
}

function phpProbe(body) {
 return String.raw`
function homeboy_wordpress_count_blocks( $blocks, &$counts ) {
    foreach ( $blocks as $block ) {
        $name = isset( $block['blockName'] ) ? (string) $block['blockName'] : '';
        if ( '' !== $name ) {
            $counts['total_blocks']++;
            if ( 'core/html' === $name ) {
                $counts['core_html_blocks']++;
            }
        }
        if ( ! empty( $block['innerBlocks'] ) ) {
            homeboy_wordpress_count_blocks( $block['innerBlocks'], $counts );
        }
    }
}

function homeboy_wordpress_empty_block_quality_counts() {
    return array(
        'posts_seen'                  => 0,
        'posts_with_content'          => 0,
        'posts_with_blocks'           => 0,
        'pages_seen'                  => 0,
        'templates_seen'              => 0,
        'template_parts_seen'         => 0,
        'raw_html_unconverted'        => 0,
        'total_blocks'                => 0,
        'core_html_blocks'            => 0,
        'serialized_block_comments'   => 0,
        'fallback_count'              => 0,
        'core_html_without_fallback'  => 0,
        'target_posts_seen'           => 0,
        'target_pages_seen'           => 0,
        'target_posts_with_blocks'    => 0,
        'target_raw_html_unconverted' => 0,
        'target_total_blocks'         => 0,
        'target_core_html_blocks'     => 0,
        'target_core_html_without_fallback' => 0,
        'target_core_html_without_bfb_fallback' => 0,
        'target_serialized_block_comments' => 0,
        'post_type_counts'            => new stdClass(),
    );
}

function homeboy_wordpress_add_post_type_count( &$counts, $post_type ) {
    $post_type = (string) $post_type;
    if ( ! isset( $counts['post_type_counts']->$post_type ) ) {
        $counts['post_type_counts']->$post_type = 0;
    }
    $counts['post_type_counts']->$post_type++;
}

function homeboy_wordpress_add_post_block_quality( &$counts, $post ) {
    $content = (string) $post->post_content;
    $counts['posts_seen']++;
    homeboy_wordpress_add_post_type_count( $counts, $post->post_type );
    if ( 'page' === $post->post_type ) {
        $counts['pages_seen']++;
    } elseif ( 'wp_template' === $post->post_type ) {
        $counts['templates_seen']++;
    } elseif ( 'wp_template_part' === $post->post_type ) {
        $counts['template_parts_seen']++;
    }
    if ( '' === trim( $content ) ) {
        return;
    }
    $counts['posts_with_content']++;
    $counts['serialized_block_comments'] += substr_count( $content, '<!-- wp:' );
    if ( false !== strpos( $content, '<!-- wp:' ) ) {
        $counts['posts_with_blocks']++;
    } elseif ( preg_match( '/<\/?[a-z][\s>]/i', $content ) ) {
        $counts['raw_html_unconverted']++;
    }
    homeboy_wordpress_count_blocks( parse_blocks( $content ), $counts );
}

function homeboy_wordpress_block_quality_target_snapshot( $counts ) {
    return array(
        'total_blocks'              => (int) $counts['total_blocks'],
        'core_html_blocks'          => (int) $counts['core_html_blocks'],
        'serialized_block_comments' => (int) $counts['serialized_block_comments'],
        'raw_html_unconverted'      => (int) $counts['raw_html_unconverted'],
    );
}

function homeboy_wordpress_is_target_block_quality_post( $post, $target_post_ids, $target_post_titles ) {
    if ( in_array( (int) $post->ID, $target_post_ids, true ) ) {
        return true;
    }
    return in_array( (string) $post->post_title, $target_post_titles, true );
}

function homeboy_wordpress_add_target_post_block_quality( &$counts, $post, $before ) {
    $counts['target_posts_seen']++;
    if ( 'page' === $post->post_type ) {
        $counts['target_pages_seen']++;
    }
    if ( false !== strpos( (string) $post->post_content, '<!-- wp:' ) ) {
        $counts['target_posts_with_blocks']++;
    }
    $counts['target_total_blocks'] += max( 0, (int) $counts['total_blocks'] - (int) $before['total_blocks'] );
    $counts['target_core_html_blocks'] += max( 0, (int) $counts['core_html_blocks'] - (int) $before['core_html_blocks'] );
    $counts['target_serialized_block_comments'] += max( 0, (int) $counts['serialized_block_comments'] - (int) $before['serialized_block_comments'] );
    $counts['target_raw_html_unconverted'] += max( 0, (int) $counts['raw_html_unconverted'] - (int) $before['raw_html_unconverted'] );
}

function homeboy_wordpress_fallback_option_total( $option_names ) {
    $total = 0;
    foreach ( $option_names as $option_name ) {
        $total += (int) get_option( (string) $option_name, 0 );
    }
    return $total;
}
${body}`;
}

module.exports = {
 parseWordPressBlockQualityProbeOutput,
 probeWordPressBlockQuality,
 probeWordPressPostBlockQuality,
 wordpressBlockQualityProbeCode,
 wordpressPostBlockQualityProbeCode,
};
