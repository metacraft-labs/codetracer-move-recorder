//! Integration tests for the Move trace converter.

use std::path::Path;

use codetracer_trace_types::TraceLowLevelEvent;
use codetracer_trace_writer_nim::non_streaming_trace_writer::NonStreamingTraceWriter;
use codetracer_trace_writer_nim::TraceEventsFileFormat;
use std::collections::HashMap;

use codetracer_move_recorder::converter;
use codetracer_move_recorder::move_types::{SerializableMoveValue, TraceEvent, VersionHeader};
use codetracer_move_recorder::source_map::SourceMapResolver;

/// Create synthetic NDJSON trace data for a simple `test_computation` function.
fn create_synthetic_trace() -> String {
    let lines = vec![
        r#"{"version":3}"#,
        // OpenFrame for test_computation
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"test_computation","module":{"address":"0x0","name":"flow_test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"u64"},{"type_":"u64"},{"type_":"u64"},{"type_":"u64"},{"type_":"u64"}],"is_native":false},"gas_left":1000000}}"#,
        // Instruction pc=0: let a = 10
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":999990,"instruction":"LdU64(10)"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"U64","value":10}}}}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":10}}}}}}"#,
        // Instruction pc=1: let b = 32
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":999980,"instruction":"LdU64(32)"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"U64","value":32}}}}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,1]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":32}}}}}}"#,
        // Instruction pc=2: let sum = a + b
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":999970,"instruction":"Add"}}"#,
        r#"{"Effect":{"Pop":{"RuntimeValue":{"value":{"type":"U64","value":32}}}}}"#,
        r#"{"Effect":{"Pop":{"RuntimeValue":{"value":{"type":"U64","value":10}}}}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"U64","value":42}}}}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,2]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":42}}}}}}"#,
        // Instruction pc=3: let doubled = sum * 2
        r#"{"Instruction":{"type_parameters":[],"pc":3,"gas_left":999960,"instruction":"Mul"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,3]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":84}}}}}}"#,
        // Instruction pc=4: let final_val = doubled + a
        r#"{"Instruction":{"type_parameters":[],"pc":4,"gas_left":999950,"instruction":"Add"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,4]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":94}}}}}}"#,
        // CloseFrame
        r#"{"CloseFrame":{"frame_id":1,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":94}}}],"gas_left":999940}}"#,
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

/// Helper: run convert_trace_into_writer with NonStreamingTraceWriter.
fn run_converter_events(
    ndjson: &str,
    source_map: &SourceMapResolver,
    source_name: &str,
) -> Vec<TraceLowLevelEvent> {
    let source_path = Path::new(source_name);
    let mut writer = NonStreamingTraceWriter::new(source_name, &[]);

    converter::convert_trace_into_writer(
        ndjson.as_bytes(),
        source_map,
        source_path,
        &mut writer,
    )
    .expect("convert_trace_into_writer should succeed");

    writer.events
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
            TraceEvent::Effect(..) => effect_count += 1,
            TraceEvent::External(..) => {}
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

    let events = run_converter_events(&trace_str, &source_map, "flow_test.move");

    let step_lines: Vec<i64> = events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Step(step) => Some(step.line.0),
            _ => None,
        })
        .collect();

    // The steps come from instruction events mapped via the source map to lines 3-7.
    let instruction_step_lines: Vec<i64> = step_lines
        .iter()
        .copied()
        .filter(|&line| (3..=7).contains(&line))
        .collect();

    // We expect 5 distinct lines (3, 4, 5, 6, 7) since each pc maps to a different line.
    let mut unique_lines = instruction_step_lines.clone();
    unique_lines.sort();
    unique_lines.dedup();
    assert_eq!(
        unique_lines,
        vec![3, 4, 5, 6, 7],
        "should have steps for lines 3 through 7"
    );

    // Verify the instruction-derived steps appear in order.
    assert_eq!(
        instruction_step_lines,
        vec![3, 4, 5, 6, 7],
        "instruction steps should appear in sequential order"
    );

    // Verify the trace also contains variable assignments from Write effects.
    let value_count = events
        .iter()
        .filter(|e| matches!(e, TraceLowLevelEvent::Value(_)))
        .count();
    assert!(
        value_count > 0,
        "trace should contain Value events from Write effects"
    );
}

// ---- Test 3: Verify OpenFrame/CloseFrame produce Call/Return events ----

#[test]
fn test_move_to_ct_call_trace() {
    let trace_str = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"outer","module":{"address":"0x0","name":"mod"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":999,"instruction":"Call"}}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":2,"function_name":"inner","module":{"address":"0x0","name":"mod"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":998}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":997,"instruction":"LdU64(1)"}}"#,
        r#"{"CloseFrame":{"frame_id":2,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":1}}}],"gas_left":996}}"#,
        r#"{"CloseFrame":{"frame_id":1,"gas_left":995}}"#,
    ]
    .join("\n");

    let events = run_converter_events(&trace_str, &SourceMapResolver::empty(), "call_test.move");

    let call_count = events
        .iter()
        .filter(|e| matches!(e, TraceLowLevelEvent::Call(_)))
        .count();
    let return_count = events
        .iter()
        .filter(|e| matches!(e, TraceLowLevelEvent::Return(_)))
        .count();

    // 2 OpenFrame + 1 toplevel Call from start() = 3 total.
    assert_eq!(call_count, 3, "expected 3 Call events (toplevel + outer + inner)");
    assert_eq!(return_count, 3, "expected 3 Return events (outer + inner + toplevel)");

    // Build a function name lookup from Function events.
    let mut function_names: HashMap<usize, String> = HashMap::new();
    let mut next_fn_id = 0usize;
    for event in &events {
        if let TraceLowLevelEvent::Function(func) = event {
            function_names.insert(next_fn_id, func.name.clone());
            next_fn_id += 1;
        }
    }

    // Extract function names referenced by Call events in order.
    let call_fn_names: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Call(call) => {
                function_names.get(&call.function_id.0).cloned()
            }
            _ => None,
        })
        .collect();

    assert_eq!(call_fn_names.len(), 3);
    assert_eq!(call_fn_names[0], "<toplevel>", "first call should be toplevel");
    assert_eq!(call_fn_names[1], "outer", "second call should be 'outer'");
    assert_eq!(call_fn_names[2], "inner", "third call should be 'inner'");

    // Verify Return events have correct return values.
    let return_values: Vec<&codetracer_trace_types::ValueRecord> = events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Return(ret) => Some(&ret.return_value),
            _ => None,
        })
        .collect();

    assert_eq!(return_values.len(), 3);
    match &return_values[0] {
        codetracer_trace_types::ValueRecord::Int { i, .. } => {
            assert_eq!(*i, 1, "inner function should return 1");
        }
        _ => panic!("expected Int return value from inner function, got {:?}", return_values[0]),
    }

    assert!(
        matches!(return_values[1], codetracer_trace_types::ValueRecord::None { .. }),
        "outer function with no return_ should produce None value, got {:?}",
        return_values[1]
    );
}

// ---- Test 4: Verify Move values convert to correct ValueRecord ----

#[test]
fn test_move_to_ct_value_conversion() {
    let trace_str = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"value_test","module":{"address":"0x0","name":"val_mod"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"u64"},{"type_":"bool"},{"type_":"address"}],"is_native":false},"gas_left":1000}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":42}}}}}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,1]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Bool","value":true}}}}}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,2]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Address","value":"0xCAFE"}}}}}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"MyStruct"},"fields":[["field_0",{"type":"U64","value":100}],["field_1",{"type":"Bool","value":false}]]}}}}}}"#,
        r#"{"CloseFrame":{"frame_id":1,"gas_left":900}}"#,
    ]
    .join("\n");

    let events = run_converter_events(&trace_str, &SourceMapResolver::empty(), "value_test.move");
    assert!(!events.is_empty());

    // Verify Value events were generated for Write effects.
    let value_count = events
        .iter()
        .filter(|e| matches!(e, TraceLowLevelEvent::Value(_)))
        .count();
    assert!(
        value_count >= 3,
        "expected at least 3 Value events from Write effects, got {value_count}"
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
        r#"{"type":"Struct","value":{"type_":{"name":"Foo"},"fields":[["field_0",{"type":"U64","value":10}]]}}"#,
    )
    .expect("parse Struct");
    match struct_val {
        SerializableMoveValue::Struct { value: content } => {
            assert_eq!(content.type_.get("name").and_then(|v| v.as_str()), Some("Foo"));
            assert_eq!(content.fields.len(), 1);
        }
        _ => panic!("expected Struct variant"),
    }
}

// ---- Test 5: Verify .ct output file is produced ----

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
        TraceEventsFileFormat::Binary,
    )
    .expect("convert_trace should succeed");

    // Verify .ct output with CTFS magic bytes.
    let ct_files: Vec<_> = std::fs::read_dir(&out_dir)
        .expect("read output dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |ext| ext == "ct"))
        .collect();
    assert!(!ct_files.is_empty(), "expected at least one .ct file in output dir");
    let content = std::fs::read(&ct_files[0]).expect("read .ct file");
    assert!(content.len() >= 5, ".ct file too small");
    assert_eq!(&content[..5], &[0xC0, 0xDE, 0x72, 0xAC, 0xE2], "CTFS magic bytes mismatch");
}
