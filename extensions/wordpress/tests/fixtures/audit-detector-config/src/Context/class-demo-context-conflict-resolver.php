<?php
/**
 * Resolver contract for context conflict decisions.
 */

interface Demo_Context_Conflict_Resolver {
 public function resolve( array $items, array $context = array() ): array;
}
