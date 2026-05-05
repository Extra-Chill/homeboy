<?php
/**
 * Procedural helper — registers the demo artifact types. Already covered by
 * `convention_exception_globs` `**\/register-*.php`.
 */

Demo_Package_Artifacts_Registry::instance()->register(
	new Demo_Package_Artifact_Type( 'demo', array(), array() )
);
