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
        .join("ct-print")
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
/// the produced `.ct` container to JSON via `ct-print --json` and assert
/// on the textual representation.
///
/// Pre-2026-05-08 the recorder shipped a `--format json` mode and a
/// trace.json file was written directly.  The convention now mandates
/// CTFS-only output; `ct print` is the canonical conversion tool.  See
/// `Recorder-CLI-Conventions.md` §4.
///
/// The Move recorder's variable payload (typed values encoded as
/// `ValueRecord::Int { i, type_id }` for u8/u16/u32/u64/u128 and
/// `ValueRecord::String` for u256/address/struct/vector — see
/// `converter::convert_move_value`) does not round-trip cleanly
/// through `ct print --json` today (same pre-existing limitation as
/// cardano / circom / flow / fuel / leo / miden), so this test asserts
/// on **structural anchors** — the fixture's source path file name
/// and at least one of the Move function names — rather than on
/// integer/typed values.
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

    // ct-print --json <file.ct>
    let output = Command::new(&ct_print)
        .args(["--json"])
        .arg(ct_path)
        .output()
        .expect("failed to run ct-print");

    assert!(
        output.status.success(),
        "ct-print should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.is_empty(), "ct-print --json produced empty output");

    // Structural anchor 1: the fixture source path name appears in the
    // path stream rendered by ct-print.
    assert!(
        stdout.contains("flow_test.move"),
        "ct-print --json output should mention the fixture source path \
         (flow_test.move); got:\n{stdout}"
    );

    // Structural anchor 2: at least one of the Move function names from
    // the fixture's `flow_test` module should appear.  The fixture
    // captured `test_computation`; the converter also emits the
    // synthetic `<toplevel>` frame that wraps every recording.
    let fn_anchor = ["test_computation", "toplevel"]
        .iter()
        .any(|v| stdout.contains(v));
    assert!(
        fn_anchor,
        "ct-print --json output should mention at least one Move function \
         name (test_computation/toplevel); got:\n{stdout}"
    );
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
