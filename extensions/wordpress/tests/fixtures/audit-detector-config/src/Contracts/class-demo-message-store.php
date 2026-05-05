<?php

interface Demo_Message_Store {
	public function get( string $id ): ?array;
}
