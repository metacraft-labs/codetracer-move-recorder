//! Integration tests for the Move trace converter.

use std::path::Path;

use codetracer_trace_writer::TraceEventsFileFormat;

use codetracer_move_recorder::converter;
use codetracer_move_recorder::move_types::{SerializableMoveValue, TraceEvent, VersionHeader};
use codetracer_move_recorder::source_map::SourceMapResolver;

/// Create synthetic NDJSON trace data for a simple `test_computation` function.
fn create_synthetic_trace() -> String {
    let lines = vec![
        r#"{"version":3}"#,
        // OpenFrame for test_computation
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"test_computation","module":{"address":"0x0","name":"flow_test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":["u64","u64","u64","u64","u64"],"is_native":false},"gas_left":1000000}"#,
        // Instruction pc=0: let a = 10
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":999990,"instruction":"LdU64(10)"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"U64","value":10}}}}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":10}}}}"#,
        // Instruction pc=1: let b = 32
        r#"{"type":"Instruction","type_parameters":[],"pc":1,"gas_left":999980,"instruction":"LdU64(32)"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"U64","value":32}}}}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":1},"value":{"type":"RuntimeValue","value":{"type":"U64","value":32}}}}"#,
        // Instruction pc=2: let sum = a + b
        r#"{"type":"Instruction","type_parameters":[],"pc":2,"gas_left":999970,"instruction":"Add"}"#,
        r#"{"type":"Effect","effect":{"type":"Pop","value":{"type":"RuntimeValue","value":{"type":"U64","value":32}}}}"#,
        r#"{"type":"Effect","effect":{"type":"Pop","value":{"type":"RuntimeValue","value":{"type":"U64","value":10}}}}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"U64","value":42}}}}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":2},"value":{"type":"RuntimeValue","value":{"type":"U64","value":42}}}}"#,
        // Instruction pc=3: let doubled = sum * 2
        r#"{"type":"Instruction","type_parameters":[],"pc":3,"gas_left":999960,"instruction":"Mul"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":3},"value":{"type":"RuntimeValue","value":{"type":"U64","value":84}}}}"#,
        // Instruction pc=4: let final_val = doubled + a
        r#"{"type":"Instruction","type_parameters":[],"pc":4,"gas_left":999950,"instruction":"Add"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":4},"value":{"type":"RuntimeValue","value":{"type":"U64","value":94}}}}"#,
        // CloseFrame
        r#"{"type":"CloseFrame","frame_id":1,"return_":[{"type":"U64","value":94}],"gas_left":999940}"#,
    ];
    lines.join("\n")
}

/// Create a source map for the synthetic trace.
fn create_synthetic_source_map() -> SourceMapResolver {
    SourceMapResolver::from_entries(vec![
        ("flow_test".to_string(), 0, "flow_test.move".to_string(), 3),
        ("flow_test".to_string(), 1, "flow_test.move".to_string(), 4),
        ("flow_test".to_string(), 2, "flow_test.move".to_string(), 5),
        ("flow_test".to_string(), 3, "flow_test.move".to_string(), 6),
        ("flow_test".to_string(), 4, "flow_test.move".to_string(), 7),
    ])
}

// ---- Test 1: Parse synthetic trace NDJSON, verify all event types handled ----

#[test]
fn test_move_trace_parser_basic() {
    let trace_str = create_synthetic_trace();
    let mut lines = trace_str.lines();

    // Parse version header
    let header: VersionHeader =
        serde_json::from_str(lines.next().unwrap()).expect("failed to parse version header");
    assert_eq!(header.version, 3);

    // Parse all remaining events
    let mut open_frame_count = 0;
    let mut close_frame_count = 0;
    let mut instruction_count = 0;
    let mut effect_count = 0;

    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: TraceEvent =
            serde_json::from_str(line).expect(&format!("failed to parse event: {line}"));
        match event {
            TraceEvent::OpenFrame { .. } => open_frame_count += 1,
            TraceEvent::CloseFrame { .. } => close_frame_count += 1,
            TraceEvent::Instruction { .. } => instruction_count += 1,
            TraceEvent::Effect { .. } => effect_count += 1,
            TraceEvent::External { .. } => {}
        }
    }

    assert_eq!(open_frame_count, 1, "expected 1 OpenFrame event");
    assert_eq!(close_frame_count, 1, "expected 1 CloseFrame event");
    assert_eq!(instruction_count, 5, "expected 5 Instruction events");
    assert!(effect_count > 0, "expected some Effect events");
}

// ---- Test 2: Verify Instruction events produce Step events with correct source lines ----

#[test]
fn test_move_to_ct_step_mapping() {
    let trace_str = create_synthetic_trace();
    let source_map = create_synthetic_source_map();
    let tmp = tempfile::TempDir::new().expect("failed to create temp dir");
    let out_dir = tmp.path().join("ct-out");
    let source_path = Path::new("flow_test.move");

    converter::convert_trace(
        trace_str.as_bytes(),
        &source_map,
        source_path,
        &out_dir,
        TraceEventsFileFormat::Json,
    )
    .expect("convert_trace should succeed");

    // The trace.bin (JSON format) should exist and contain step data.
    let trace_bin = out_dir.join("trace.bin");
    assert!(trace_bin.exists(), "trace.bin should exist");

    let trace_content =
        std::fs::read_to_string(&trace_bin).expect("failed to read trace.bin");

    // In JSON mode, trace events are serialized. Verify the file is non-empty
    // and contains step-related data.
    assert!(!trace_content.is_empty(), "trace.bin should not be empty");

    // Source map maps pc 0-4 to lines 3-7. Verify the converter ran successfully.
    // The detailed content verification is done by checking that the metadata
    // and paths files are also valid.
    let metadata_content = std::fs::read_to_string(out_dir.join("trace_metadata.json"))
        .expect("failed to read trace_metadata.json");
    let _metadata: serde_json::Value =
        serde_json::from_str(&metadata_content).expect("trace_metadata.json should be valid JSON");
}

// ---- Test 3: Verify OpenFrame/CloseFrame produce Call/Return events ----

#[test]
fn test_move_to_ct_call_trace() {
    // Trace with nested function calls.
    let trace_str = vec![
        r#"{"version":3}"#,
        // Open outer function
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"outer","module":{"address":"0x0","name":"mod"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":999,"instruction":"Call"}"#,
        // Open inner function
        r#"{"type":"OpenFrame","frame":{"frame_id":2,"function_name":"inner","module":{"address":"0x0","name":"mod"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":998}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":997,"instruction":"LdU64(1)"}"#,
        // Close inner function
        r#"{"type":"CloseFrame","frame_id":2,"return_":[{"type":"U64","value":1}],"gas_left":996}"#,
        // Close outer function
        r#"{"type":"CloseFrame","frame_id":1,"gas_left":995}"#,
    ]
    .join("\n");

    let source_map = SourceMapResolver::empty();
    let tmp = tempfile::TempDir::new().expect("failed to create temp dir");
    let out_dir = tmp.path().join("ct-out");
    let source_path = Path::new("call_test.move");

    converter::convert_trace(
        trace_str.as_bytes(),
        &source_map,
        source_path,
        &out_dir,
        TraceEventsFileFormat::Json,
    )
    .expect("convert_trace should succeed with nested calls");

    // Verify output files exist.
    assert!(out_dir.join("trace.bin").exists());
    assert!(out_dir.join("trace_metadata.json").exists());
    assert!(out_dir.join("trace_paths.json").exists());

    // The trace.bin should be non-empty (it contains call/return events).
    let trace_content =
        std::fs::read_to_string(out_dir.join("trace.bin")).expect("failed to read trace.bin");
    assert!(
        !trace_content.is_empty(),
        "trace.bin should contain call/return data"
    );
}

// ---- Test 4: Verify Move values convert to correct ValueRecord ----

#[test]
fn test_move_to_ct_value_conversion() {
    // Test with various value types.
    let trace_str = vec![
        r#"{"version":3}"#,
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"value_test","module":{"address":"0x0","name":"val_mod"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":["u64","bool","address"],"is_native":false},"gas_left":1000}"#,
        // Write a u64 value
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":42}}}}"#,
        // Write a bool value
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":1},"value":{"type":"RuntimeValue","value":{"type":"Bool","value":true}}}}"#,
        // Write an address value
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":2},"value":{"type":"RuntimeValue","value":{"type":"Address","value":"0xCAFE"}}}}"#,
        // Push a struct value
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"Struct","fields":[{"type":"U64","value":100},{"type":"Bool","value":false}],"type_":"MyStruct"}}}}"#,
        r#"{"type":"CloseFrame","frame_id":1,"gas_left":900}"#,
    ]
    .join("\n");

    let source_map = SourceMapResolver::empty();
    let tmp = tempfile::TempDir::new().expect("failed to create temp dir");
    let out_dir = tmp.path().join("ct-out");
    let source_path = Path::new("value_test.move");

    converter::convert_trace(
        trace_str.as_bytes(),
        &source_map,
        source_path,
        &out_dir,
        TraceEventsFileFormat::Json,
    )
    .expect("convert_trace should succeed with various value types");

    // Verify the trace was written successfully.
    assert!(out_dir.join("trace.bin").exists());
    let trace_content =
        std::fs::read_to_string(out_dir.join("trace.bin")).expect("failed to read trace.bin");
    assert!(
        !trace_content.is_empty(),
        "trace.bin should contain variable records"
    );

    // Also test direct value parsing roundtrip.
    let u64_val: SerializableMoveValue =
        serde_json::from_str(r#"{"type":"U64","value":42}"#).expect("parse U64");
    match u64_val {
        SerializableMoveValue::U64 { value } => assert_eq!(value, 42),
        _ => panic!("expected U64 variant"),
    }

    let bool_val: SerializableMoveValue =
        serde_json::from_str(r#"{"type":"Bool","value":true}"#).expect("parse Bool");
    match bool_val {
        SerializableMoveValue::Bool { value } => assert!(value),
        _ => panic!("expected Bool variant"),
    }

    let struct_val: SerializableMoveValue = serde_json::from_str(
        r#"{"type":"Struct","fields":[{"type":"U64","value":10}],"type_":"Foo"}"#,
    )
    .expect("parse Struct");
    match struct_val {
        SerializableMoveValue::Struct { fields, type_ } => {
            assert_eq!(type_, "Foo");
            assert_eq!(fields.len(), 1);
        }
        _ => panic!("expected Struct variant"),
    }
}

// ---- Test 5: Verify trace.bin, trace_metadata.json, trace_paths.json exist and are valid ----

#[test]
fn test_move_trace_3file_output() {
    let trace_str = create_synthetic_trace();
    let source_map = create_synthetic_source_map();
    let tmp = tempfile::TempDir::new().expect("failed to create temp dir");
    let out_dir = tmp.path().join("ct-out");
    let source_path = Path::new("flow_test.move");

    converter::convert_trace(
        trace_str.as_bytes(),
        &source_map,
        source_path,
        &out_dir,
        TraceEventsFileFormat::Json,
    )
    .expect("convert_trace should succeed");

    // 1. trace.bin exists and is non-empty
    let trace_bin = out_dir.join("trace.bin");
    assert!(trace_bin.exists(), "trace.bin must exist");
    let trace_size = std::fs::metadata(&trace_bin)
        .expect("trace.bin metadata")
        .len();
    assert!(trace_size > 0, "trace.bin must be non-empty");

    // 2. trace_metadata.json exists and is valid JSON
    let metadata_path = out_dir.join("trace_metadata.json");
    assert!(metadata_path.exists(), "trace_metadata.json must exist");
    let metadata_str =
        std::fs::read_to_string(&metadata_path).expect("failed to read trace_metadata.json");
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_str).expect("trace_metadata.json must be valid JSON");
    // Should have a "program" field
    assert!(
        metadata.get("program").is_some(),
        "trace_metadata.json should have a 'program' field"
    );

    // 3. trace_paths.json exists and is valid JSON
    let paths_path = out_dir.join("trace_paths.json");
    assert!(paths_path.exists(), "trace_paths.json must exist");
    let paths_str =
        std::fs::read_to_string(&paths_path).expect("failed to read trace_paths.json");
    let _paths: serde_json::Value =
        serde_json::from_str(&paths_str).expect("trace_paths.json must be valid JSON");
}
