<?php
// Homeboy WordPress archive deploy: run a check or an action inside PHP-FPM.
// Source of truth for the archive_install after_switch_command and the
// release_layout precondition_command in wordpress.json. Regenerate with:
//   node scripts/deploy/archive-runtime-commands.js --write
// opcache_reset()/opcache_invalidate()/ini_get() from the CLI SAPI only see the
// CLI's own settings and cache, so this talks FastCGI straight to each FPM
// socket and runs a tiny worker script inside FPM.
//
// HOMEBOY_FPM_MODE unset (after switch): drop PHP-FPM's cached bytecode for the
// swapped component. opcache.revalidate_freq keeps serving old compiled files
// for a few seconds after a deploy swap, so old code can require files the new
// release deleted (homeboy#17922). Best-effort: never fails the deploy.
//
// HOMEBOY_FPM_MODE=release-precondition: decide whether the versioned release
// layout (homeboy#18365) is safe. With opcache.revalidate_path=Off opcache keys
// scripts by the unresolved path, so after a symlink flip FPM would keep serving
// the old release. Passes only when every FPM socket answers that opcache is
// disabled or revalidate_path is On. Fails closed: no php, no socket, an
// unreachable socket, or any other answer exits non-zero.
$precondition = 'release-precondition' === getenv('HOMEBOY_FPM_MODE');
$label = $precondition ? 'release precondition' : 'opcache';
$stop = function ($message) use ($precondition, $label) {
    echo "{$label}: {$message}" . ($precondition ? '; failing closed' : '; skipped') . "\n";
    exit($precondition ? 1 : 0);
};
$target = rtrim((string) getenv('HOMEBOY_OPCACHE_TARGET'), '/');
if ($precondition) {
    // The target may not exist yet (first install); the worker lives beside it.
    $dir = '' === $target ? false : realpath(dirname($target));
    if (false === $dir || !is_dir($dir)) {
        $stop("parent directory of '{$target}' not found");
    }
    $worker = $dir . '/.homeboy-fpm-check-' . bin2hex(random_bytes(8)) . '.php';
    $code = <<<'WORKER'
<?php
if (!extension_loaded('Zend OPcache') || !filter_var(ini_get('opcache.enable'), FILTER_VALIDATE_BOOLEAN)) { echo 'opcache=disabled'; return; }
echo filter_var(ini_get('opcache.revalidate_path'), FILTER_VALIDATE_BOOLEAN) ? 'revalidate_path=on' : 'revalidate_path=off';
WORKER;
} else {
    $root = '' === $target ? false : realpath($target);
    if (false === $root || !is_dir($root)) {
        $stop("target '{$target}' not found");
    }
    $worker = dirname($root) . '/.homeboy-opcache-' . bin2hex(random_bytes(8)) . '.php';
    $code = '<?php $root = ' . var_export($root, true) . ';' . <<<'WORKER'
if (!function_exists('opcache_invalidate') || !ini_get('opcache.enable')) { echo 'opcache disabled'; return; }
$prefix = $root . '/';
$files = array();
$status = function_exists('opcache_get_status') ? @opcache_get_status(true) : false;
if (is_array($status) && !empty($status['scripts'])) {
    foreach (array_keys($status['scripts']) as $file) {
        if (0 === strncmp($file, $prefix, strlen($prefix))) { $files[$file] = true; }
    }
}
$walk = new RecursiveIteratorIterator(new RecursiveDirectoryIterator($root, FilesystemIterator::SKIP_DOTS));
foreach ($walk as $file) {
    if ('.php' === substr($file->getFilename(), -4)) { $files[$file->getPathname()] = true; }
}
$done = 0;
foreach (array_keys($files) as $file) { if (opcache_invalidate($file, true)) { $done++; } }
echo 'invalidated ' . $done . ' file(s)';
WORKER;
}
$sockets = array();
$configured = (string) getenv('HOMEBOY_PHP_FPM_SOCKET');
if ('' !== $configured) {
    foreach (explode(',', $configured) as $socket) {
        if ('' !== trim($socket)) {
            $sockets[trim($socket)] = trim($socket);
        }
    }
} else {
    foreach (array('/run/php/*.sock', '/var/run/php/*.sock', '/run/php-fpm/*.sock', '/var/run/php-fpm/*.sock', '/run/php*-fpm.sock', '/var/run/php*-fpm.sock') as $pattern) {
        foreach ((array) glob($pattern) as $socket) {
            $real = realpath($socket);
            if (false !== $real && is_writable($real)) {
                $sockets[$real] = $real;
            }
        }
    }
}
if (array() === $sockets) {
    $stop('no writable PHP-FPM socket found (set HOMEBOY_PHP_FPM_SOCKET)');
}
if (false === @file_put_contents($worker, $code)) {
    $stop("could not write {$worker}");
}
@chmod($worker, 0644);
$record = function ($type, $content) {
    $length = strlen($content);
    $pad = (8 - $length % 8) % 8;
    return pack('CCnnCC', 1, $type, 1, $length, $pad, 0) . $content . str_repeat("\0", $pad);
};
$pair = function ($name, $value) {
    $encode = function ($length) { return $length < 128 ? chr($length) : pack('N', $length | 0x80000000); };
    return $encode(strlen($name)) . $encode(strlen($value)) . $name . $value;
};
$params = '';
foreach (array(
    'GATEWAY_INTERFACE' => 'CGI/1.1',
    'REQUEST_METHOD'    => 'GET',
    'SCRIPT_FILENAME'   => $worker,
    'SCRIPT_NAME'       => '/' . basename($worker),
    'REQUEST_URI'       => '/' . basename($worker),
    'DOCUMENT_ROOT'     => dirname($worker),
    'QUERY_STRING'      => '',
    'SERVER_SOFTWARE'   => 'homeboy',
    'SERVER_PROTOCOL'   => 'HTTP/1.1',
    'REMOTE_ADDR'       => '127.0.0.1',
) as $name => $value) {
    $params .= $pair($name, $value);
}
$request = $record(1, pack('nCx5', 1, 0)) . $record(4, $params) . $record(4, '') . $record(5, '');
$read = function ($stream, $length) {
    $data = '';
    while (strlen($data) < $length && !feof($stream)) {
        $chunk = fread($stream, $length - strlen($data));
        if (false === $chunk || '' === $chunk) {
            $meta = stream_get_meta_data($stream);
            if ($meta['timed_out']) { break; }
        }
        $data .= (string) $chunk;
    }
    return $data;
};
$failed = 0;
foreach ($sockets as $socket) {
    $stream = @stream_socket_client('unix://' . $socket, $errno, $error, 5);
    if (false === $stream) {
        echo "{$label}: {$socket}: connect failed ({$error})" . ($precondition ? '' : '; skipped') . "\n";
        $failed++;
        continue;
    }
    stream_set_timeout($stream, 15);
    fwrite($stream, $request);
    $stdout = '';
    $stderr = '';
    while (8 === strlen($header = $read($stream, 8))) {
        $head = unpack('Cversion/Ctype/nid/nlength/Cpad/Creserved', $header);
        $body = $read($stream, $head['length'] + $head['pad']);
        $body = substr($body, 0, $head['length']);
        if (6 === $head['type']) { $stdout .= $body; }
        if (7 === $head['type']) { $stderr .= $body; }
        if (3 === $head['type']) { break; }
    }
    fclose($stream);
    $split = strpos($stdout, "\r\n\r\n");
    $result = trim(false === $split ? $stdout : substr($stdout, $split + 4));
    if ('' === $result) {
        $result = 'no response' . ('' === trim($stderr) ? '' : ': ' . trim($stderr));
    }
    echo "{$label}: {$socket}: {$result}\n";
    if ('revalidate_path=on' !== $result && 'opcache=disabled' !== $result) {
        $failed++;
    }
}
@unlink($worker);
if ($precondition) {
    if (0 !== $failed) {
        echo "{$label}: {$failed} of " . count($sockets) . " PHP-FPM socket(s) cannot serve a symlinked release safely; set opcache.revalidate_path=1 in every FPM pool to enable the release layout\n";
        exit(1);
    }
    echo "{$label}: passed for " . count($sockets) . " PHP-FPM socket(s)\n";
}
