//! Support code shared by the integration-test binaries under `tests/`.
//!
//! Cargo compiles every `tests/*.rs` file as its own crate, so a test file
//! pulls this module in with `mod common;` and gets a private copy of it.

pub mod prerequisites;
