<?php
/**
 * Adopter contract — interface, distinct PHP role from the value-object
 * siblings.
 */

interface Demo_Package_Adopter {
	public function diff( Demo_Package $package ): Demo_Package_Adoption_Diff;

	public function adopt( Demo_Package $package ): Demo_Package_Adoption_Result;
}
