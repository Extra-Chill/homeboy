//! Unit tests for the agent-task scheduler engine, split by concern:
//! `scheduling_tests` covers dispatch/concurrency/dependency behavior and
//! `outcome_tests` covers outcome normalization and failed-status detection.
//! `fixtures` holds the shared mock executors and builders.

#![cfg(test)]

mod fixtures;
mod outcome_tests;
mod scheduling_tests;
