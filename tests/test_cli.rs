//! CLI-surface integration tests for `codetracer-move-recorder`.
//!
//! Tests cover three areas:
//!
//! 1. **Smoke tests** — basic `--help`, `--version`, error paths.
//! 2. **`ct print` content** — record a fixture and pipe the resulting
//!    `.ct` container through `ct-print --json` from
//!    `codetracer-trace-format-nim` to make content-level assertions.
//!    Skips gracefully when `ct-print` is not present (i.e. when this
//!    crate is built outside the metacraft workspace).
//! 3. **CLI env-var contract** — exercise the post-2026-05-08
//!    `CODETRACER_MOVE_RECORDER_OUT_DIR` /
//!    `CODETRACER_MOVE_RECORDER_DISABLED` env vars and the
//!    no-`--format` invariant from `Recorder-CLI-Conventions.md` §4 / §5.
//!
//! History note: pre-2026-05-08 the recorder shipped a `--format
//! ctfs|binary|json` flag and the `record_creates_output_files` test
//! passed `--format ctfs` implicitly (it was the default).  When the
//! convention switched to CTFS-only the `--format` argument was
//! removed and the smoke test was rewritten to omit it.  See
//! `AUDIT-CTFS-2026-05.md` ("Convention compliance follow-up — 2026-05-08")
//! for the full record.

use std::path::PathBuf;
use std::process::Command;

/// CTFS magic bytes: C0 DE 72 AC E2.
const CTFS_MAGIC: [u8; 5] = [0xC0, 0xDE, 0x72, 0xAC, 0xE2];

fn cargo_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_codetracer-move-recorder"))
}

/// Path to a real Sui move-trace-format v3 NDJSON fixture (zstd-compressed).
///
/// Captured from `sui move test --trace-execution` against the
/// `flow_test` Move package; small enough to keep in-tree and
/// representative enough to exercise the converter's full code path
/// (multiple frames, instructions, effects).
fn flow_test_trace_fixture() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test-programs/move/flow_test/traces/\
         flow_test__flow_test__test_computation.json.zst"
    ))
}

/// Path to the corresponding `.move` source file.
fn flow_test_source_path() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test-programs/move/flow_test/sources/flow_test.move"
    ))
}

/// Path to the `ct-print` binary shipped with `codetracer-trace-format-nim`.
///
/// The Move recorder is CTFS-only; tests that need to make content-level
/// assertions on a recorded trace pipe the `.ct` container through
/// `ct-print --json` and assert on the resulting JSON.  This is the
/// same workflow that `Recorder-CLI-Conventions.md` §4 prescribes for
/// downstream tools / golden snapshots.
fn ct_print_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("codetracer-trace-format-nim")
        .join(format!("ct-print{}", std::env::consts::EXE_SUFFIX))
}

// ===========================================================================
// Smoke tests
// ===========================================================================

#[test]
fn help_succeeds_and_mentions_name() {
    let output = cargo_bin().arg("--help").output().expect("failed to run");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("codetracer-move-recorder"),
        "Help output should mention codetracer-move-recorder, got: {stdout}"
    );
}

#[test]
fn version_succeeds_and_contains_version() {
    let output = cargo_bin()
        .arg("--version")
        .output()
        .expect("failed to run");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("0.1.0"),
        "Version output should contain 0.1.0, got: {stdout}"
    );
}

#[test]
fn record_nonexistent_file_fails() {
    let output = cargo_bin()
        .args(["record", "/nonexistent/path/trace.json.zst"])
        .output()
        .expect("failed to run");
    assert!(
        !output.status.success(),
        "record with nonexistent file should fail"
    );
}

#[test]
fn record_creates_output_files() {
    let tmp = tempfile::TempDir::new().expect("failed to create temp dir");

    // Write a minimal valid NDJSON trace file (not compressed, .json extension).
    let trace_file = tmp.path().join("dummy_trace.json");
    let trace_data = "{\"version\":3}\n{\"OpenFrame\":{\"frame\":{\"frame_id\":1,\"function_name\":\"main\",\"module\":{\"address\":\"0x0\",\"name\":\"test\"},\"type_instantiation\":[],\"parameters\":[],\"return_types\":[],\"locals_types\":[],\"is_native\":false},\"gas_left\":1000}}\n{\"CloseFrame\":{\"frame_id\":1,\"return_\":[],\"gas_left\":900}}\n";
    std::fs::write(&trace_file, trace_data).expect("failed to write dummy trace");

    let out_dir = tmp.path().join("ct-traces");

    let output = cargo_bin()
        .args([
            "record",
            "-o",
            out_dir.to_str().unwrap(),
            trace_file.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run");

    assert!(
        output.status.success(),
        "record should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify .ct output with CTFS magic bytes.
    let ct_files: Vec<_> = std::fs::read_dir(&out_dir)
        .expect("read output dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "ct"))
        .collect();
    assert!(
        !ct_files.is_empty(),
        "expected at least one .ct file in output dir"
    );
    let content = std::fs::read(&ct_files[0]).expect("read .ct file");
    assert!(content.len() >= CTFS_MAGIC.len(), ".ct file too small");
    assert_eq!(
        &content[..CTFS_MAGIC.len()],
        &CTFS_MAGIC,
        "CTFS magic bytes mismatch"
    );
}

// ===========================================================================
// CTFS content via `ct-print` — replaces the legacy `--format json` content
// assertions
// ===========================================================================

/// Record the bundled `flow_test` Sui-format NDJSON fixture, then convert
/// the produced `.ct` container to JSON via `ct-print` and assert on:
///
/// 1. **Structural anchors** (legacy layer): `ct-print --json` output
///    contains the source filename and at least one Move function name
///    somewhere in the textual rendering.
/// 2. **Exact decoded values** (the layer enabled by `ct-print --full`):
///    the `flow_test::test_computation` Move function executes the let
///    bindings `a = 10`, `b = 32`, `sum_val = a + b = 42`,
///    `doubled = sum_val * 2 = 84`, `final_result = doubled + a = 94`.
///    The Move recorder surfaces locals via `Effect::Read`/`Effect::Write`
///    using source-level identifiers when the Sui Move compiler's
///    debug-info JSON sidecar at
///    `<package_root>/build/<PackageName>/debug_info/<Module>.json` is
///    available (see `crate::move_debug_info`); for hand-rolled NDJSON
///    fixtures without a `build/` directory the recorder falls back to
///    synthetic `local_<N>` slot names.  Stack pushes/pops surface as
///    `stack_top` / `popped` regardless.  Each binding must surface in
///    the trace as a step variable with a decoded `Int` ValueRecord
///    whose `i` field matches the literal value from the source program.
///
/// Pre-2026-05-08 the recorder shipped a `--format json` mode and a
/// trace.json file was written directly.  The convention now mandates
/// CTFS-only output; `ct print` is the canonical conversion tool.  See
/// `Recorder-CLI-Conventions.md` §4.  `ct-print --full` (added 2026-05
/// in `codetracer-trace-format-nim`) is what enables the exact-value
/// layer — its output is a deterministic JSON document with every CBOR
/// `ValueRecord` decoded to a structured form like
/// `{"kind":"Int","i":42,"type_id":N}`.
///
/// The Move recorder's note about `Variable` integer payloads not
/// round-tripping through `ct-print --json` is empirically obsolete
/// for `--full`: the `register_variable_with_full_value` path decodes
/// back to `{"kind":"Int","i":<n>,"type_id":N}` with values intact.
/// Variables not directly representable as small ints (e.g. booleans
/// produced by comparison opcodes) surface as `{"kind":"Raw","r":"true"}`,
/// which is why the strict layer below filters to `Int` payloads
/// before doing arithmetic comparisons.
#[test]
fn test_recorded_trace_via_ct_print_json() {
    let ct_print = ct_print_path();
    if !ct_print.exists() {
        eprintln!(
            "SKIP: ct-print not found at {} — only available within the \
             metacraft workspace where codetracer-trace-format-nim is a sibling.",
            ct_print.display()
        );
        return;
    }

    let trace_fixture = flow_test_trace_fixture();
    if !trace_fixture.exists() {
        eprintln!(
            "SKIP: flow_test trace fixture not found at {}.",
            trace_fixture.display()
        );
        return;
    }

    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let out_dir = tmp.path().join("traces");

    // Drive the recorder through the binary entry point so the test
    // exercises the full CLI surface (and the new env-var contract).
    let output = cargo_bin()
        .args(["record"])
        .args(["--out-dir"])
        .arg(&out_dir)
        .args(["--source"])
        .arg(flow_test_source_path())
        .arg(&trace_fixture)
        .env_remove("CODETRACER_MOVE_RECORDER_DISABLED")
        .env_remove("CODETRACER_MOVE_RECORDER_OUT_DIR")
        .output()
        .expect("failed to run recorder");

    assert!(
        output.status.success(),
        "recorder should succeed on flow_test fixture; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let ct_files: Vec<_> = std::fs::read_dir(&out_dir)
        .expect("failed to read output directory")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "ct"))
        .collect();
    assert!(
        !ct_files.is_empty(),
        "expected at least one .ct file in {}",
        out_dir.display()
    );
    let ct_path = &ct_files[0];

    // -----------------------------------------------------------------
    // Layer 1 (legacy): ct-print --json — substring presence checks.
    // Kept as a safety net so a regression in the textual rendering
    // is caught even if --full's JSON shape evolves.
    // -----------------------------------------------------------------
    let output = Command::new(&ct_print)
        .args(["--json"])
        .arg(ct_path)
        .output()
        .expect("failed to run ct-print");

    assert!(
        output.status.success(),
        "ct-print --json should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout_json = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout_json.is_empty(),
        "ct-print --json produced empty output"
    );

    // Structural anchor 1: the fixture source path name appears in the
    // path stream rendered by ct-print.
    assert!(
        stdout_json.contains("flow_test.move"),
        "ct-print --json output should mention the fixture source path \
         (flow_test.move); got:\n{stdout_json}"
    );

    // Structural anchor 2: at least one of the Move function names from
    // the fixture's `flow_test` module should appear.  The fixture
    // captured `test_computation`; the converter also emits the
    // synthetic `<toplevel>` frame that wraps every recording.
    let fn_anchor = ["test_computation", "toplevel"]
        .iter()
        .any(|v| stdout_json.contains(v));
    assert!(
        fn_anchor,
        "ct-print --json output should mention at least one Move function \
         name (test_computation/toplevel); got:\n{stdout_json}"
    );

    // -----------------------------------------------------------------
    // Layer 2 (the upgrade): ct-print --full — exact decoded values.
    // -----------------------------------------------------------------
    let full_output = Command::new(&ct_print)
        .args(["--full", "--strip-paths"])
        .arg(ct_path)
        .output()
        .expect("failed to run ct-print --full");

    assert!(
        full_output.status.success(),
        "ct-print --full should succeed; stderr: {}",
        String::from_utf8_lossy(&full_output.stderr)
    );

    let doc: serde_json::Value = serde_json::from_slice(&full_output.stdout)
        .expect("ct-print --full should emit valid JSON");

    // ----- Function table: `test_computation` must appear -------------
    // The Move recorder currently registers function names as bare
    // identifiers (no module qualifier), but downstream tooling may add
    // one (e.g. `flow_test::flow_test::test_computation`), so we use
    // `ends_with` to stay robust against that.
    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        functions.iter().any(|f| f.ends_with("test_computation")),
        "expected `test_computation` in functions table; got {:?}",
        functions
    );

    // ----- Path table: the canonical fixture path must appear ---------
    let paths: Vec<&str> = doc["paths"]
        .as_array()
        .expect("paths array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        paths.iter().any(|p| p.ends_with("flow_test.move")),
        "expected flow_test.move in paths table; got {:?}",
        paths
    );

    // ----- Step / call counts ----------------------------------------
    // The Move converter wraps the bytecode trace in exactly one
    // `call_entry` for `test_computation`.  These are stable properties
    // of the canonical fixture — if they change, that's a real
    // regression to investigate, not a flake.
    let counts = &doc["counts"];
    assert_eq!(
        counts["calls"].as_u64(),
        Some(1),
        "expected 1 call event (test_computation); counts={counts}",
    );

    let events = doc["events"].as_array().expect("events array");

    // ----- Call sequence: only test_computation -----------------------
    let call_sequence: Vec<&str> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .filter_map(|e| e["function"].as_str())
        .collect();
    assert_eq!(
        call_sequence.len(),
        1,
        "expected exactly 1 call_entry event; got {:?}",
        call_sequence
    );
    assert!(
        call_sequence[0].ends_with("test_computation"),
        "expected first call to be `test_computation`; got {:?}",
        call_sequence
    );

    // ----- Exact decoded variable values ------------------------------
    // Collect every (varname, i64) pair surfaced by step events whose
    // value decoded as `Int`.  Move recorders also emit `Raw` payloads
    // for booleans (e.g. comparison opcodes preceding asserts); those
    // are ignored here because this fixture's checked values are all
    // u64-typed integer payloads.
    let observed_vars: Vec<(String, i64)> = events
        .iter()
        .filter(|e| e["kind"] == "step")
        .flat_map(|e| {
            e["vars"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
        })
        .filter_map(|v| {
            let name = v["varname"].as_str()?.to_string();
            let value = v.get("value")?.clone();
            let kind = value["kind"].as_str()?;
            // Only consume Int payloads.  Booleans/structs use other
            // ValueRecord variants and are not part of this fixture's
            // verified set.
            if kind != "Int" {
                return None;
            }
            let i = value["i"].as_i64()?;
            Some((name, i))
        })
        .collect();

    // The Move VM stores let-bindings in numbered local slots; the
    // recorder resolves those slots to source-level identifiers via
    // the package's compiler-emitted debug info JSON sidecar at
    // `<package_root>/build/<PackageName>/debug_info/<Module>.json`
    // (see `crate::move_debug_info`).  For `test_computation`'s body
    // the canonical assignments are (pinned by
    // `function_map["10"].locals == ["a#1#0", "doubled#1#0",
    // "sum_val#1#0"]` in `flow_test.json`, with the compiler-internal
    // `#scope#unique` suffix stripped):
    //   slot 0 (`a`)        = 10
    //   slot 1 (`doubled`)  = 84
    //   slot 2 (`sum_val`)  = 42
    // (`b = 32` is consumed before being stored back into a
    //  long-lived slot, so it surfaces only via the stack stream below.
    //  `final_result = 94` similarly is computed and immediately fed
    //  into the assert, so it surfaces via the stack stream rather than
    //  a dedicated local write.)
    let expected_locals: &[(&str, i64)] = &[("a", 10), ("sum_val", 42), ("doubled", 84)];
    for (name, value) in expected_locals {
        assert!(
            observed_vars.iter().any(|(n, v)| n == name && v == value),
            "expected step variable `{name}` = {value} in --full output; \
             observed = {observed_vars:?}"
        );
    }

    // The full canonical value sequence (10, 32, 42, 84, 94) must
    // surface across the stack-track variables.  This pins down the
    // intermediate values that aren't bound to long-lived locals — in
    // particular `b = 32` and `final_result = 94`.  We require each
    // value to appear at least once in `stack_top` or `popped`.
    let stack_values: Vec<i64> = observed_vars
        .iter()
        .filter(|(n, _)| n == "stack_top" || n == "popped")
        .map(|(_, v)| *v)
        .collect();
    for expected in [10_i64, 32, 42, 84, 94] {
        assert!(
            stack_values.contains(&expected),
            "expected canonical value {expected} to surface in \
             stack_top/popped stream; observed stack values = {stack_values:?}"
        );
    }
}

// ===========================================================================
// CLI env-var contract
// ===========================================================================

/// `CODETRACER_MOVE_RECORDER_OUT_DIR` must be honoured as a fallback
/// for `--out-dir`.  Convention: `Recorder-CLI-Conventions.md` §5.
#[test]
fn test_env_out_dir_used_when_flag_omitted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env_out_dir = tmp.path().join("via-env");

    // Use a minimal in-line trace; the Sui fixture path requires the
    // `--source` argument and we want to test the env-var path
    // independently of source-resolution behaviour.
    let trace_file = tmp.path().join("dummy_trace.json");
    let trace_data = "{\"version\":3}\n{\"OpenFrame\":{\"frame\":{\"frame_id\":1,\"function_name\":\"main\",\"module\":{\"address\":\"0x0\",\"name\":\"test\"},\"type_instantiation\":[],\"parameters\":[],\"return_types\":[],\"locals_types\":[],\"is_native\":false},\"gas_left\":1000}}\n{\"CloseFrame\":{\"frame_id\":1,\"return_\":[],\"gas_left\":900}}\n";
    std::fs::write(&trace_file, trace_data).expect("failed to write dummy trace");

    let output = cargo_bin()
        .args(["record"])
        .arg(&trace_file)
        .env("CODETRACER_MOVE_RECORDER_OUT_DIR", &env_out_dir)
        // Make sure the env-var doesn't bleed in from the developer's shell.
        .env_remove("CODETRACER_MOVE_RECORDER_DISABLED")
        .output()
        .expect("failed to run recorder");

    assert!(
        output.status.success(),
        "recorder should succeed when CODETRACER_MOVE_RECORDER_OUT_DIR is set; \
         stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The env-var-supplied output dir must contain the .ct bundle.
    let ct_files: Vec<_> = std::fs::read_dir(&env_out_dir)
        .unwrap_or_else(|e| {
            panic!("expected env-supplied out-dir {env_out_dir:?} to exist after record: {e}")
        })
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "ct"))
        .collect();
    assert!(
        !ct_files.is_empty(),
        "expected the env-supplied output dir {env_out_dir:?} to receive the .ct trace bundle"
    );
}

/// `CODETRACER_MOVE_RECORDER_DISABLED=1` must skip recording entirely.
/// The recorder process should still exit 0 (the Move recorder doesn't
/// run a separate target subprocess — it consumes a recorded trace
/// file and converts it — so "disabled" simply means "don't write any
/// trace artefacts").
#[test]
fn test_env_disabled_skips_recording() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out_dir = tmp.path().join("should-stay-empty");

    let trace_file = tmp.path().join("dummy_trace.json");
    let trace_data = "{\"version\":3}\n{\"OpenFrame\":{\"frame\":{\"frame_id\":1,\"function_name\":\"main\",\"module\":{\"address\":\"0x0\",\"name\":\"test\"},\"type_instantiation\":[],\"parameters\":[],\"return_types\":[],\"locals_types\":[],\"is_native\":false},\"gas_left\":1000}}\n{\"CloseFrame\":{\"frame_id\":1,\"return_\":[],\"gas_left\":900}}\n";
    std::fs::write(&trace_file, trace_data).expect("failed to write dummy trace");

    let output = cargo_bin()
        .args(["record"])
        .arg(&trace_file)
        .args(["--out-dir"])
        .arg(&out_dir)
        .env("CODETRACER_MOVE_RECORDER_DISABLED", "1")
        .output()
        .expect("failed to run recorder");

    assert!(
        output.status.success(),
        "recorder should succeed in disabled mode; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // No trace artefacts of any kind should have been written.
    let no_artefacts = !out_dir.exists()
        || (std::fs::read_dir(&out_dir)
            .map(|rd| rd.filter_map(|e| e.ok()).next().is_none())
            .unwrap_or(true));
    assert!(
        no_artefacts,
        "no trace artefacts should be written when \
         CODETRACER_MOVE_RECORDER_DISABLED=1; got files in {out_dir:?}"
    );
}

/// `--format` is no longer accepted at any level — clap must reject it.
/// Convention: §4 (CTFS-only).
#[test]
fn test_format_flag_rejected_by_clap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out_dir = tmp.path().join("traces");

    let trace_file = tmp.path().join("dummy_trace.json");
    let trace_data = "{\"version\":3}\n{\"OpenFrame\":{\"frame\":{\"frame_id\":1,\"function_name\":\"main\",\"module\":{\"address\":\"0x0\",\"name\":\"test\"},\"type_instantiation\":[],\"parameters\":[],\"return_types\":[],\"locals_types\":[],\"is_native\":false},\"gas_left\":1000}}\n{\"CloseFrame\":{\"frame_id\":1,\"return_\":[],\"gas_left\":900}}\n";
    std::fs::write(&trace_file, trace_data).expect("failed to write dummy trace");

    let output = cargo_bin()
        .args(["record"])
        .arg(&trace_file)
        .args(["--out-dir"])
        .arg(&out_dir)
        .args(["--format", "json"])
        .output()
        .expect("failed to run recorder");

    assert!(
        !output.status.success(),
        "--format should be rejected by clap; stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--format")
            || stderr.contains("unexpected argument")
            || stderr.contains("unrecognized")
            || stderr.contains("found argument"),
        "clap error should mention the unknown --format flag; got stderr:\n{stderr}"
    );
}

/// The CLI binary must not expose a `--format` flag at any level.
/// Convention: `Recorder-CLI-Conventions.md` §4 — recorders are
/// CTFS-only.
#[test]
fn test_no_format_flag_in_help() {
    for subcmd in [None, Some("record"), Some("replay"), Some("aptos-replay")] {
        let mut cmd = cargo_bin();
        if let Some(s) = subcmd {
            cmd.arg(s);
        }
        cmd.arg("--help");

        let output = cmd.output().expect("failed to run --help");
        assert!(
            output.status.success(),
            "--help (subcmd={subcmd:?}) should exit 0"
        );

        let help = String::from_utf8_lossy(&output.stdout);
        assert!(
            !help.contains("--format"),
            "--help (subcmd={subcmd:?}) must not advertise --format; got:\n{help}"
        );
        assert!(
            !help.contains("CODETRACER_FORMAT"),
            "--help (subcmd={subcmd:?}) must not advertise CODETRACER_FORMAT; got:\n{help}"
        );
    }
}

/// `--help` must mention `ct print` so users know where to go for
/// human-readable conversion of the recorded CTFS bundle.
#[test]
fn test_help_mentions_ct_print() {
    let output = cargo_bin()
        .arg("--help")
        .output()
        .expect("failed to run --help");
    assert!(output.status.success(), "--help should exit 0");

    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("ct print"),
        "--help must mention `ct print` as the conversion tool; got:\n{help}"
    );
}
