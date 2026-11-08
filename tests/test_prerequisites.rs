//! Tests for the prerequisite checks in `tests/common/prerequisites.rs`.
//!
//! The other integration tests rely on those checks to fail loudly when
//! `ct-print` or `sui` is missing, instead of returning early and being
//! counted as passes. These tests pin that behaviour: each check must panic
//! for a tool that is certainly absent, and must name the tool and the
//! calling test in its message so the failure says what to fix.
//!
//! No mocks: the "absent" cases point the real checks at a path inside a
//! fresh empty temporary directory, and the "present" case uses a real file.

mod common;

use std::panic;

use common::prerequisites::{require_ct_print_at, require_sui_cli_at};

/// Runs `f`, asserts that it panicked, and returns the panic message.
fn panic_message(f: impl FnOnce() + panic::UnwindSafe) -> String {
    let payload = panic::catch_unwind(f).expect_err("the prerequisite check should have panicked");
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else {
        panic!("the prerequisite check panicked with a non-string payload")
    }
}

#[test]
fn missing_ct_print_fails_the_test() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("ct-print");

    let msg = panic_message(|| {
        require_ct_print_at(missing.clone(), "some_test");
    });

    assert!(msg.contains("some_test"), "message names the test: {msg}");
    assert!(
        msg.contains(&missing.display().to_string()),
        "message names the missing path: {msg}"
    );
    assert!(
        msg.contains("just build-ct-print"),
        "message says how to build ct-print: {msg}"
    );
}

#[test]
fn present_ct_print_is_returned() {
    // Positive control: the check is not simply panicking on everything.
    let dir = tempfile::tempdir().expect("tempdir");
    let present = dir.path().join("ct-print");
    std::fs::write(&present, b"").expect("create placeholder file");

    assert_eq!(require_ct_print_at(present.clone(), "some_test"), present);
}

#[test]
fn a_directory_is_not_ct_print() {
    // A directory at the expected path would only fail later, when the
    // test tries to execute it; the check rejects it up front.
    let dir = tempfile::tempdir().expect("tempdir");

    let msg = panic_message(|| {
        require_ct_print_at(dir.path().to_path_buf(), "some_test");
    });
    assert!(msg.contains("requires ct-print"), "{msg}");
}

#[test]
fn missing_sui_fails_the_test() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("sui");

    let msg = panic_message(|| require_sui_cli_at(&missing, "some_test"));

    assert!(msg.contains("some_test"), "message names the test: {msg}");
    assert!(
        msg.contains("could not be run"),
        "message gives the cause: {msg}"
    );
    assert!(
        msg.contains("nix develop"),
        "message says where sui comes from: {msg}"
    );
}
