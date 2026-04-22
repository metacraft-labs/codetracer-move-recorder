//! Integration test that runs `sui move test --trace` on the flow_test
//! Move package and verifies the recorder converts the real NDJSON trace
//! output into correct CodeTracer format.
//!
//! This test requires the `sui` CLI to be available in PATH. If `sui` is
//! not found, the test is skipped with a clear message rather than failing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use codetracer_trace_types::TraceLowLevelEvent;
use codetracer_trace_writer_nim::TraceEventsFileFormat;

use codetracer_move_recorder::converter;
use codetracer_move_recorder::source_map::SourceMapResolver;

/// Path to the flow_test Move package (relative to the project root).
const FLOW_TEST_PACKAGE: &str = "test-programs/move/flow_test";

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

/// Run `sui move test --trace` on the given package directory.
/// Returns the path to the package directory (traces are written inside
/// the package's `build/` subdirectory).
fn run_sui_move_test_trace(package_dir: &Path) -> (bool, String, String) {
    let output = Command::new("sui")
        .args(["move", "test", "--trace-execution"])
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

/// Find trace NDJSON files in the build directory after running `sui move test --trace`.
/// Sui writes traces to `<package>/build/<package_name>/traces/`.
fn find_sui_trace_files(package_dir: &Path) -> Vec<PathBuf> {
    let build_dir = package_dir.join("build");
    if !build_dir.exists() {
        return Vec::new();
    }

    // Look for trace files in the build directory tree.
    // Sui places them under build/<PackageName>/traces/
    let mut trace_files = find_trace_files(&build_dir);

    // Also check for trace files directly in the traces subdirectories.
    if trace_files.is_empty() {
        // Try a broader search — some versions may put traces elsewhere.
        for entry in walkdir::WalkDir::new(&build_dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.is_file() {
                // Check if file looks like NDJSON trace data (starts with {"version":)
                if let Ok(content) = std::fs::read_to_string(path) {
                    if content.starts_with("{\"version\":") {
                        trace_files.push(path.to_path_buf());
                    }
                }
            }
        }
    }

    trace_files
}

/// Parse a CodeTracer trace.json (JSON format) and return the events.
fn parse_trace_events(trace_bin_path: &Path) -> Vec<TraceLowLevelEvent> {
    let content = std::fs::read_to_string(trace_bin_path).expect("failed to read trace.json");
    serde_json::from_str(&content).expect("trace.json should be valid JSON array")
}

/// Extract function names from the trace events, keyed by function ID index.
fn extract_function_names(events: &[TraceLowLevelEvent]) -> HashMap<usize, String> {
    let mut names = HashMap::new();
    let mut next_id = 0usize;
    for event in events {
        if let TraceLowLevelEvent::Function(func) = event {
            names.insert(next_id, func.name.clone());
            next_id += 1;
        }
    }
    names
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
        let trace_bytes = if trace_file
            .extension()
            .is_some_and(|ext| ext == "zst")
        {
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
        let first_line = trace_text.lines().next().expect("trace file should not be empty");
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

        converter::convert_trace(
            &trace_bytes,
            &source_map,
            &source_path,
            &out_dir,
            TraceEventsFileFormat::Json,
        )
        .unwrap_or_else(|e| {
            panic!(
                "convert_trace failed for {}: {e}",
                trace_file.display()
            )
        });

        // ---- Step 4: Verify the output files exist --------------------------
        assert!(
            out_dir.join("trace.json").exists(),
            "trace.json should exist after conversion"
        );
        assert!(
            out_dir.join("trace_metadata.json").exists(),
            "trace_metadata.json should exist after conversion"
        );
        assert!(
            out_dir.join("trace_paths.json").exists(),
            "trace_paths.json should exist after conversion"
        );

        // ---- Step 5: Parse and verify the CodeTracer trace ------------------
        let events = parse_trace_events(&out_dir.join("trace.json"));
        assert!(
            !events.is_empty(),
            "trace.json should contain events"
        );

        // Count event types.
        let step_count = events
            .iter()
            .filter(|e| matches!(e, TraceLowLevelEvent::Step(_)))
            .count();
        let call_count = events
            .iter()
            .filter(|e| matches!(e, TraceLowLevelEvent::Call(_)))
            .count();
        let return_count = events
            .iter()
            .filter(|e| matches!(e, TraceLowLevelEvent::Return(_)))
            .count();
        let value_count = events
            .iter()
            .filter(|e| matches!(e, TraceLowLevelEvent::Value(_)))
            .count();
        let function_count = events
            .iter()
            .filter(|e| matches!(e, TraceLowLevelEvent::Function(_)))
            .count();

        eprintln!(
            "  Events: total={}, steps={step_count}, calls={call_count}, \
             returns={return_count}, values={value_count}, functions={function_count}",
            events.len()
        );

        // The converter always emits at least one Step (from the toplevel start).
        assert!(
            step_count >= 1,
            "trace should contain at least 1 Step event, got {step_count}"
        );

        // Each test function should produce at least one Call event (toplevel + test function).
        assert!(
            call_count >= 2,
            "trace should contain at least 2 Call events \
             (toplevel + test function), got {call_count}"
        );

        // Each OpenFrame should have a matching CloseFrame, producing Return events.
        assert!(
            return_count >= 1,
            "trace should contain at least 1 Return event, got {return_count}"
        );

        // The test functions assign variables, so we should see Value events.
        assert!(
            value_count > 0,
            "trace should contain Value events from variable writes, got {value_count}"
        );

        // There should be Function definition events.
        assert!(
            function_count >= 1,
            "trace should contain at least 1 Function event, got {function_count}"
        );

        // ---- Step 6: Verify function names ----------------------------------
        let fn_names = extract_function_names(&events);
        let all_fn_names: Vec<&String> = fn_names.values().collect();

        // The toplevel entry should always be present.
        assert!(
            all_fn_names.iter().any(|n| n.contains("toplevel")),
            "trace should contain a <toplevel> function entry, got: {all_fn_names:?}"
        );

        // At least one test function name should appear (the trace file
        // corresponds to a specific test function).
        let known_test_fns = [
            "test_computation",
            "test_structs",
            "test_vectors",
            "test_loops",
            "test_nested_calls",
            "test_generics",
            "test_fibonacci",
            "test_references",
            "test_boolean_and_integers",
            "test_abort",
        ];
        let has_test_fn = all_fn_names
            .iter()
            .any(|n| known_test_fns.iter().any(|tf| n.contains(tf)));
        // The function name might be mangled or just the bare name.
        // If it is not a known test fn, it could be a helper called from a test.
        // At minimum, there should be at least one non-toplevel function.
        assert!(
            fn_names.len() >= 2 || has_test_fn,
            "trace should reference at least one test function or helper. \
             Functions found: {all_fn_names:?}"
        );

        // ---- Step 7: Verify metadata ----------------------------------------
        let metadata_content =
            std::fs::read_to_string(out_dir.join("trace_metadata.json"))
                .expect("failed to read trace_metadata.json");
        let metadata: serde_json::Value =
            serde_json::from_str(&metadata_content).expect("metadata should be valid JSON");

        assert!(
            metadata.get("program").is_some(),
            "metadata should have a 'program' field"
        );

        // ---- Step 8: Verify call/return balance -----------------------------
        // Every Call should eventually have a matching Return (except possibly
        // the toplevel). The number of Returns should be at most the number of Calls.
        assert!(
            return_count <= call_count,
            "return_count ({return_count}) should not exceed call_count ({call_count})"
        );
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
    assert!(
        !trace_files.is_empty(),
        "No trace files found"
    );

    for trace_file in &trace_files {
        let trace_data = std::fs::read(trace_file).expect("failed to read trace file");
        let trace_bytes = if trace_file
            .extension()
            .is_some_and(|ext| ext == "zst")
        {
            let mut decoder =
                zstd::Decoder::new(trace_data.as_slice()).expect("zstd decoder");
            let mut decompressed = Vec::new();
            std::io::Read::read_to_end(&mut decoder, &mut decompressed)
                .expect("decompress");
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

        // Basic sanity: there should be events.
        assert!(event_count > 0, "trace should contain at least one event");
        assert!(
            open_frame_count > 0,
            "trace should contain at least one OpenFrame"
        );
        assert!(
            close_frame_count > 0,
            "trace should contain at least one CloseFrame"
        );
        assert!(
            instruction_count > 0,
            "trace should contain at least one Instruction"
        );
        assert!(
            effect_count > 0,
            "trace should contain at least one Effect"
        );

        // OpenFrame and CloseFrame counts should match (each opened frame is closed).
        assert_eq!(
            open_frame_count, close_frame_count,
            "OpenFrame count ({open_frame_count}) should equal \
             CloseFrame count ({close_frame_count})"
        );
    }
}
