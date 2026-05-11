<?php
return [
    'success' => false,
    'reward' => 0.5,
    'done' => true,
    'grade' => [
        'score' => 0.5,
        'max_score' => 1,
        'checks' => [
            [
                'id' => 'created_post',
                'passed' => true,
                'score' => 0.5,
                'max_score' => 0.5,
            ],
            [
                'id' => 'matches_expected_structure',
                'passed' => false,
                'score' => 0,
                'max_score' => 0.5,
                'message' => 'Navigation block was missing.',
            ],
        ],
    ],
];
