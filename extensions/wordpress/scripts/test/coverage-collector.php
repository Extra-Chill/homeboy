<?php
/** Inherited via auto_prepend_file by each host PHP test process. */
\pcov\start();
register_shutdown_function(static function () {
    // Inclusive requires an explicit filename filter in pcov 1.0.x. It still
    // includes unexecuted executable lines in every compiled/included file.
    $path = getenv('HOMEBOY_PCOV_MAP_DIR') . '/' . getmypid() . '.json';
    if (file_put_contents($path, json_encode(\pcov\collect(\pcov\inclusive, get_included_files()), JSON_THROW_ON_ERROR)) === false) {
        fwrite(STDERR, "ERROR: could not write pcov process coverage: {$path}\n");
        exit(1);
    }
});
