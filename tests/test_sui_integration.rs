//! Integration test that runs `sui move test --trace` on the flow_test
//! Move package and verifies the recorder converts the real NDJSON trace
//! output into correct CodeTracer format.
//!
//! This test requires the `sui` CLI to be available in PATH. If `sui` is
//! not found, the test is skipped with a clear message rather than failing.

use std::path::{Path, PathBuf};
use std::process::Command;

use codetracer_move_recorder::converter;
use codetracer_move_recorder::source_map::SourceMapResolver;

/// Canonical CTFS container magic bytes.  Mirrors the constant
/// `CTFS_MAGIC` in `codetracer-trace-format-spec/src/container.rs`.
const CTFS_MAGIC: [u8; 5] = [0xC0, 0xDE, 0x72, 0xAC, 0xE2];

/// Path to the Sui Move package this test compiles with `sui move test`.
///
/// This is a *dedicated* package holding only `flow_test.move` -- a
/// Sui-edition-2024-valid module.  It is intentionally separate from the
/// broader `test-programs/move/flow_test` corpus: that corpus also carries
/// generic-Move and Aptos fixtures (resource structs without a Sui `UID`
/// field, `aptos_std` imports, deprecated-edition constructs) which the
/// recorder consumes directly as source but which `sui move test` -- which
/// compiles *every* file under `sources/` -- cannot build.  Pointing this
/// integration test at the mixed corpus made `sui move test` fail to
/// compile regardless of the host OS or Sui version.
const FLOW_TEST_PACKAGE: &str = "test-programs/move/sui_flow_test";

/// Find the project root by looking for Cargo.toml from CARGO_MANIFEST_DIR.
fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Check whether the `sui` CLI is available.
fn sui_is_available() -> bool {
    Command::new("sui")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run `sui move test --trace full` on the given package directory.
/// Each test's Move trace v3 NDJSON is written to a `traces/` directory at
/// the package root.
///
/// The Sui CLI renamed the unit-test tracing flag from the original boolean
/// `--trace-execution` to `--trace [<MODE>]`.  `--trace full` is the
/// equivalent and emits the externally-tagged v3 trace events that
/// `move_types::TraceEvent` (and `converter::convert_trace`) are written
/// against (Sui >= 1.68).
fn run_sui_move_test_trace(package_dir: &Path) -> (bool, String, String) {
    let output = Command::new("sui")
        .args(["move", "test", "--trace", "full"])
        .current_dir(package_dir)
        .output()
        .expect("failed to execute sui move test");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    (output.status.success(), stdout, stderr)
}

/// Recursively find all `.json` trace files under a given directory.
fn find_trace_files(dir: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    if !dir.exists() {
        return results;
    }
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() {
            // Sui trace files are NDJSON, typically named with the test function
            // and stored under build/<package>/traces/
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.ends_with(".json") || name.ends_with(".json.zst") {
                results.push(path.to_path_buf());
            }
        }
    }
    results
}

/// Find the Move trace v3 NDJSON files emitted by `sui move test --trace`.
///
/// Current Sui releases write one `<pkg>__<module>__<test>.json.zst` file
/// per test into a `traces/` directory at the *package root*.  Older
/// layouts placed them under `build/<PackageName>/traces/`.  Search both,
/// preferring the package-root `traces/` dir.
///
/// The `build/` tree also contains compiler-emitted `debug_info/*.json`
/// files whose first line is `{"version":2,...}`; those are *not* execution
/// traces, so the discovery is restricted to `traces/` directories rather
/// than scanning every `{"version":` JSON under `build/`.
fn find_sui_trace_files(package_dir: &Path) -> Vec<PathBuf> {
    let mut trace_dirs = vec![package_dir.join("traces")];
    let build_dir = package_dir.join("build");
    if build_dir.exists() {
        for entry in walkdir::WalkDir::new(&build_dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_dir() && entry.file_name() == "traces" {
                trace_dirs.push(entry.path().to_path_buf());
            }
        }
    }

    let mut trace_files = Vec::new();
    for dir in trace_dirs {
        for f in find_trace_files(&dir) {
            if !trace_files.contains(&f) {
                trace_files.push(f);
            }
        }
    }
    trace_files
}

/// Locate the recorder's CTFS `.ct` bundle inside an output directory.
fn read_ct_container(out_dir: &Path) -> Vec<u8> {
    let entries: Vec<_> = std::fs::read_dir(out_dir)
        .expect("read output dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "ct"))
        .collect();
    assert!(
        !entries.is_empty(),
        "expected at least one .ct file in {}",
        out_dir.display()
    );
    std::fs::read(&entries[0]).expect("read .ct file")
}

// =============================================================================
// Tests
// =============================================================================

#[test]
fn test_sui_move_trace_integration() {
    // Skip if sui is not available.
    if !sui_is_available() {
        eprintln!(
            "SKIPPED: sui CLI not found in PATH. \
             Install the Sui CLI to run this integration test. \
             See: https://docs.sui.io/build/install"
        );
        return;
    }

    let root = project_root();
    let package_dir = root.join(FLOW_TEST_PACKAGE);
    assert!(
        package_dir.exists(),
        "flow_test package not found at {}",
        package_dir.display()
    );
    assert!(
        package_dir.join("Move.toml").exists(),
        "Move.toml not found in flow_test package"
    );

    // ---- Step 1: Run `sui move test --trace` --------------------------------
    let (success, stdout, stderr) = run_sui_move_test_trace(&package_dir);
    assert!(
        success,
        "sui move test --trace-execution failed.\nstdout: {stdout}\nstderr: {stderr}"
    );

    // Verify that the test output mentions passing tests.
    let combined_output = format!("{stdout}{stderr}");
    assert!(
        combined_output.contains("PASS") || combined_output.contains("pass"),
        "sui move test output should indicate passing tests.\nstdout: {stdout}\nstderr: {stderr}"
    );

    // ---- Step 2: Find the trace files ---------------------------------------
    let trace_files = find_sui_trace_files(&package_dir);
    assert!(
        !trace_files.is_empty(),
        "No trace files found after running sui move test --trace-execution. \
         Looked in: {}/build/\nstdout: {stdout}\nstderr: {stderr}",
        package_dir.display()
    );

    eprintln!(
        "Found {} trace file(s): {:?}",
        trace_files.len(),
        trace_files
    );

    // ---- Step 3: Convert each trace file through the recorder ---------------
    for trace_file in &trace_files {
        eprintln!("Processing trace file: {}", trace_file.display());

        let trace_data = std::fs::read(trace_file).expect("failed to read trace file");

        // Decompress if needed.
        let trace_bytes = if trace_file.extension().is_some_and(|ext| ext == "zst") {
            let mut decoder =
                zstd::Decoder::new(trace_data.as_slice()).expect("failed to create zstd decoder");
            let mut decompressed = Vec::new();
            std::io::Read::read_to_end(&mut decoder, &mut decompressed)
                .expect("failed to decompress trace file");
            decompressed
        } else {
            trace_data
        };

        // Verify the trace data starts with a version header.
        let trace_text = std::str::from_utf8(&trace_bytes).expect("trace data should be UTF-8");
        let first_line = trace_text
            .lines()
            .next()
            .expect("trace file should not be empty");
        assert!(
            first_line.contains("\"version\""),
            "First line of trace file should be a version header, got: {first_line}"
        );

        // Use an empty source map (matching the current M2 behavior — source
        // map parsing is a later milestone). The converter should still produce
        // valid output with call/return/variable events even without line mapping.
        let source_map = SourceMapResolver::empty();
        let source_path = package_dir.join("sources/flow_test.move");

        let tmp = tempfile::TempDir::new().expect("failed to create temp dir");
        let out_dir = tmp.path().join("ct-out");

        converter::convert_trace(&trace_bytes, &source_map, &source_path, &out_dir)
            .unwrap_or_else(|e| panic!("convert_trace failed for {}: {e}", trace_file.display()));

        // ---- Step 4: Verify the .ct CTFS bundle was produced ----------------
        // The recorder is CTFS-only: no `trace.json` is written.  Use
        // `ct print --json` from `codetracer-trace-format-nim` for
        // human-readable conversion of the produced bundle.
        let bytes = read_ct_container(&out_dir);
        assert!(
            bytes.len() >= CTFS_MAGIC.len(),
            ".ct file should have at least 5 bytes for the magic header"
        );
        assert_eq!(
            &bytes[..CTFS_MAGIC.len()],
            &CTFS_MAGIC,
            ".ct file should start with CTFS magic bytes (C0 DE 72 AC E2), got {:02X?}",
            &bytes[..CTFS_MAGIC.len()]
        );

        // The container is materially populated (more than just the magic
        // header + minimal stream framing).  Pre-2026-05-08 the recorder
        // emitted ~3 KiB CTFS files at this fixture; lower bound is a
        // generous safety margin against accidental empty-trace regressions.
        assert!(
            bytes.len() > 256,
            "CTFS bundle for {} is suspiciously small ({} bytes); \
             converter may be silently dropping events",
            trace_file.display(),
            bytes.len()
        );

        // ---- Step 5: Verify metadata ----------------------------------------
        // Legacy `trace_metadata.json` sidecar was retired with the v3
        // CTFS rollout (follow-up #254 phase 2); program metadata now
        // lives in `meta.dat` inside the `.ct` container.  The container
        // size assertion above is the equivalent integrity check.
        let _ = out_dir;
    }
}

/// Test that the NDJSON trace data from `sui move test --trace` can be parsed
/// by our TraceEvent deserializer for every line.
#[test]
fn test_sui_trace_ndjson_parsing() {
    if !sui_is_available() {
        eprintln!(
            "SKIPPED: sui CLI not found in PATH. \
             Install the Sui CLI to run this integration test."
        );
        return;
    }

    let root = project_root();
    let package_dir = root.join(FLOW_TEST_PACKAGE);

    let (success, stdout, stderr) = run_sui_move_test_trace(&package_dir);
    assert!(
        success,
        "sui move test failed.\nstdout: {stdout}\nstderr: {stderr}"
    );

    let trace_files = find_sui_trace_files(&package_dir);
    assert!(!trace_files.is_empty(), "No trace files found");

    // Aggregate event-kind tallies across every trace file.  A *single*
    // trace need not exercise every event kind -- e.g. a Move test that
    // `abort`s emits an `OpenFrame` but no matching `CloseFrame`, because
    // the abort terminates the frame.  The deserializer-coverage assertions
    // therefore hold across the whole trace set rather than per file.
    let mut total_open = 0usize;
    let mut total_close = 0usize;
    let mut total_instruction = 0usize;
    let mut total_effect = 0usize;

    for trace_file in &trace_files {
        let trace_data = std::fs::read(trace_file).expect("failed to read trace file");
        let trace_bytes = if trace_file.extension().is_some_and(|ext| ext == "zst") {
            let mut decoder = zstd::Decoder::new(trace_data.as_slice()).expect("zstd decoder");
            let mut decompressed = Vec::new();
            std::io::Read::read_to_end(&mut decoder, &mut decompressed).expect("decompress");
            decompressed
        } else {
            trace_data
        };

        let text = std::str::from_utf8(&trace_bytes).expect("UTF-8");
        let mut lines_iter = text.lines();

        // Parse version header.
        let header_line = lines_iter.next().expect("trace should have a header line");
        let header: codetracer_move_recorder::move_types::VersionHeader =
            serde_json::from_str(header_line)
                .unwrap_or_else(|e| panic!("failed to parse header '{header_line}': {e}"));
        assert_eq!(
            header.version, 3,
            "trace version should be 3, got {}",
            header.version
        );

        // Parse every remaining line as a TraceEvent.
        let mut event_count = 0;
        let mut open_frame_count = 0;
        let mut close_frame_count = 0;
        let mut instruction_count = 0;
        let mut effect_count = 0;

        for (i, line) in lines_iter.enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let event: codetracer_move_recorder::move_types::TraceEvent =
                serde_json::from_str(line).unwrap_or_else(|e| {
                    panic!(
                        "failed to parse trace event at line {} of {}: {e}\nline: {line}",
                        i + 2,
                        trace_file.display()
                    )
                });

            match event {
                codetracer_move_recorder::move_types::TraceEvent::OpenFrame { .. } => {
                    open_frame_count += 1
                }
                codetracer_move_recorder::move_types::TraceEvent::CloseFrame { .. } => {
                    close_frame_count += 1
                }
                codetracer_move_recorder::move_types::TraceEvent::Instruction { .. } => {
                    instruction_count += 1
                }
                codetracer_move_recorder::move_types::TraceEvent::Effect { .. } => {
                    effect_count += 1
                }
                codetracer_move_recorder::move_types::TraceEvent::External { .. } => {}
            }

            event_count += 1;
        }

        eprintln!(
            "Trace file {} parsed successfully: {} events \
             (open_frame={open_frame_count}, close_frame={close_frame_count}, \
             instructions={instruction_count}, effects={effect_count})",
            trace_file.display(),
            event_count
        );

        // Per-file sanity: every Move test opens its entry frame and runs
        // at least one instruction, so these always hold.
        assert!(event_count > 0, "trace should contain at least one event");
        assert!(
            open_frame_count > 0,
            "trace should contain at least one OpenFrame"
        );
        assert!(
            instruction_count > 0,
            "trace should contain at least one Instruction"
        );
        // A frame can close at most once per open; an aborting test leaves
        // `close < open` (the abort terminates the frame uncleanly), so the
        // invariant is `<=`, not strict equality.
        assert!(
            close_frame_count <= open_frame_count,
            "CloseFrame count ({close_frame_count}) must not exceed \
             OpenFrame count ({open_frame_count})"
        );

        total_open += open_frame_count;
        total_close += close_frame_count;
        total_instruction += instruction_count;
        total_effect += effect_count;
    }

    // Across the whole trace set every event kind the deserializer models
    // must have been exercised at least once.
    assert!(total_open > 0, "no OpenFrame events across any trace");
    assert!(total_close > 0, "no CloseFrame events across any trace");
    assert!(
        total_instruction > 0,
        "no Instruction events across any trace"
    );
    assert!(total_effect > 0, "no Effect events across any trace");
}
