//! Checks for the external tools the integration tests depend on.
//!
//! Several tests cannot assert anything without a tool that lives outside
//! this crate:
//!
//! * `ct-print`, the `.ct` container inspector built in the sibling
//!   `codetracer-trace-format-nim` checkout. The tests decode every recorded
//!   trace with it and make their assertions on its JSON output.
//! * the `sui` CLI, which compiles the Move test packages and captures their
//!   execution traces. The Nix dev shell (`nix develop`) provides it.
//!
//! **A missing prerequisite fails the test; it never passes it.** These
//! tests used to print a `SKIP:` line and return early when a tool was
//! absent. The harness counts such a test as passed, so a machine without
//! `ct-print` reported the whole suite green in a fraction of a second while
//! asserting nothing at all, which is indistinguishable from a real pass.
//! The checks below panic instead, naming the missing tool and how to
//! provide it. There is deliberately no environment variable that turns them
//! back into skips: a run that cannot check the recorder must say so.
//!
//! Each `require_*` function has a `*_at` variant that takes the location
//! to check, so `tests/test_prerequisites.rs` can prove the failure path
//! against a tool that is certainly absent.

// Every test binary compiles its own copy of this module and uses only the
// checks it needs, so the rest would otherwise be reported as dead code.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the tests expect `ct-print`: the root of the sibling
/// `codetracer-trace-format-nim` checkout, next to this repository. That
/// repository's build writes the binary there (with the platform's
/// executable suffix, `.exe` on Windows).
pub fn ct_print_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("codetracer-trace-format-nim")
        .join(format!("ct-print{}", std::env::consts::EXE_SUFFIX))
}

/// Returns the path to `ct-print`, panicking when it has not been built.
///
/// `test_name` names the calling test in the failure message.
pub fn require_ct_print(test_name: &str) -> PathBuf {
    require_ct_print_at(ct_print_path(), test_name)
}

/// [`require_ct_print`] against an explicit location.
pub fn require_ct_print_at(path: PathBuf, test_name: &str) -> PathBuf {
    assert!(
        path.is_file(),
        "{test_name} requires ct-print at {}, and it is missing. Build it with \
         `just build-ct-print` in this repository, or with `nimble buildCtPrint` \
         in the sibling codetracer-trace-format-nim checkout. The test fails \
         rather than skips, because a skipped test would report a pass that \
         asserted nothing.",
        path.display()
    );
    path
}

/// Panics unless the `sui` CLI is on `PATH` and answers `sui --version`.
///
/// `test_name` names the calling test in the failure message.
pub fn require_sui(test_name: &str) {
    require_sui_cli_at(Path::new("sui"), test_name);
}

/// [`require_sui`] against an explicit program (a bare name is looked up on
/// `PATH`, exactly as `Command::new` does).
pub fn require_sui_cli_at(program: &Path, test_name: &str) {
    // Distinguish "could not run it" from "it ran and failed": both are
    // fatal, but the message should say which one happened.
    let problem = match Command::new(program).arg("--version").output() {
        Ok(output) if output.status.success() => return,
        Ok(output) => format!(
            "`{} --version` exited with {}; stderr: {}",
            program.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(err) => format!("`{}` could not be run: {err}", program.display()),
    };
    panic!(
        "{test_name} requires the `sui` CLI: {problem}. Run the tests inside the \
         Nix dev shell (`nix develop`), which provides it. The test fails rather \
         than skips, because a skipped test would report a pass that asserted \
         nothing."
    );
}
