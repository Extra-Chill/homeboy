<?php

namespace Demo\Api;

final class ToolAccessPolicy {
	public function can_use_tool( string $tool_name ): bool {
		return '' !== $tool_name;
	}
}
