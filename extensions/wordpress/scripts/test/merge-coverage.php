<?php
/** Merge inclusive pcov maps; inventory declarations without executing source files.
 * Usage: php merge-coverage.php <maps> <source> <component> <artifacts>
 */
if ($argc !== 5) {
    fwrite(STDERR, "Usage: php merge-coverage.php <maps> <source> <component> <artifacts>\n");
    exit(1);
}
[$script, $mapsDir, $source, $component, $artifacts] = $argv;
$maps = [];
$processFiles = glob($mapsDir . '/*.json');
if (!$processFiles) {
    fwrite(STDERR, "ERROR: pcov produced no process maps; run host PHP scripts with the coverage ini enabled.\n");
    exit(1);
}
foreach ($processFiles as $path) {
    foreach (json_decode(file_get_contents($path), true, 512, JSON_THROW_ON_ERROR) as $file => $lines) {
        foreach ($lines as $line => $hit) {
            // pcov uses 1 for executed and -1 for executable but not executed.
            $maps[$file][$line] = max($maps[$file][$line] ?? -1, $hit);
        }
    }
}

function closingBrace(array $tokens, int $start): int {
    $depth = 0;
    for ($i = $start; $i < count($tokens); $i++) {
        if ($tokens[$i]->text === '{' || in_array($tokens[$i]->id, [T_CURLY_OPEN, T_DOLLAR_OPEN_CURLY_BRACES], true)) {
            $depth++;
        } elseif ($tokens[$i]->text === '}' && --$depth === 0) {
            return $i;
        }
    }
    return count($tokens) - 1;
}

function declarations(array $tokens): array {
    $classes = [];
    $methods = [];
    $namespace = '';
    foreach ($tokens as $i => $token) {
        if ($token->id === T_NAMESPACE) {
            $namespace = '';
            for ($j = $i + 1; isset($tokens[$j]) && !in_array($tokens[$j]->text, [';', '{'], true); $j++) {
                if (in_array($tokens[$j]->id, [T_STRING, T_NAME_QUALIFIED, T_NS_SEPARATOR], true)) {
                    $namespace .= $tokens[$j]->text;
                }
            }
        }
        if (in_array($token->id, [T_CLASS, T_TRAIT, T_INTERFACE, T_ENUM], true)) {
            $j = $i + 1;
            while (isset($tokens[$j]) && $tokens[$j]->isIgnorable()) {
                $j++;
            }
            // ::class and anonymous classes have no declaration name here.
            if (!isset($tokens[$j]) || $tokens[$j]->id !== T_STRING) {
                continue;
            }
            $name = ($namespace ? $namespace . '\\' : '') . $tokens[$j]->text;
            while (isset($tokens[$j]) && $tokens[$j]->text !== '{') {
                $j++;
            }
            if (isset($tokens[$j])) {
                $classes[] = [$i, closingBrace($tokens, $j), $name];
            }
        }
        if ($token->id !== T_FUNCTION) {
            continue;
        }
        $j = $i + 1;
        while (isset($tokens[$j]) && ($tokens[$j]->isIgnorable() || $tokens[$j]->text === '&')) {
            $j++;
        }
        if (!isset($tokens[$j]) || $tokens[$j]->id !== T_STRING) {
            continue; // Closures are not named methods.
        }
        $name = $tokens[$j]->text;
        $paren = 0;
        for (; isset($tokens[$j]); $j++) {
            if ($tokens[$j]->text === '(') {
                $paren++;
            } elseif ($tokens[$j]->text === ')') {
                $paren--;
            } elseif ($paren === 0 && in_array($tokens[$j]->text, ['{', ';'], true)) {
                break;
            }
        }
        if (!isset($tokens[$j])) {
            continue;
        }
        $end = $tokens[$j]->text === '{' ? closingBrace($tokens, $j) : $j;
        $class = null;
        foreach ($classes as [$first, $last, $className]) {
            if ($first < $i && $i < $last) {
                $class = $className;
            }
        }
        $methods[] = [
            'class' => $class,
            'method' => $class === null && $namespace ? $namespace . '\\' . $name : $name,
            'start_line' => $token->line,
            'end_line' => $tokens[$end]->line,
        ];
    }
    return $methods;
}

$files = [];
$iterator = new RecursiveIteratorIterator(new RecursiveDirectoryIterator($source, FilesystemIterator::SKIP_DOTS));
foreach ($iterator as $file) {
    if ($file->isFile() && strtolower($file->getExtension()) === 'php') {
        $files[] = $file->getRealPath();
    }
}
sort($files);
$methods = [];
$xml = new SimpleXMLElement('<coverage><project name="pcov"/></coverage>');
foreach ($files as $file) {
    $tokens = PhpToken::tokenize(file_get_contents($file));
    $lines = $maps[$file] ?? [];
    if (!array_key_exists($file, $maps)) {
        // pcov cannot report executable opcodes for files PHP never compiled.
        // Token-based statement locations supply the unloaded-file denominator;
        // loaded files always use the driver's exact executable line map.
        $statementStart = null;
        foreach ($tokens as $token) {
            if ($token->isIgnorable() || $token->id === T_OPEN_TAG || $token->id === T_CLOSE_TAG) {
                continue;
            }
            if ($token->text === ';') {
                if ($statementStart !== null) {
                    $lines[$statementStart] = -1;
                }
                $statementStart = null;
            } elseif (in_array($token->text, ['{', '}'], true)) {
                $statementStart = null;
            } elseif ($statementStart === null) {
                $statementStart = $token->line;
            }
        }
    }
    ksort($lines);
    $relative = substr($file, strlen(rtrim($component, '/')) + 1);
    $fileMethods = declarations($tokens);
    $coveredMethods = 0;
    $classes = [];
    foreach ($fileMethods as $method) {
        $executable = $executed = 0;
        foreach ($lines as $line => $hit) {
            if ($line >= $method['start_line'] && $line <= $method['end_line']) {
                $executable++;
                $executed += $hit > 0 ? 1 : 0;
            }
        }
        $methods[] = array_merge(['file' => $relative], $method, [
            'executed_lines' => $executed, 'executable_lines' => $executable,
        ]);
        $coveredMethods += $executed > 0 ? 1 : 0;
        if ($method['class'] !== null) {
            $classes[$method['class']] = ($classes[$method['class']] ?? false) || $executed > 0;
        }
    }
    $node = $xml->project->addChild('file');
    $node->addAttribute('name', $file);
    foreach ($lines as $line => $hit) {
        $lineNode = $node->addChild('line');
        $lineNode->addAttribute('num', (string) $line);
        $lineNode->addAttribute('type', 'stmt');
        $lineNode->addAttribute('count', $hit > 0 ? '1' : '0');
    }
    $metrics = $node->addChild('metrics');
    foreach ([
        'statements' => count($lines),
        'coveredstatements' => count(array_filter($lines, static fn($hit) => $hit > 0)),
        'methods' => count($fileMethods), 'coveredmethods' => $coveredMethods,
        'classes' => count($classes), 'coveredclasses' => count(array_filter($classes)),
    ] as $key => $value) {
        $metrics->addAttribute($key, (string) $value);
    }
}
if (!$xml->asXML($artifacts . '/coverage.xml') || file_put_contents(
    $artifacts . '/coverage-methods.json',
    json_encode($methods, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR) . "\n"
) === false) {
    fwrite(STDERR, "ERROR: could not write merged coverage artifacts.\n");
    exit(1);
}
