<?php
/** Legacy workload return arrays without a metrics key remain metadata-free. */
return function (): array {
    return ['kind' => 'legacy'];
};
