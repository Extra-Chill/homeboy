<?php

final class Demo_Event {
	public const TOOL_CALL = 'tool_call';

	public function dispatch(): array {
		do_action( 'tool_call', $this );

		return array(
			'tool_call' => true,
			'type'      => 'tool_call',
			'matches'   => self::TOOL_CALL === 'tool_call',
		);
	}
}
