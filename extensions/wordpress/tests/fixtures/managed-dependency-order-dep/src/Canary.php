<?php
namespace HomeboyManagedDependencyOrder;

final class Canary {
    public static function value(): string {
        return 'managed-dependency-loaded';
    }
}
