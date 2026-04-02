//! Tests for the replay pipeline and source lookup modules.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use codetracer_trace_writer::TraceEventsFileFormat;

use codetracer_move_recorder::replay::{self, ReplayConfig};
use codetracer_move_recorder::source_lookup::SourceLookup;

/// Minimal valid NDJSON trace data for testing.
fn minimal_trace_ndjson() -> &'static str {
    concat!(
        r#"{"version":3}"#, "\n",
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"transfer","module":{"address":"0x2","name":"coin"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":["u64"],"is_native":false},"gas_left":1000000}"#, "\n",
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":999990,"instruction":"LdU64(100)"}"#, "\n",
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"U64","value":100}}}}"#, "\n",
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":100}}}}"#, "\n",
        r#"{"type":"CloseFrame","frame_id":1,"return_":[{"type":"U64","value":100}],"gas_left":999900}"#, "\n",
    )
}

// ---- find_trace_file tests ----

#[test]
fn test_find_trace_file_zst() {
    let tmp = tempfile::TempDir::new().unwrap();
    let zst_path = tmp.path().join("trace.json.zst");
    fs::write(&zst_path, b"fake zst data").unwrap();

    let found = replay::find_trace_file(tmp.path()).unwrap();
    assert_eq!(found, zst_path);
}

#[test]
fn test_find_trace_file_json() {
    let tmp = tempfile::TempDir::new().unwrap();
    let json_path = tmp.path().join("trace.json");
    fs::write(&json_path, b"fake json data").unwrap();

    let found = replay::find_trace_file(tmp.path()).unwrap();
    assert_eq!(found, json_path);
}

#[test]
fn test_find_trace_file_missing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let result = replay::find_trace_file(tmp.path());
    assert!(result.is_err(), "should error when no trace file exists");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("No trace file"),
        "error should mention missing trace file, got: {err_msg}"
    );
}

// ---- source lookup tests ----

#[test]
fn test_source_lookup_finds_move_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sources_dir = tmp.path().join("sources");
    fs::create_dir_all(&sources_dir).unwrap();

    let move_file = sources_dir.join("coin.move");
    fs::write(&move_file, "module 0x2::coin {}").unwrap();

    let lookup = SourceLookup::new(vec![tmp.path().to_path_buf()]);
    let resolved = lookup.resolve("coin");
    assert!(resolved.is_some(), "should find coin.move");
    assert_eq!(resolved.unwrap(), move_file);
}

#[test]
fn test_source_lookup_nested_dirs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let nested = tmp.path().join("project").join("sources");
    fs::create_dir_all(&nested).unwrap();

    let move_file = nested.join("token.move");
    fs::write(&move_file, "module 0x1::token {}").unwrap();

    let lookup = SourceLookup::new(vec![tmp.path().to_path_buf()]);
    let resolved = lookup.resolve("token");
    assert!(resolved.is_some(), "should find token.move in nested sources/");
    assert_eq!(resolved.unwrap(), move_file);
}

#[test]
fn test_source_lookup_missing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let lookup = SourceLookup::new(vec![tmp.path().to_path_buf()]);
    let resolved = lookup.resolve("nonexistent_module");
    assert!(
        resolved.is_none(),
        "should return None for missing module (not an error)"
    );
}

// ---- ReplayConfig tests ----

#[test]
fn test_replay_config_defaults() {
    let config = ReplayConfig::new("ABC123".to_string());
    assert_eq!(config.rpc_url, "http://localhost:9000");
    assert_eq!(config.digest, "ABC123");
    assert!(config.source_dir.is_none());
    assert_eq!(config.out_dir, PathBuf::from("./ct-traces/"));
}

// ---- end-to-end test (bypassing sui CLI) ----

#[test]
fn test_replay_end_to_end_with_existing_trace() {
    let tmp = tempfile::TempDir::new().unwrap();

    // Create a compressed trace file (.json.zst).
    let trace_data = minimal_trace_ndjson();
    let zst_path = tmp.path().join("trace.json.zst");
    let mut encoder = zstd::Encoder::new(
        fs::File::create(&zst_path).unwrap(),
        3, // compression level
    )
    .unwrap();
    encoder.write_all(trace_data.as_bytes()).unwrap();
    encoder.finish().unwrap();

    // Create a source directory with a .move file.
    let source_dir = tmp.path().join("sources");
    fs::create_dir_all(&source_dir).unwrap();
    fs::write(source_dir.join("coin.move"), "module 0x2::coin {}").unwrap();

    // Output directory.
    let out_dir = tmp.path().join("ct-traces");

    // Run the pipeline (skipping sui CLI).
    replay::replay_from_existing_trace(
        &zst_path,
        &[tmp.path().to_path_buf()],
        &out_dir,
        TraceEventsFileFormat::Json,
    )
    .expect("replay_from_existing_trace should succeed");

    // Verify the 3-file output.
    assert!(
        out_dir.join("trace.json").exists(),
        "trace.json should be created"
    );
    assert!(
        out_dir.join("trace_metadata.json").exists(),
        "trace_metadata.json should be created"
    );
    assert!(
        out_dir.join("trace_paths.json").exists(),
        "trace_paths.json should be created"
    );

    // Verify metadata is valid JSON.
    let metadata_str =
        fs::read_to_string(out_dir.join("trace_metadata.json")).unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_str).expect("trace_metadata.json should be valid JSON");
    assert!(
        metadata.get("program").is_some(),
        "trace_metadata.json should have a 'program' field"
    );

    // Verify trace.json is non-empty.
    let trace_size = fs::metadata(out_dir.join("trace.json")).unwrap().len();
    assert!(trace_size > 0, "trace.json should be non-empty");
}
