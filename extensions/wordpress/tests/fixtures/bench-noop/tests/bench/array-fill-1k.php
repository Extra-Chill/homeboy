<?php
/**
 * Array fill bench workload — tiny synthetic load so two scenarios round-
 * trip through the harness (validates per-workload isolation + correct
 * scenario ordering in the BenchResults envelope).
 */
return function (): array {
    $arr = array_fill(0, 1000, 'x');
    $count = count($arr);
    return ['filled' => $count];
};
