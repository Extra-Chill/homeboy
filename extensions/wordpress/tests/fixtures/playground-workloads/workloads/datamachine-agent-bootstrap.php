<?php

update_option( 'homeboy_datamachine_agent_bootstrap_smoke', 'ran', false );

return array(
    'metrics'  => array(
        'bootstrap_ran' => 1,
    ),
    'metadata' => array(
        'bootstrap_value' => get_option( 'homeboy_datamachine_agent_bootstrap_smoke' ),
    ),
);
