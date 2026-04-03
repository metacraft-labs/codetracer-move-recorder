//! Comprehensive integration tests for the Move trace converter.
//!
//! Exercises the converter against synthetic NDJSON traces representing
//! realistic Move execution scenarios covering all language constructs,
//! call patterns, variable tracking, control flow, and real-world scenarios.

use std::path::Path;

use codetracer_trace_types::TraceLowLevelEvent;
use codetracer_trace_writer::TraceEventsFileFormat;

use codetracer_move_recorder::converter;
use codetracer_move_recorder::move_types::{SerializableMoveValue, TraceEvent, TraceValue};
use codetracer_move_recorder::source_map::SourceMapResolver;

// ============================================================================
// Helpers
// ============================================================================

/// Run convert_trace on the given NDJSON string with the given source map,
/// returning the parsed trace.json content, metadata, and paths as JSON values.
fn run_converter(
    ndjson: &str,
    source_map: &SourceMapResolver,
    source_name: &str,
) -> (String, serde_json::Value, serde_json::Value) {
    let tmp = tempfile::TempDir::new().expect("failed to create temp dir");
    let out_dir = tmp.path().join("ct-out");
    let source_path = Path::new(source_name);

    converter::convert_trace(
        ndjson.as_bytes(),
        source_map,
        source_path,
        &out_dir,
        TraceEventsFileFormat::Json,
    )
    .expect("convert_trace should succeed");

    let trace_content =
        std::fs::read_to_string(out_dir.join("trace.json")).expect("read trace.json");
    let metadata_str =
        std::fs::read_to_string(out_dir.join("trace_metadata.json")).expect("read metadata");
    let paths_str =
        std::fs::read_to_string(out_dir.join("trace_paths.json")).expect("read paths");

    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_str).expect("metadata is valid JSON");
    let paths: serde_json::Value =
        serde_json::from_str(&paths_str).expect("paths is valid JSON");

    (trace_content, metadata, paths)
}

/// Run convert_trace and verify it succeeds, returning trace.json content string.
fn run_converter_simple(ndjson: &str) -> String {
    let (trace, _, _) = run_converter(ndjson, &SourceMapResolver::empty(), "test.move");
    trace
}

/// Parse trace.json JSON content into a Vec of TraceLowLevelEvent.
fn parse_trace_events(trace_content: &str) -> Vec<TraceLowLevelEvent> {
    serde_json::from_str(trace_content).expect("trace.json should be valid JSON array of events")
}

/// Count Call and Return events in parsed trace events.
fn count_call_return(events: &[TraceLowLevelEvent]) -> (usize, usize) {
    let calls = events
        .iter()
        .filter(|e| matches!(e, TraceLowLevelEvent::Call(_)))
        .count();
    let returns = events
        .iter()
        .filter(|e| matches!(e, TraceLowLevelEvent::Return(_)))
        .count();
    (calls, returns)
}

/// Extract step line numbers from parsed trace events.
fn extract_step_lines(events: &[TraceLowLevelEvent]) -> Vec<i64> {
    events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Step(step) => Some(step.line.0),
            _ => None,
        })
        .collect()
}

/// Parse NDJSON events (skip version header) and count event types.
fn count_events(ndjson: &str) -> (usize, usize, usize, usize) {
    let mut lines = ndjson.lines();
    lines.next(); // skip version header
    let mut open = 0;
    let mut close = 0;
    let mut instr = 0;
    let mut effect = 0;
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: TraceEvent = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("failed to parse: {e}\nline: {line}"));
        match event {
            TraceEvent::OpenFrame { .. } => open += 1,
            TraceEvent::CloseFrame { .. } => close += 1,
            TraceEvent::Instruction { .. } => instr += 1,
            TraceEvent::Effect(..) => effect += 1,
            TraceEvent::External(..) => {}
        }
    }
    (open, close, instr, effect)
}

// ============================================================================
// 1. Rich type coverage in trace values
// ============================================================================

#[test]
fn test_value_u8() {
    let v: SerializableMoveValue =
        serde_json::from_str(r#"{"type":"U8","value":255}"#).unwrap();
    match v {
        SerializableMoveValue::U8 { value } => assert_eq!(value, 255),
        _ => panic!("expected U8"),
    }
}

#[test]
fn test_value_u16() {
    let v: SerializableMoveValue =
        serde_json::from_str(r#"{"type":"U16","value":65535}"#).unwrap();
    match v {
        SerializableMoveValue::U16 { value } => assert_eq!(value, 65535),
        _ => panic!("expected U16"),
    }
}

#[test]
fn test_value_u32() {
    let v: SerializableMoveValue =
        serde_json::from_str(r#"{"type":"U32","value":4294967295}"#).unwrap();
    match v {
        SerializableMoveValue::U32 { value } => assert_eq!(value, 4294967295),
        _ => panic!("expected U32"),
    }
}

#[test]
fn test_value_u64() {
    let v: SerializableMoveValue =
        serde_json::from_str(r#"{"type":"U64","value":18446744073709551615}"#).unwrap();
    match v {
        SerializableMoveValue::U64 { value } => assert_eq!(value, u64::MAX),
        _ => panic!("expected U64"),
    }
}

#[test]
fn test_value_u128() {
    // U128 deserialization now works via a custom deserializer that handles
    // the serde_json limitation with u128 in internally tagged enums.
    let v: SerializableMoveValue =
        serde_json::from_str(r#"{"type":"U128","value":42}"#)
            .expect("U128 deserialization should succeed");
    match v {
        SerializableMoveValue::U128 { value } => assert_eq!(value, 42),
        _ => panic!("expected U128 variant"),
    }

    // Test with a large value that exceeds u64 range.
    let v_large: SerializableMoveValue =
        serde_json::from_str(r#"{"type":"U128","value":340282366920938463463374607431768211455}"#)
            .expect("U128 max value deserialization should succeed");
    match v_large {
        SerializableMoveValue::U128 { value } => assert_eq!(value, u128::MAX),
        _ => panic!("expected U128 variant"),
    }

    // Test with string-encoded u128 (some Move VMs may encode this way).
    let v_str: SerializableMoveValue =
        serde_json::from_str(r#"{"type":"U128","value":"12345678901234567890"}"#)
            .expect("U128 from string should succeed");
    match v_str {
        SerializableMoveValue::U128 { value } => assert_eq!(value, 12345678901234567890u128),
        _ => panic!("expected U128 variant"),
    }
}

#[test]
fn test_value_u256() {
    let v: SerializableMoveValue = serde_json::from_str(
        r#"{"type":"U256","value":"115792089237316195423570985008687907853269984665640564039457584007913129639935"}"#,
    )
    .unwrap();
    match v {
        SerializableMoveValue::U256 { value } => {
            assert!(value.starts_with("1157920892373"));
        }
        _ => panic!("expected U256"),
    }
}

#[test]
fn test_value_bool_true_false() {
    let t: SerializableMoveValue =
        serde_json::from_str(r#"{"type":"Bool","value":true}"#).unwrap();
    let f: SerializableMoveValue =
        serde_json::from_str(r#"{"type":"Bool","value":false}"#).unwrap();
    match t {
        SerializableMoveValue::Bool { value } => assert!(value),
        _ => panic!("expected Bool true"),
    }
    match f {
        SerializableMoveValue::Bool { value } => assert!(!value),
        _ => panic!("expected Bool false"),
    }
}

#[test]
fn test_value_address() {
    let v: SerializableMoveValue = serde_json::from_str(
        r#"{"type":"Address","value":"0x0000000000000000000000000000000000000000000000000000000000000002"}"#,
    )
    .unwrap();
    match v {
        SerializableMoveValue::Address { value } => {
            assert!(value.starts_with("0x"));
            assert!(value.ends_with("2"));
        }
        _ => panic!("expected Address"),
    }
}

#[test]
fn test_value_struct_with_named_fields() {
    let v: SerializableMoveValue = serde_json::from_str(
        r#"{"type":"Struct","value":{"type_":{"name":"0x2::coin::Coin"},"fields":[["field_0",{"type":"U64","value":100}],["field_1",{"type":"Bool","value":true}],["field_2",{"type":"Address","value":"0xCAFE"}]]}}"#,
    )
    .unwrap();
    match v {
        SerializableMoveValue::Struct { value: content } => {
            assert_eq!(content.type_.get("name").and_then(|v| v.as_str()), Some("0x2::coin::Coin"));
            assert_eq!(content.fields.len(), 3);
            match &content.fields[0].1 {
                SerializableMoveValue::U64 { value } => assert_eq!(*value, 100),
                _ => panic!("expected U64 in field 0"),
            }
            match &content.fields[1].1 {
                SerializableMoveValue::Bool { value } => assert!(*value),
                _ => panic!("expected Bool in field 1"),
            }
            match &content.fields[2].1 {
                SerializableMoveValue::Address { value } => assert_eq!(value, "0xCAFE"),
                _ => panic!("expected Address in field 2"),
            }
        }
        _ => panic!("expected Struct"),
    }
}

#[test]
fn test_value_vector() {
    let v: SerializableMoveValue = serde_json::from_str(
        r#"{"type":"Vector","elements":[{"type":"U64","value":1},{"type":"U64","value":2},{"type":"U64","value":3}]}"#,
    )
    .unwrap();
    match v {
        SerializableMoveValue::Vector { elements } => {
            assert_eq!(elements.len(), 3);
            for (i, elem) in elements.iter().enumerate() {
                match elem {
                    SerializableMoveValue::U64 { value } => {
                        assert_eq!(*value, (i + 1) as u64)
                    }
                    _ => panic!("expected U64 in vector element {i}"),
                }
            }
        }
        _ => panic!("expected Vector"),
    }
}

#[test]
fn test_value_nested_vector_of_structs() {
    let v: SerializableMoveValue = serde_json::from_str(
        r#"{"type":"Vector","elements":[{"type":"Struct","value":{"type_":{"name":"Item"},"fields":[["field_0",{"type":"U64","value":10}]]}},{"type":"Struct","value":{"type_":{"name":"Item"},"fields":[["field_0",{"type":"U64","value":20}]]}}]}"#,
    )
    .unwrap();
    match v {
        SerializableMoveValue::Vector { elements } => {
            assert_eq!(elements.len(), 2);
            for elem in &elements {
                match elem {
                    SerializableMoveValue::Struct { value: content } => {
                        assert_eq!(content.type_.get("name").and_then(|v| v.as_str()), Some("Item"));
                    }
                    _ => panic!("expected Struct inside Vector"),
                }
            }
        }
        _ => panic!("expected Vector"),
    }
}

#[test]
fn test_value_variant() {
    let v: SerializableMoveValue = serde_json::from_str(
        r#"{"type":"Variant","tag":1,"fields":[{"type":"U64","value":42}],"type_":"0x1::option::Option"}"#,
    )
    .unwrap();
    match v {
        SerializableMoveValue::Variant { tag, fields, type_ } => {
            assert_eq!(tag, 1);
            assert_eq!(type_, "0x1::option::Option");
            assert_eq!(fields.len(), 1);
        }
        _ => panic!("expected Variant"),
    }
}

#[test]
fn test_value_variant_no_fields() {
    let v: SerializableMoveValue = serde_json::from_str(
        r#"{"type":"Variant","tag":0,"fields":[],"type_":"0x1::option::Option"}"#,
    )
    .unwrap();
    match v {
        SerializableMoveValue::Variant { tag, fields, .. } => {
            assert_eq!(tag, 0);
            assert!(fields.is_empty());
        }
        _ => panic!("expected Variant"),
    }
}

// Test all value types through the full converter pipeline
#[test]
fn test_all_value_types_through_converter() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"all_types","module":{"address":"0x0","name":"types_test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"u8"},{"type_":"u16"},{"type_":"u32"},{"type_":"u64"},{"type_":"u128"},{"type_":"u256"},{"type_":"bool"},{"type_":"address"}],"is_native":false},"gas_left":1000000}}"#,
        // U8
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U8","value":42}}}}}}"#,
        // U16
        r#"{"Effect":{"Write":{"location":{"Local":[1,1]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U16","value":1000}}}}}}"#,
        // U32
        r#"{"Effect":{"Write":{"location":{"Local":[1,2]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U32","value":100000}}}}}}"#,
        // U64
        r#"{"Effect":{"Write":{"location":{"Local":[1,3]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":9999999}}}}}}"#,
        // Note: U128 skipped here because serde_json does not support u128 deserialization.
        // U256
        r#"{"Effect":{"Write":{"location":{"Local":[1,5]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U256","value":"115792089237316195423570985008687907853269984665640564039457584007913129639935"}}}}}}"#,
        // Bool
        r#"{"Effect":{"Write":{"location":{"Local":[1,6]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Bool","value":true}}}}}}"#,
        // Address
        r#"{"Effect":{"Write":{"location":{"Local":[1,7]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Address","value":"0xDEADBEEF"}}}}}}"#,
        // Struct via Push
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"MyStruct"},"fields":[["field_0",{"type":"U64","value":100}],["field_1",{"type":"Bool","value":false}]]}}}}}}"#,
        // Vector via Push
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"Vector","elements":[{"type":"U8","value":1},{"type":"U8","value":2}]}}}}}"#,
        // Variant via Push
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"Variant","tag":1,"fields":[{"type":"U64","value":99}],"type_":"Option"}}}}}"#,
        r#"{"CloseFrame":{"frame_id":1,"gas_left":999000}}"#,
    ]
    .join("\n");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty(), "trace.json should not be empty");

    // Parse and verify Value events were generated for all the write effects.
    // We have 7 Write effects (u8, u16, u32, u64, u256, bool, address).
    let events = parse_trace_events(&result);
    let value_count = events
        .iter()
        .filter(|e| matches!(e, TraceLowLevelEvent::Value(_)))
        .count();
    assert!(
        value_count >= 7,
        "expected at least 7 Value events from Write effects for all value types, got {value_count}"
    );
}

// ============================================================================
// 2. Call trace patterns
// ============================================================================

#[test]
fn test_simple_function_call() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"simple_fn","module":{"address":"0x1","name":"my_module"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"u64"}],"is_native":false},"gas_left":1000}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":999,"instruction":"LdU64(5)"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"U64","value":5}}}}}"#,
        r#"{"CloseFrame":{"frame_id":1,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":5}}}],"gas_left":998}}"#,
    ]
    .join("\n");

    let (open, close, instr, effect) = count_events(&trace);
    assert_eq!(open, 1);
    assert_eq!(close, 1);
    assert_eq!(instr, 1);
    assert_eq!(effect, 1);

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());

    // Parse and verify basic event structure for a simple single-function call.
    let events = parse_trace_events(&result);
    let (calls, returns) = count_call_return(&events);
    // Toplevel call + simple_fn = 2 Calls, simple_fn close + toplevel close = 2 Returns
    assert_eq!(calls, 2, "expected 2 Call events (toplevel + simple_fn), got {calls}");
    assert_eq!(returns, 2, "expected 2 Return events (simple_fn + toplevel), got {returns}");
}

#[test]
fn test_nested_calls_a_calls_b_calls_c() {
    let trace = vec![
        r#"{"version":3}"#,
        // A opens
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"func_a","module":{"address":"0x1","name":"mod_a"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":10000}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":9999,"instruction":"Call"}}"#,
        // B opens
        r#"{"OpenFrame":{"frame":{"frame_id":2,"function_name":"func_b","module":{"address":"0x1","name":"mod_b"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":9998}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":9997,"instruction":"Call"}}"#,
        // C opens
        r#"{"OpenFrame":{"frame":{"frame_id":3,"function_name":"func_c","module":{"address":"0x1","name":"mod_c"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":9996}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":9995,"instruction":"LdU64(99)"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"U64","value":99}}}}}"#,
        // C closes
        r#"{"CloseFrame":{"frame_id":3,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":99}}}],"gas_left":9994}}"#,
        // B closes
        r#"{"CloseFrame":{"frame_id":2,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":99}}}],"gas_left":9993}}"#,
        // A closes
        r#"{"CloseFrame":{"frame_id":1,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":99}}}],"gas_left":9992}}"#,
    ]
    .join("\n");

    let (open, close, _, _) = count_events(&trace);
    assert_eq!(open, 3, "3 nested OpenFrame events");
    assert_eq!(close, 3, "3 nested CloseFrame events");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());

    // Parse trace.json and verify Call/Return balance for 3-level nesting.
    // Expect 4 Call events: 1 toplevel + func_a + func_b + func_c
    // Expect 3 Return events: func_c + func_b + func_a
    let events = parse_trace_events(&result);
    let (calls, returns) = count_call_return(&events);
    assert_eq!(calls, 4, "expected 4 Call events (toplevel + A + B + C)");
    assert_eq!(returns, 4, "expected 4 Return events (C + B + A + toplevel)");
}

#[test]
fn test_generic_function_instantiation() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"transfer","module":{"address":"0x2","name":"transfer"},"type_instantiation":["0x2::coin::Coin<0x2::sui::SUI>"],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":5000}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":4999,"instruction":"MoveLoc(0)"}}"#,
        r#"{"CloseFrame":{"frame_id":1,"gas_left":4998}}"#,
    ]
    .join("\n");

    // Verify parsing of type_instantiation
    let mut lines = trace.lines();
    lines.next(); // skip version
    let event: TraceEvent = serde_json::from_str(lines.next().unwrap()).unwrap();
    match event {
        TraceEvent::OpenFrame { frame, .. } => {
            assert_eq!(frame.type_instantiation.len(), 1);
            assert!(frame.type_instantiation[0].as_str().unwrap_or("").contains("Coin"));
        }
        _ => panic!("expected OpenFrame"),
    }

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());

    // Parse and verify the converter produced Call/Return events for the generic function.
    let events = parse_trace_events(&result);
    let (calls, returns) = count_call_return(&events);
    assert!(calls >= 2, "expected at least 2 Call events (toplevel + transfer), got {calls}");
    assert!(returns >= 1, "expected at least 1 Return event, got {returns}");
}

#[test]
fn test_entry_function_with_parameters() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"entry_transfer","module":{"address":"0x2","name":"pay"},"type_instantiation":[],"parameters":[{"RuntimeValue":{"value":{"type":"Address","value":"0xABCD"}}},{"RuntimeValue":{"value":{"type":"U64","value":1000}}}],"return_types":[],"locals_types":[{"type_":"address"},{"type_":"u64"}],"is_native":false},"gas_left":10000}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":9999,"instruction":"CopyLoc(0)"}}"#,
        r#"{"Effect":{"Read":{"location":{"Local":[1,0]},"root_value_read":{"RuntimeValue":{"value":{"type":"Address","value":"0xABCD"}}},"moved":false}}}"#,
        r#"{"CloseFrame":{"frame_id":1,"gas_left":9990}}"#,
    ]
    .join("\n");

    // Verify parameter parsing
    let mut lines = trace.lines();
    lines.next();
    let event: TraceEvent = serde_json::from_str(lines.next().unwrap()).unwrap();
    match event {
        TraceEvent::OpenFrame { frame, .. } => {
            assert_eq!(frame.parameters.len(), 2);
            match frame.parameters[0].inner_value() {
                SerializableMoveValue::Address { value } => assert_eq!(value, "0xABCD"),
                _ => panic!("expected Address parameter"),
            }
        }
        _ => panic!("expected OpenFrame"),
    }

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());

    // Parse and verify Call events exist for the entry function with parameters.
    let events = parse_trace_events(&result);
    let (calls, returns) = count_call_return(&events);
    assert!(calls >= 2, "expected at least 2 Call events (toplevel + entry_transfer), got {calls}");
    assert!(returns >= 1, "expected at least 1 Return event, got {returns}");
}

#[test]
fn test_module_crossing_calls() {
    let trace = vec![
        r#"{"version":3}"#,
        // coin::transfer calls balance::withdraw
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"transfer","module":{"address":"0x2","name":"coin"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":10000}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":9999,"instruction":"Call"}}"#,
        // Cross-module call into balance
        r#"{"OpenFrame":{"frame":{"frame_id":2,"function_name":"withdraw","module":{"address":"0x2","name":"balance"},"type_instantiation":["0x2::sui::SUI"],"parameters":[],"return_types":[],"locals_types":[{"type_":"u64"}],"is_native":false},"gas_left":9998}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":9997,"instruction":"LdU64(500)"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"U64","value":500}}}}}"#,
        r#"{"CloseFrame":{"frame_id":2,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":500}}}],"gas_left":9996}}"#,
        // Back in coin module, call transfer::transfer_internal
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":9995,"instruction":"Call"}}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":3,"function_name":"transfer_internal","module":{"address":"0x2","name":"transfer"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":9994}}"#,
        r#"{"CloseFrame":{"frame_id":3,"gas_left":9993}}"#,
        r#"{"CloseFrame":{"frame_id":1,"gas_left":9992}}"#,
    ]
    .join("\n");

    let (open, close, _, _) = count_events(&trace);
    assert_eq!(open, 3);
    assert_eq!(close, 3);

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());

    // Parse and verify Call/Return for cross-module calls:
    // toplevel + coin::transfer + balance::withdraw + transfer::transfer_internal = 4 Calls
    // coin::transfer + balance::withdraw + transfer::transfer_internal = 3 Returns
    let events = parse_trace_events(&result);
    let (calls, returns) = count_call_return(&events);
    assert_eq!(calls, 4, "expected 4 Call events (toplevel + 3 functions), got {calls}");
    assert_eq!(returns, 4, "expected 4 Return events (3 functions + toplevel), got {returns}");
}

#[test]
fn test_recursive_function_calls() {
    let trace = vec![
        r#"{"version":3}"#,
        // factorial(3)
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"factorial","module":{"address":"0x1","name":"math"},"type_instantiation":[],"parameters":[{"RuntimeValue":{"value":{"type":"U64","value":3}}}],"return_types":[],"locals_types":[{"type_":"u64"}],"is_native":false},"gas_left":10000}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":9999,"instruction":"CopyLoc(0)"}}"#,
        r#"{"Effect":{"Read":{"location":{"Local":[1,0]},"root_value_read":{"RuntimeValue":{"value":{"type":"U64","value":3}}},"moved":false}}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":9998,"instruction":"Call"}}"#,
        // factorial(2) - recursive call
        r#"{"OpenFrame":{"frame":{"frame_id":2,"function_name":"factorial","module":{"address":"0x1","name":"math"},"type_instantiation":[],"parameters":[{"RuntimeValue":{"value":{"type":"U64","value":2}}}],"return_types":[],"locals_types":[{"type_":"u64"}],"is_native":false},"gas_left":9997}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":9996,"instruction":"CopyLoc(0)"}}"#,
        r#"{"Effect":{"Read":{"location":{"Local":[2,0]},"root_value_read":{"RuntimeValue":{"value":{"type":"U64","value":2}}},"moved":false}}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":9995,"instruction":"Call"}}"#,
        // factorial(1) - base case
        r#"{"OpenFrame":{"frame":{"frame_id":3,"function_name":"factorial","module":{"address":"0x1","name":"math"},"type_instantiation":[],"parameters":[{"RuntimeValue":{"value":{"type":"U64","value":1}}}],"return_types":[],"locals_types":[{"type_":"u64"}],"is_native":false},"gas_left":9994}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":9993,"instruction":"LdU64(1)"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"U64","value":1}}}}}"#,
        r#"{"CloseFrame":{"frame_id":3,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":1}}}],"gas_left":9992}}"#,
        // factorial(2) multiplies: 2 * 1 = 2
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":9991,"instruction":"Mul"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"U64","value":2}}}}}"#,
        r#"{"CloseFrame":{"frame_id":2,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":2}}}],"gas_left":9990}}"#,
        // factorial(3) multiplies: 3 * 2 = 6
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":9989,"instruction":"Mul"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"U64","value":6}}}}}"#,
        r#"{"CloseFrame":{"frame_id":1,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":6}}}],"gas_left":9988}}"#,
    ]
    .join("\n");

    let (open, close, _, _) = count_events(&trace);
    assert_eq!(open, 3, "3 recursive OpenFrame events");
    assert_eq!(close, 3, "3 recursive CloseFrame events");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());

    // Parse and verify Call/Return balance for recursive factorial(3) -> factorial(2) -> factorial(1).
    // Expect 4 Call events: 1 toplevel + 3 recursive factorial calls
    // Expect 3 Return events: one per CloseFrame
    let events = parse_trace_events(&result);
    let (calls, returns) = count_call_return(&events);
    assert_eq!(calls, 4, "expected 4 Call events (toplevel + 3 recursive), got {calls}");
    assert_eq!(returns, 4, "expected 4 Return events (3 recursive + toplevel), got {returns}");

    // Verify that Function events were emitted for "factorial"
    let function_names: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Function(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        function_names.iter().any(|n| n.contains("factorial")),
        "expected a Function event for 'factorial', got: {:?}",
        function_names
    );
}

// ============================================================================
// 3. Variable tracking via Effects
// ============================================================================

#[test]
fn test_effect_push_pop() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"push_pop_test","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":999,"instruction":"LdU64(10)"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"U64","value":10}}}}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":998,"instruction":"LdU64(20)"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"U64","value":20}}}}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":997,"instruction":"Add"}}"#,
        r#"{"Effect":{"Pop":{"RuntimeValue":{"value":{"type":"U64","value":20}}}}}"#,
        r#"{"Effect":{"Pop":{"RuntimeValue":{"value":{"type":"U64","value":10}}}}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"U64","value":30}}}}}"#,
        r#"{"CloseFrame":{"frame_id":1,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":30}}}],"gas_left":996}}"#,
    ]
    .join("\n");

    let (_, _, _, effect) = count_events(&trace);
    assert_eq!(effect, 5, "2 pushes + 2 pops + 1 push result");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());

    // Parse and verify the converter produced events. The push/pop effects
    // should result in Step events and at least one Call/Return pair.
    let events = parse_trace_events(&result);
    let (calls, returns) = count_call_return(&events);
    assert!(calls >= 1, "expected at least 1 Call event, got {calls}");
    assert!(returns >= 1, "expected at least 1 Return event, got {returns}");
    let steps = extract_step_lines(&events);
    assert!(!steps.is_empty(), "expected at least one Step event from instructions");
}

#[test]
fn test_effect_read_write_locals() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"rw_test","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"u64"},{"type_":"u64"}],"is_native":false},"gas_left":1000}}"#,
        // Write to local_0
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":999,"instruction":"LdU64(42)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":42}}}}}}"#,
        // Read local_0
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":998,"instruction":"CopyLoc(0)"}}"#,
        r#"{"Effect":{"Read":{"location":{"Local":[1,0]},"root_value_read":{"RuntimeValue":{"value":{"type":"U64","value":42}}},"moved":false}}}"#,
        // Write to local_1
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":997,"instruction":"StLoc(1)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,1]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":42}}}}}}"#,
        // Read local_1
        r#"{"Instruction":{"type_parameters":[],"pc":3,"gas_left":996,"instruction":"MoveLoc(1)"}}"#,
        r#"{"Effect":{"Read":{"location":{"Local":[1,1]},"root_value_read":{"RuntimeValue":{"value":{"type":"U64","value":42}}},"moved":false}}}"#,
        r#"{"CloseFrame":{"frame_id":1,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":42}}}],"gas_left":995}}"#,
    ]
    .join("\n");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());

    // Parse and verify the read/write effects generated Value events.
    // The trace writes 42 to local_0, reads it, writes to local_1, reads it.
    let events = parse_trace_events(&result);

    // Should have Value events for the write effects
    let value_count = events
        .iter()
        .filter(|e| matches!(e, TraceLowLevelEvent::Value(_)))
        .count();
    assert!(
        value_count >= 2,
        "expected at least 2 Value events from Write effects, got {value_count}"
    );

    // Verify Step events exist for the 4 instruction PCs
    let steps = extract_step_lines(&events);
    assert!(!steps.is_empty(), "expected Step events from instructions");
}

#[test]
fn test_effect_mut_ref_tracking() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"mutref_test","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"u64"}],"is_native":false},"gas_left":1000}}"#,
        // Write initial value
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":10}}}}}}"#,
        // MutRef borrow
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":999,"instruction":"MutBorrowLoc(0)"}}"#,
        r#"{"Effect":{"Push":{"MutRef":{"location":{"Local":[1,0]},"snapshot":{"type":"U64","value":10}}}}}"#,
        // Write through ref (value changes)
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":20}}}}}}"#,
        r#"{"CloseFrame":{"frame_id":1,"gas_left":998}}"#,
    ]
    .join("\n");

    // Verify MutRef parsing
    let mut lines = trace.lines();
    lines.next(); // version
    lines.next(); // OpenFrame
    lines.next(); // Write effect
    lines.next(); // Instruction
    let push_line = lines.next().unwrap();
    let event: TraceEvent = serde_json::from_str(push_line).unwrap();
    match event {
        TraceEvent::Effect(effect) => match effect {
            codetracer_move_recorder::move_types::Effect::Push(value) => {
                match &value {
                    TraceValue::MutRef { location, snapshot } => {
                        assert_eq!(location.local_index(), 0);
                        match snapshot {
                            SerializableMoveValue::U64 { value } => assert_eq!(*value, 10),
                            _ => panic!("expected U64 snapshot"),
                        }
                    }
                    _ => panic!("expected MutRef"),
                }
            }
            _ => panic!("expected Push effect"),
        },
        _ => panic!("expected Effect event"),
    }

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
}

#[test]
fn test_effect_imm_ref_tracking() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"immref_test","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"u64"}],"is_native":false},"gas_left":1000}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":77}}}}}}"#,
        // ImmRef borrow
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":999,"instruction":"ImmBorrowLoc(0)"}}"#,
        r#"{"Effect":{"Push":{"ImmRef":{"location":{"Local":[1,0]},"snapshot":{"type":"U64","value":77}}}}}"#,
        // Read through ref
        r#"{"Effect":{"Read":{"location":{"Local":[1,0]},"root_value_read":{"ImmRef":{"location":{"Local":[1,0]},"snapshot":{"type":"U64","value":77}}},"moved":false}}}"#,
        r#"{"CloseFrame":{"frame_id":1,"gas_left":998}}"#,
    ]
    .join("\n");

    // Verify ImmRef parsing
    let mut lines = trace.lines();
    lines.next(); // version
    lines.next(); // OpenFrame
    lines.next(); // Write
    lines.next(); // Instruction
    let push_line = lines.next().unwrap();
    let event: TraceEvent = serde_json::from_str(push_line).unwrap();
    match event {
        TraceEvent::Effect(effect) => match effect {
            codetracer_move_recorder::move_types::Effect::Push(value) => {
                match &value {
                    TraceValue::ImmRef { location, snapshot } => {
                        assert_eq!(location.local_index(), 0);
                        match snapshot {
                            SerializableMoveValue::U64 { value } => assert_eq!(*value, 77),
                            _ => panic!("expected U64 snapshot"),
                        }
                    }
                    _ => panic!("expected ImmRef"),
                }
            }
            _ => panic!("expected Push effect"),
        },
        _ => panic!("expected Effect event"),
    }

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
}

// ============================================================================
// 4. Control flow patterns
// ============================================================================

#[test]
fn test_linear_execution_with_source_map() {
    let source_map = SourceMapResolver::from_entries(vec![
        ("linear".to_string(), 0, "linear.move".to_string(), 5),
        ("linear".to_string(), 1, "linear.move".to_string(), 6),
        ("linear".to_string(), 2, "linear.move".to_string(), 7),
        ("linear".to_string(), 3, "linear.move".to_string(), 8),
        ("linear".to_string(), 4, "linear.move".to_string(), 9),
    ]);

    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"linear_fn","module":{"address":"0x0","name":"linear"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"u64"},{"type_":"u64"},{"type_":"u64"},{"type_":"u64"},{"type_":"u64"}],"is_native":false},"gas_left":10000}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":9999,"instruction":"LdU64(1)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":1}}}}}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":9998,"instruction":"LdU64(2)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,1]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":2}}}}}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":9997,"instruction":"LdU64(3)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,2]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":3}}}}}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":3,"gas_left":9996,"instruction":"LdU64(4)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,3]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":4}}}}}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":4,"gas_left":9995,"instruction":"LdU64(5)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,4]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":5}}}}}}"#,
        r#"{"CloseFrame":{"frame_id":1,"gas_left":9990}}"#,
    ]
    .join("\n");

    let (trace_content, metadata, _) = run_converter(&trace, &source_map, "linear.move");
    assert!(!trace_content.is_empty());
    assert!(metadata.get("program").is_some());

    // Parse and verify Step events map to the expected source lines 5-9
    // (pc 0->line 5, pc 1->line 6, ..., pc 4->line 9).
    let events = parse_trace_events(&trace_content);
    let step_lines = extract_step_lines(&events);

    let instruction_step_lines: Vec<i64> = step_lines
        .iter()
        .copied()
        .filter(|&line| (5..=9).contains(&line))
        .collect();

    let mut unique_lines = instruction_step_lines.clone();
    unique_lines.sort();
    unique_lines.dedup();
    assert_eq!(
        unique_lines,
        vec![5, 6, 7, 8, 9],
        "expected steps for lines 5 through 9 from linear execution"
    );
}

#[test]
fn test_branch_pattern() {
    // Simulates: if (x > 5) { a = 10 } else { a = 20 }
    // Branch taken: pc jumps from 2 to 5 (skipping 3,4)
    let source_map = SourceMapResolver::from_entries(vec![
        ("branch".to_string(), 0, "branch.move".to_string(), 3),
        ("branch".to_string(), 1, "branch.move".to_string(), 4),
        ("branch".to_string(), 2, "branch.move".to_string(), 5),  // BrTrue
        ("branch".to_string(), 5, "branch.move".to_string(), 8),  // else branch target
        ("branch".to_string(), 6, "branch.move".to_string(), 9),
    ]);

    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"branch_fn","module":{"address":"0x0","name":"branch"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"u64"},{"type_":"u64"}],"is_native":false},"gas_left":1000}}"#,
        // Load x = 3
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":999,"instruction":"LdU64(3)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":3}}}}}}"#,
        // Compare: x > 5 => false
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":998,"instruction":"Gt"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"Bool","value":false}}}}}"#,
        // BrTrue (false, so fall through to else)
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":997,"instruction":"BrTrue(4)"}}"#,
        r#"{"Effect":{"Pop":{"RuntimeValue":{"value":{"type":"Bool","value":false}}}}}"#,
        // Else branch: a = 20 (jumps to pc=5)
        r#"{"Instruction":{"type_parameters":[],"pc":5,"gas_left":996,"instruction":"LdU64(20)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,1]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":20}}}}}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":6,"gas_left":995,"instruction":"Ret"}}"#,
        r#"{"CloseFrame":{"frame_id":1,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":20}}}],"gas_left":994}}"#,
    ]
    .join("\n");

    let (trace_content, _, _) = run_converter(&trace, &source_map, "branch.move");
    assert!(!trace_content.is_empty());

    // Parse and verify that the branch produced steps on the else path.
    // Source map: pc 0->line 3, pc 1->line 4, pc 2->line 5, pc 5->line 8, pc 6->line 9
    // Since BrTrue was false, we should see steps for lines 3, 4, 5 (condition), 8, 9 (else branch)
    // but NOT line 6 or 7 (the if-true branch, which was skipped).
    let events = parse_trace_events(&trace_content);
    let step_lines = extract_step_lines(&events);

    let instruction_step_lines: Vec<i64> = step_lines
        .iter()
        .copied()
        .filter(|&line| line != 1) // exclude toplevel initial step
        .collect();

    // Should have steps on line 8 (else branch) and line 9 (return)
    assert!(
        instruction_step_lines.contains(&8),
        "expected step on line 8 (else branch), got steps: {:?}",
        instruction_step_lines
    );
    assert!(
        instruction_step_lines.contains(&9),
        "expected step on line 9 (return in else), got steps: {:?}",
        instruction_step_lines
    );
}

#[test]
fn test_loop_pattern() {
    // Simulates: while (i < 3) { sum = sum + i; i = i + 1; }
    // PCs 2-5 repeat 3 times
    let source_map = SourceMapResolver::from_entries(vec![
        ("loop_mod".to_string(), 0, "loop.move".to_string(), 3),
        ("loop_mod".to_string(), 1, "loop.move".to_string(), 4),
        ("loop_mod".to_string(), 2, "loop.move".to_string(), 5),  // loop condition
        ("loop_mod".to_string(), 3, "loop.move".to_string(), 6),  // loop body
        ("loop_mod".to_string(), 4, "loop.move".to_string(), 7),  // increment
        ("loop_mod".to_string(), 5, "loop.move".to_string(), 5),  // back to condition (same line)
        ("loop_mod".to_string(), 6, "loop.move".to_string(), 9),  // after loop
    ]);

    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"loop_fn","module":{"address":"0x0","name":"loop_mod"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"u64"},{"type_":"u64"}],"is_native":false},"gas_left":10000}}"#,
        // i = 0
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":9999,"instruction":"LdU64(0)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":0}}}}}}"#,
        // sum = 0
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":9998,"instruction":"LdU64(0)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,1]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":0}}}}}}"#,
        // Iteration 1: i=0, check i<3
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":9997,"instruction":"Lt"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"Bool","value":true}}}}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":3,"gas_left":9996,"instruction":"Add"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,1]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":0}}}}}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":4,"gas_left":9995,"instruction":"Add"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":1}}}}}}"#,
        // Iteration 2: i=1, check i<3
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":9994,"instruction":"Lt"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"Bool","value":true}}}}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":3,"gas_left":9993,"instruction":"Add"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,1]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":1}}}}}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":4,"gas_left":9992,"instruction":"Add"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":2}}}}}}"#,
        // Iteration 3: i=2, check i<3
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":9991,"instruction":"Lt"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"Bool","value":true}}}}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":3,"gas_left":9990,"instruction":"Add"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,1]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":3}}}}}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":4,"gas_left":9989,"instruction":"Add"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":3}}}}}}"#,
        // Exit: i=3, check i<3 => false
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":9988,"instruction":"Lt"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"Bool","value":false}}}}}"#,
        // After loop
        r#"{"Instruction":{"type_parameters":[],"pc":6,"gas_left":9987,"instruction":"Ret"}}"#,
        r#"{"CloseFrame":{"frame_id":1,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":3}}}],"gas_left":9986}}"#,
    ]
    .join("\n");

    let (trace_content, _, _) = run_converter(&trace, &source_map, "loop.move");
    assert!(!trace_content.is_empty());

    // Verify event counts: should have repeated pc=2 four times (3 true + 1 false)
    let (_, _, instr, _) = count_events(&trace);
    assert!(instr >= 10, "loop should produce many instruction events, got {instr}");

    // Parse and verify Step events show the loop body lines repeating.
    // Source map: pc 2->line 5 (condition), pc 3->line 6 (body), pc 4->line 7 (increment)
    // The loop runs 3 iterations, so line 5 should appear multiple times (transitions
    // from other lines back to line 5).
    let events = parse_trace_events(&trace_content);
    let step_lines = extract_step_lines(&events);

    let instruction_step_lines: Vec<i64> = step_lines
        .iter()
        .copied()
        .filter(|&line| line != 1)
        .collect();

    // Lines from the loop body (5, 6, 7) should all appear
    assert!(
        instruction_step_lines.contains(&5),
        "expected step on line 5 (loop condition), got steps: {:?}",
        instruction_step_lines
    );
    assert!(
        instruction_step_lines.contains(&6),
        "expected step on line 6 (loop body), got steps: {:?}",
        instruction_step_lines
    );
    assert!(
        instruction_step_lines.contains(&7),
        "expected step on line 7 (increment), got steps: {:?}",
        instruction_step_lines
    );

    // Line 9 (after loop) should appear once at the end
    assert!(
        instruction_step_lines.contains(&9),
        "expected step on line 9 (after loop), got steps: {:?}",
        instruction_step_lines
    );

    // The loop body lines should repeat: total step count should be > unique line count
    // due to 3 loop iterations
    assert!(
        instruction_step_lines.len() > 4,
        "expected more than 4 instruction steps due to loop iterations, got {}",
        instruction_step_lines.len()
    );
}

#[test]
fn test_execution_error_abort() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"will_abort","module":{"address":"0x0","name":"abort_mod"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"u64"}],"is_native":false},"gas_left":1000}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":999,"instruction":"LdU64(0)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":0}}}}}}"#,
        // Abort instruction triggers ExecutionError
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":998,"instruction":"Abort"}}"#,
        r#"{"Effect":{"ExecutionError":"ABORT with code 42"}}"#,
        r#"{"CloseFrame":{"frame_id":1,"gas_left":997}}"#,
    ]
    .join("\n");

    // Should not panic, should handle ExecutionError gracefully
    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
}

// ============================================================================
// 5. Realistic Move scenarios
// ============================================================================

#[test]
fn test_scenario_token_transfer() {
    // Simulates coin::transfer<SUI>(coin, recipient, amount)
    let source_map = SourceMapResolver::from_entries(vec![
        ("coin".to_string(), 0, "coin.move".to_string(), 10),
        ("coin".to_string(), 1, "coin.move".to_string(), 11),
        ("coin".to_string(), 2, "coin.move".to_string(), 12),
        ("coin".to_string(), 3, "coin.move".to_string(), 13),
        ("balance".to_string(), 0, "balance.move".to_string(), 20),
        ("balance".to_string(), 1, "balance.move".to_string(), 21),
    ]);

    let trace = vec![
        r#"{"version":3}"#,
        // coin::transfer entry point
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"transfer","module":{"address":"0x2","name":"coin"},"type_instantiation":["0x2::sui::SUI"],"parameters":[{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"0x2::coin::Coin"},"fields":[["field_0",{"type":"Struct","value":{"type_":{"name":"0x2::object::UID"},"fields":[["field_0",{"type":"Address","value":"0xOBJ1"}]]}}],["field_1",{"type":"Struct","value":{"type_":{"name":"0x2::balance::Balance"},"fields":[["field_0",{"type":"U64","value":1000}]]}}]]}}}},{"RuntimeValue":{"value":{"type":"Address","value":"0xRECIPIENT"}}},{"RuntimeValue":{"value":{"type":"U64","value":500}}}],"return_types":[],"locals_types":[{"type_":"0x2::coin::Coin"},{"type_":"address"},{"type_":"u64"},{"type_":"0x2::balance::Balance"}],"is_native":false},"gas_left":100000}}"#,
        // Read the coin struct
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":99999,"instruction":"CopyLoc(0)"}}"#,
        r#"{"Effect":{"Read":{"location":{"Local":[1,0]},"root_value_read":{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"0x2::coin::Coin"},"fields":[["field_0",{"type":"Struct","value":{"type_":{"name":"0x2::object::UID"},"fields":[["field_0",{"type":"Address","value":"0xOBJ1"}]]}}],["field_1",{"type":"Struct","value":{"type_":{"name":"0x2::balance::Balance"},"fields":[["field_0",{"type":"U64","value":1000}]]}}]]}}}},"moved":false}}}"#,
        // Call balance::split to extract amount
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":99998,"instruction":"Call"}}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":2,"function_name":"split","module":{"address":"0x2","name":"balance"},"type_instantiation":["0x2::sui::SUI"],"parameters":[],"return_types":[],"locals_types":[{"type_":"u64"}],"is_native":false},"gas_left":99997}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":99996,"instruction":"LdU64(500)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[2,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":500}}}}}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":99995,"instruction":"Ret"}}"#,
        r#"{"CloseFrame":{"frame_id":2,"return_":[{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"0x2::balance::Balance"},"fields":[["field_0",{"type":"U64","value":500}]]}}}}],"gas_left":99994}}"#,
        // Store split balance
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":99993,"instruction":"StLoc(3)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,3]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"0x2::balance::Balance"},"fields":[["field_0",{"type":"U64","value":500}]]}}}}}}}"#,
        // Transfer to recipient
        r#"{"Instruction":{"type_parameters":[],"pc":3,"gas_left":99992,"instruction":"Call"}}"#,
        r#"{"CloseFrame":{"frame_id":1,"gas_left":99991}}"#,
    ]
    .join("\n");

    let (trace_content, metadata, _) = run_converter(&trace, &source_map, "coin.move");
    assert!(!trace_content.is_empty());
    assert!(metadata.get("program").is_some());

    let (open, close, _, _) = count_events(&trace);
    assert_eq!(open, 2, "outer transfer + inner split");
    assert_eq!(close, 2);

    // Parse and verify Call/Return events for the token transfer scenario.
    // Expect 3 Calls: toplevel + coin::transfer + balance::split
    // Expect 2 Returns: balance::split + coin::transfer
    let events = parse_trace_events(&trace_content);
    let (calls, returns) = count_call_return(&events);
    assert_eq!(calls, 3, "expected 3 Call events (toplevel + transfer + split), got {calls}");
    assert_eq!(returns, 3, "expected 3 Return events (transfer + split + toplevel), got {returns}");

    // Verify Step events reference lines from the source map
    let step_lines = extract_step_lines(&events);
    let coin_lines: Vec<i64> = step_lines
        .iter()
        .copied()
        .filter(|&line| (10..=13).contains(&line))
        .collect();
    assert!(
        !coin_lines.is_empty(),
        "expected steps on coin.move source lines (10-13)"
    );

    // Verify the call tree structure by checking function names in order.
    let mut function_names: std::collections::HashMap<usize, String> =
        std::collections::HashMap::new();
    let mut next_fn_id = 0usize;
    for event in &events {
        if let TraceLowLevelEvent::Function(func) = event {
            function_names.insert(next_fn_id, func.name.clone());
            next_fn_id += 1;
        }
    }

    let call_fn_names: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Call(call) => function_names.get(&call.function_id.0).cloned(),
            _ => None,
        })
        .collect();

    assert_eq!(call_fn_names.len(), 3);
    assert_eq!(call_fn_names[0], "<toplevel>", "first call is toplevel");
    assert_eq!(call_fn_names[1], "transfer", "second call is coin::transfer");
    assert_eq!(call_fn_names[2], "split", "third call is balance::split");

    // Verify variable tracking: the converter should record Write effects as
    // variables. Check that local_0 (the coin struct) was recorded.
    let variable_events: Vec<&codetracer_trace_types::FullValueRecord> = events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Value(val) => Some(val),
            _ => None,
        })
        .collect();
    assert!(
        !variable_events.is_empty(),
        "should have variable value events from Write/Read effects"
    );

    // Verify that the split function's return value (Balance with 500) is captured.
    let return_values: Vec<&codetracer_trace_types::ReturnRecord> = events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Return(ret) => Some(ret),
            _ => None,
        })
        .collect();
    assert_eq!(return_values.len(), 3, "expected 3 returns (split + transfer + toplevel)");
    // The first return is from balance::split which returns a Balance struct.
    // It should be serialized as a String (struct rendering).
    match &return_values[0].return_value {
        codetracer_trace_types::ValueRecord::String { text, .. } => {
            assert!(
                text.contains("500"),
                "split return value should contain 500, got: {text}"
            );
        }
        other => panic!(
            "expected String value for struct return from split, got: {:?}",
            other
        ),
    }

    // Verify that source map produces steps on both coin.move and balance.move lines.
    // balance module maps pc 0->line 20, pc 1->line 21.
    let balance_lines: Vec<i64> = step_lines
        .iter()
        .copied()
        .filter(|&line| (20..=21).contains(&line))
        .collect();
    assert!(
        !balance_lines.is_empty(),
        "expected steps on balance.move source lines (20-21)"
    );
}

#[test]
fn test_scenario_object_creation() {
    // Simulates creating an object with a UID
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"create","module":{"address":"0x1","name":"nft"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"0x2::object::UID"},{"type_":"0x1::nft::NFT"}],"is_native":false},"gas_left":50000}}"#,
        // Create UID via object::new
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":49999,"instruction":"Call"}}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":2,"function_name":"new","module":{"address":"0x2","name":"object"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":49998}}"#,
        r#"{"CloseFrame":{"frame_id":2,"return_":[{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"0x2::object::UID"},"fields":[["field_0",{"type":"Address","value":"0xUID_ADDR_123"}]]}}}}],"gas_left":49997}}"#,
        // Store UID
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"0x2::object::UID"},"fields":[["field_0",{"type":"Address","value":"0xUID_ADDR_123"}]]}}}}}}}"#,
        // Pack NFT struct: NFT { id: uid, name_length: 5, value: 100 }
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":49996,"instruction":"Pack(NFT)"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"0x1::nft::NFT"},"fields":[["field_0",{"type":"Struct","value":{"type_":{"name":"0x2::object::UID"},"fields":[["field_0",{"type":"Address","value":"0xUID_ADDR_123"}]]}}],["field_1",{"type":"U64","value":5}],["field_2",{"type":"U64","value":100}]]}}}}}}"#,
        // Store NFT
        r#"{"Effect":{"Write":{"location":{"Local":[1,1]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"0x1::nft::NFT"},"fields":[["field_0",{"type":"Struct","value":{"type_":{"name":"0x2::object::UID"},"fields":[["field_0",{"type":"Address","value":"0xUID_ADDR_123"}]]}}],["field_1",{"type":"U64","value":5}],["field_2",{"type":"U64","value":100}]]}}}}}}}"#,
        // Transfer the NFT
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":49995,"instruction":"Call"}}"#,
        r#"{"CloseFrame":{"frame_id":1,"gas_left":49990}}"#,
    ]
    .join("\n");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
}

#[test]
fn test_scenario_vector_manipulation() {
    // Simulates: vector::push_back, vector::pop_back, vector::length
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"vec_ops","module":{"address":"0x0","name":"vec_test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"vector<u64>"},{"type_":"u64"}],"is_native":false},"gas_left":10000}}"#,
        // Create empty vector
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":9999,"instruction":"VecPack(0)"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"Vector","elements":[]}}}}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Vector","elements":[]}}}}}}"#,
        // push_back(10)
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":9998,"instruction":"VecPushBack"}}"#,
        r#"{"Effect":{"Pop":{"RuntimeValue":{"value":{"type":"U64","value":10}}}}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Vector","elements":[{"type":"U64","value":10}]}}}}}}"#,
        // push_back(20)
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":9997,"instruction":"VecPushBack"}}"#,
        r#"{"Effect":{"Pop":{"RuntimeValue":{"value":{"type":"U64","value":20}}}}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Vector","elements":[{"type":"U64","value":10},{"type":"U64","value":20}]}}}}}}"#,
        // push_back(30)
        r#"{"Instruction":{"type_parameters":[],"pc":3,"gas_left":9996,"instruction":"VecPushBack"}}"#,
        r#"{"Effect":{"Pop":{"RuntimeValue":{"value":{"type":"U64","value":30}}}}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Vector","elements":[{"type":"U64","value":10},{"type":"U64","value":20},{"type":"U64","value":30}]}}}}}}"#,
        // pop_back => 30
        r#"{"Instruction":{"type_parameters":[],"pc":4,"gas_left":9995,"instruction":"VecPopBack"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"U64","value":30}}}}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Vector","elements":[{"type":"U64","value":10},{"type":"U64","value":20}]}}}}}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,1]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":30}}}}}}"#,
        // vector::length => 2
        r#"{"Instruction":{"type_parameters":[],"pc":5,"gas_left":9994,"instruction":"VecLen"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"U64","value":2}}}}}"#,
        r#"{"CloseFrame":{"frame_id":1,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":2}}}],"gas_left":9993}}"#,
    ]
    .join("\n");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());

    // Parse and verify the vector operations generated Value events.
    // The trace has multiple Write effects (empty vec, [10], [10,20], [10,20,30], [10,20], popped=30).
    let events = parse_trace_events(&result);

    let value_count = events
        .iter()
        .filter(|e| matches!(e, TraceLowLevelEvent::Value(_)))
        .count();
    assert!(
        value_count >= 4,
        "expected at least 4 Value events from vector Write effects, got {value_count}"
    );

    // Should have Step events for the 6 instruction PCs
    let steps = extract_step_lines(&events);
    assert!(!steps.is_empty(), "expected Step events from vector instruction PCs");
}

#[test]
fn test_scenario_error_abort_with_code() {
    // Simulates: assert!(balance >= amount, EInsufficientBalance) which aborts
    let source_map = SourceMapResolver::from_entries(vec![
        ("pay".to_string(), 0, "pay.move".to_string(), 15),
        ("pay".to_string(), 1, "pay.move".to_string(), 16),
        ("pay".to_string(), 2, "pay.move".to_string(), 17),
    ]);

    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"pay","module":{"address":"0x2","name":"pay"},"type_instantiation":[],"parameters":[{"RuntimeValue":{"value":{"type":"U64","value":100}}},{"RuntimeValue":{"value":{"type":"U64","value":500}}}],"return_types":[],"locals_types":[{"type_":"u64"},{"type_":"u64"}],"is_native":false},"gas_left":5000}}"#,
        // Load balance = 100
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":4999,"instruction":"CopyLoc(0)"}}"#,
        r#"{"Effect":{"Read":{"location":{"Local":[1,0]},"root_value_read":{"RuntimeValue":{"value":{"type":"U64","value":100}}},"moved":false}}}"#,
        // Load amount = 500
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":4998,"instruction":"CopyLoc(1)"}}"#,
        r#"{"Effect":{"Read":{"location":{"Local":[1,1]},"root_value_read":{"RuntimeValue":{"value":{"type":"U64","value":500}}},"moved":false}}}"#,
        // Check balance >= amount => false, abort
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":4997,"instruction":"Abort"}}"#,
        r#"{"Effect":{"ExecutionError":"ABORT with code 1 (EInsufficientBalance)"}}"#,
        r#"{"CloseFrame":{"frame_id":1,"gas_left":4996}}"#,
    ]
    .join("\n");

    let (trace_content, _, _) = run_converter(&trace, &source_map, "pay.move");
    assert!(!trace_content.is_empty());
}

// ============================================================================
// Edge cases and robustness
// ============================================================================

#[test]
fn test_empty_return_values() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"void_fn","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000}}"#,
        // No return values (void function)
        r#"{"CloseFrame":{"frame_id":1,"gas_left":999}}"#,
    ]
    .join("\n");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
}

#[test]
fn test_close_frame_with_null_return() {
    // return_ field is explicitly null or absent
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"no_ret","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000}}"#,
        r#"{"CloseFrame":{"frame_id":1,"return_":[],"gas_left":999}}"#,
    ]
    .join("\n");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
}

#[test]
fn test_multiple_return_values() {
    // Move functions can return tuples
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"multi_ret","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000}}"#,
        r#"{"CloseFrame":{"frame_id":1,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":1}}},{"RuntimeValue":{"value":{"type":"Bool","value":true}}},{"RuntimeValue":{"value":{"type":"Address","value":"0xABC"}}}],"gas_left":999}}"#,
    ]
    .join("\n");

    // The converter only uses the first return value
    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
}

#[test]
fn test_native_function_frame() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"main","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":10000}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":9999,"instruction":"Call"}}"#,
        // Native function call
        r#"{"OpenFrame":{"frame":{"frame_id":2,"function_name":"native_hash","module":{"address":"0x1","name":"hash"},"type_instantiation":[],"parameters":[{"RuntimeValue":{"value":{"type":"Vector","elements":[{"type":"U8","value":1},{"type":"U8","value":2}]}}}],"return_types":[],"locals_types":[],"is_native":true},"gas_left":9998}}"#,
        r#"{"CloseFrame":{"frame_id":2,"return_":[{"RuntimeValue":{"value":{"type":"Vector","elements":[{"type":"U8","value":100},{"type":"U8","value":200}]}}}],"gas_left":9997}}"#,
        r#"{"CloseFrame":{"frame_id":1,"gas_left":9996}}"#,
    ]
    .join("\n");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
}

#[test]
fn test_data_load_effect() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"load_test","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000}}"#,
        r#"{"Effect":{"DataLoad":{"address":"0xSOME_OBJ_ADDR"}}}"#,
        r#"{"CloseFrame":{"frame_id":1,"gas_left":999}}"#,
    ]
    .join("\n");

    // DataLoad should be handled gracefully (no-op)
    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
}

#[test]
fn test_external_effect() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"ext_test","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000}}"#,
        r#"{"External":{"kind":"transfer_object"}}"#,
        r#"{"CloseFrame":{"frame_id":1,"gas_left":999}}"#,
    ]
    .join("\n");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
}

#[test]
fn test_wrong_version_rejected() {
    let trace = r#"{"version":2}"#;
    let tmp = tempfile::TempDir::new().unwrap();
    let out_dir = tmp.path().join("ct-out");
    let result = converter::convert_trace(
        trace.as_bytes(),
        &SourceMapResolver::empty(),
        Path::new("test.move"),
        &out_dir,
        TraceEventsFileFormat::Json,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("unsupported trace format version"),
        "error should mention version, got: {err}"
    );
}

#[test]
fn test_empty_trace_data_rejected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let out_dir = tmp.path().join("ct-out");
    let result = converter::convert_trace(
        b"",
        &SourceMapResolver::empty(),
        Path::new("test.move"),
        &out_dir,
        TraceEventsFileFormat::Json,
    );
    assert!(result.is_err());
}

#[test]
fn test_deeply_nested_struct() {
    // Struct containing struct containing struct
    let v: SerializableMoveValue = serde_json::from_str(
        r#"{"type":"Struct","value":{"type_":{"name":"Outer"},"fields":[["field_0",{"type":"Struct","value":{"type_":{"name":"Middle"},"fields":[["field_0",{"type":"Struct","value":{"type_":{"name":"Inner"},"fields":[["field_0",{"type":"U64","value":42}]]}}]]}}]]}}"#,
    )
    .unwrap();
    match v {
        SerializableMoveValue::Struct { value: outer } => {
            assert_eq!(outer.type_.get("name").and_then(|v| v.as_str()), Some("Outer"));
            match &outer.fields[0].1 {
                SerializableMoveValue::Struct { value: middle } => {
                    assert_eq!(middle.type_.get("name").and_then(|v| v.as_str()), Some("Middle"));
                    match &middle.fields[0].1 {
                        SerializableMoveValue::Struct { value: inner } => {
                            assert_eq!(inner.type_.get("name").and_then(|v| v.as_str()), Some("Inner"));
                            match &inner.fields[0].1 {
                                SerializableMoveValue::U64 { value } => assert_eq!(*value, 42),
                                _ => panic!("expected U64 at innermost level"),
                            }
                        }
                        _ => panic!("expected Inner struct"),
                    }
                }
                _ => panic!("expected Middle struct"),
            }
        }
        _ => panic!("expected Outer struct"),
    }
}

#[test]
fn test_deeply_nested_struct_through_converter() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"deep_test","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"Outer"},"fields":[["field_0",{"type":"Struct","value":{"type_":{"name":"Middle"},"fields":[["field_0",{"type":"Struct","value":{"type_":{"name":"Inner"},"fields":[["field_0",{"type":"U64","value":42}]]}}]]}}]]}}}}}}"#,
        r#"{"CloseFrame":{"frame_id":1,"return_":[{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"Outer"},"fields":[["field_0",{"type":"Struct","value":{"type_":{"name":"Middle"},"fields":[["field_0",{"type":"Struct","value":{"type_":{"name":"Inner"},"fields":[["field_0",{"type":"U64","value":42}]]}}]]}}]]}}}}],"gas_left":999}}"#,
    ]
    .join("\n");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());

    // Parse and verify the deeply nested struct produced events.
    let events = parse_trace_events(&result);
    let (calls, returns) = count_call_return(&events);
    assert!(calls >= 1, "expected at least 1 Call event (toplevel), got {calls}");
    assert!(returns >= 1, "expected at least 1 Return event, got {returns}");
}

#[test]
fn test_large_vector() {
    // Vector with many elements
    let elements: Vec<String> = (0..50)
        .map(|i| format!(r#"{{"type":"U8","value":{}}}"#, i % 256))
        .collect();
    let json = format!(
        r#"{{"type":"Vector","elements":[{}]}}"#,
        elements.join(",")
    );
    let v: SerializableMoveValue = serde_json::from_str(&json).unwrap();
    match v {
        SerializableMoveValue::Vector { elements } => {
            assert_eq!(elements.len(), 50);
        }
        _ => panic!("expected Vector"),
    }
}

#[test]
fn test_empty_vector() {
    let v: SerializableMoveValue =
        serde_json::from_str(r#"{"type":"Vector","elements":[]}"#).unwrap();
    match v {
        SerializableMoveValue::Vector { elements } => {
            assert!(elements.is_empty());
        }
        _ => panic!("expected Vector"),
    }
}

#[test]
fn test_struct_with_no_type_name() {
    let v: SerializableMoveValue = serde_json::from_str(
        r#"{"type":"Struct","value":{"type_":{},"fields":[["field_0",{"type":"U64","value":1}]]}}"#,
    )
    .unwrap();
    match v {
        SerializableMoveValue::Struct { value: content } => {
            // type_ is an empty JSON object when no type name is provided
            assert!(
                content.type_.get("name").is_none(),
                "type_ should have no name field when empty"
            );
        }
        _ => panic!("expected Struct"),
    }
}

#[test]
fn test_variant_with_no_type_name() {
    let v: SerializableMoveValue = serde_json::from_str(
        r#"{"type":"Variant","tag":0,"fields":[{"type":"Bool","value":true}]}"#,
    )
    .unwrap();
    match v {
        SerializableMoveValue::Variant { tag, type_, .. } => {
            assert_eq!(tag, 0);
            assert!(type_.is_empty());
        }
        _ => panic!("expected Variant"),
    }
}

#[test]
fn test_source_map_dedup_same_line_no_duplicate_steps() {
    // When consecutive instructions map to the same line, only one step should be emitted.
    // The converter checks prev_line != Some(line) before emitting a step.
    let source_map = SourceMapResolver::from_entries(vec![
        ("dedup".to_string(), 0, "dedup.move".to_string(), 5),
        ("dedup".to_string(), 1, "dedup.move".to_string(), 5),  // same line
        ("dedup".to_string(), 2, "dedup.move".to_string(), 5),  // same line
        ("dedup".to_string(), 3, "dedup.move".to_string(), 6),  // different line
    ]);

    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"dedup_fn","module":{"address":"0x0","name":"dedup"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":999,"instruction":"LdU64(1)"}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":998,"instruction":"LdU64(2)"}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":997,"instruction":"Add"}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":3,"gas_left":996,"instruction":"Ret"}}"#,
        r#"{"CloseFrame":{"frame_id":1,"gas_left":995}}"#,
    ]
    .join("\n");

    // This should succeed - the converter deduplicates steps on the same line
    let (trace_content, _, _) = run_converter(&trace, &source_map, "dedup.move");
    assert!(!trace_content.is_empty());

    // Parse trace.json and verify deduplication: consecutive instructions on the
    // same line should produce only one Step event per line transition.
    // The converter emits an initial Step(line 1) from start(), then:
    // pc 0,1,2 all map to line 5 => one Step(line 5)
    // pc 3 maps to line 6 => one Step(line 6)
    // Total: 3 Step events (initial + 2 from instructions), NOT 5 (initial + 4 per-instruction)
    let events: Vec<TraceLowLevelEvent> =
        serde_json::from_str(&trace_content).expect("trace.json should be valid JSON array");

    let step_lines: Vec<i64> = events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Step(step) => Some(step.line.0),
            _ => None,
        })
        .collect();

    // Filter out the initial step (line 1 from start()) to check only instruction-derived steps
    let instruction_step_lines: Vec<i64> = step_lines
        .iter()
        .copied()
        .filter(|&line| line != 1)
        .collect();

    assert_eq!(
        instruction_step_lines.len(),
        2,
        "expected exactly 2 instruction-derived Step events (dedup same-line instructions), got {}: {:?}",
        instruction_step_lines.len(),
        instruction_step_lines,
    );
    assert_eq!(instruction_step_lines[0], 5, "first instruction step should be on line 5");
    assert_eq!(instruction_step_lines[1], 6, "second instruction step should be on line 6");
}

#[test]
fn test_no_source_map_entries_still_works() {
    // With an empty source map, no steps are emitted but the trace still converts
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"no_map","module":{"address":"0x0","name":"unknown"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"u64"}],"is_native":false},"gas_left":1000}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":999,"instruction":"LdU64(1)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":1}}}}}}"#,
        r#"{"CloseFrame":{"frame_id":1,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":1}}}],"gas_left":998}}"#,
    ]
    .join("\n");

    let (trace_content, _, _) = run_converter(&trace, &SourceMapResolver::empty(), "test.move");
    assert!(!trace_content.is_empty());
}

// ============================================================================
// Full end-to-end realistic scenario combining everything
// ============================================================================

#[test]
fn test_full_defi_swap_scenario() {
    // Simulates a DeFi token swap: swap_exact_input(pool, coin_in, min_out)
    // This exercises: nested calls, struct values, vector values, refs, branches
    let source_map = SourceMapResolver::from_entries(vec![
        ("dex".to_string(), 0, "dex.move".to_string(), 10),
        ("dex".to_string(), 1, "dex.move".to_string(), 11),
        ("dex".to_string(), 2, "dex.move".to_string(), 12),
        ("dex".to_string(), 3, "dex.move".to_string(), 13),
        ("dex".to_string(), 4, "dex.move".to_string(), 14),
        ("dex".to_string(), 5, "dex.move".to_string(), 15),
        ("pool".to_string(), 0, "pool.move".to_string(), 20),
        ("pool".to_string(), 1, "pool.move".to_string(), 21),
        ("pool".to_string(), 2, "pool.move".to_string(), 22),
    ]);

    let trace = vec![
        r#"{"version":3}"#,
        // Entry: dex::swap_exact_input
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"swap_exact_input","module":{"address":"0x3","name":"dex"},"type_instantiation":["0x2::sui::SUI","0x3::usdc::USDC"],"parameters":[{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"0x3::dex::Pool"},"fields":[["field_0",{"type":"Struct","value":{"type_":{"name":"UID"},"fields":[["field_0",{"type":"Address","value":"0xPOOL_ID"}]]}}],["field_1",{"type":"U64","value":1000000}],["field_2",{"type":"U64","value":2000000}]]}}}},{"RuntimeValue":{"value":{"type":"U64","value":100}}},{"RuntimeValue":{"value":{"type":"U64","value":50}}}],"return_types":[],"locals_types":[{"type_":"0x3::dex::Pool"},{"type_":"u64"},{"type_":"u64"},{"type_":"u64"},{"type_":"bool"}],"is_native":false},"gas_left":500000}}"#,
        // Read input amount
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":499999,"instruction":"CopyLoc(1)"}}"#,
        r#"{"Effect":{"Read":{"location":{"Local":[1,1]},"root_value_read":{"RuntimeValue":{"value":{"type":"U64","value":100}}},"moved":false}}}"#,
        // Call pool::calculate_output
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":499998,"instruction":"Call"}}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":2,"function_name":"calculate_output","module":{"address":"0x3","name":"pool"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"u64"}],"is_native":false},"gas_left":499997}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":499996,"instruction":"Mul"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"U64","value":198}}}}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":499995,"instruction":"Div"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[2,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":198}}}}}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":499994,"instruction":"Ret"}}"#,
        r#"{"CloseFrame":{"frame_id":2,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":198}}}],"gas_left":499993}}"#,
        // Store output amount
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":499992,"instruction":"StLoc(3)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,3]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":198}}}}}}"#,
        // Check output >= min_out (198 >= 50 => true)
        r#"{"Instruction":{"type_parameters":[],"pc":3,"gas_left":499991,"instruction":"Ge"}}"#,
        r#"{"Effect":{"Push":{"RuntimeValue":{"value":{"type":"Bool","value":true}}}}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,4]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Bool","value":true}}}}}}"#,
        // Update pool reserves (mutable ref)
        r#"{"Instruction":{"type_parameters":[],"pc":4,"gas_left":499990,"instruction":"MutBorrowLoc(0)"}}"#,
        r#"{"Effect":{"Push":{"MutRef":{"location":{"Local":[1,0]},"snapshot":{"type":"Struct","value":{"type_":{"name":"0x3::dex::Pool"},"fields":[["field_0",{"type":"Struct","value":{"type_":{"name":"UID"},"fields":[["field_0",{"type":"Address","value":"0xPOOL_ID"}]]}}],["field_1",{"type":"U64","value":1000000}],["field_2",{"type":"U64","value":2000000}]]}}}}}}"#,
        // Write updated pool reserves
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"0x3::dex::Pool"},"fields":[["field_0",{"type":"Struct","value":{"type_":{"name":"UID"},"fields":[["field_0",{"type":"Address","value":"0xPOOL_ID"}]]}}],["field_1",{"type":"U64","value":1000100}],["field_2",{"type":"U64","value":1999802}]]}}}}}}}"#,
        // Return output coin
        r#"{"Instruction":{"type_parameters":[],"pc":5,"gas_left":499989,"instruction":"Ret"}}"#,
        r#"{"CloseFrame":{"frame_id":1,"return_":[{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"0x2::coin::Coin"},"fields":[["field_0",{"type":"Struct","value":{"type_":{"name":"UID"},"fields":[["field_0",{"type":"Address","value":"0xCOIN_OUT"}]]}}],["field_1",{"type":"Struct","value":{"type_":{"name":"Balance"},"fields":[["field_0",{"type":"U64","value":198}]]}}]]}}}}],"gas_left":499988}}"#,
    ]
    .join("\n");

    let (trace_content, metadata, paths) = run_converter(&trace, &source_map, "dex.move");
    assert!(!trace_content.is_empty(), "trace.json should have content");
    assert!(metadata.get("program").is_some(), "metadata should have program");
    // Paths should be valid JSON
    assert!(paths.is_object() || paths.is_array(), "paths should be structured JSON");

    let (open, close, instr, effect) = count_events(&trace);
    assert_eq!(open, 2, "dex::swap + pool::calculate_output");
    assert_eq!(close, 2);
    assert!(instr >= 8, "many instructions in swap scenario");
    assert!(effect >= 8, "many effects in swap scenario");

    // Parse trace.json and verify the full DeFi scenario produces correct event structure.
    let events = parse_trace_events(&trace_content);

    // Verify Call/Return balance: toplevel + swap_exact_input + calculate_output = 3 Calls,
    // calculate_output + swap_exact_input + toplevel = 3 Returns
    let (calls, returns) = count_call_return(&events);
    assert_eq!(calls, 3, "expected 3 Call events (toplevel + swap + calculate), got {calls}");
    assert_eq!(returns, 3, "expected 3 Return events (swap + calculate + toplevel), got {returns}");

    // Verify Step events include lines from both dex.move (10-15) and pool.move (20-22)
    let step_lines = extract_step_lines(&events);
    let dex_lines: Vec<i64> = step_lines
        .iter()
        .copied()
        .filter(|&line| (10..=15).contains(&line))
        .collect();
    let pool_lines: Vec<i64> = step_lines
        .iter()
        .copied()
        .filter(|&line| (20..=22).contains(&line))
        .collect();
    assert!(
        !dex_lines.is_empty(),
        "expected steps on dex.move source lines (10-15), got: {:?}",
        step_lines
    );
    assert!(
        !pool_lines.is_empty(),
        "expected steps on pool.move source lines (20-22), got: {:?}",
        step_lines
    );

    // Verify Value events were produced for the write effects
    let value_count = events
        .iter()
        .filter(|e| matches!(e, TraceLowLevelEvent::Value(_)))
        .count();
    assert!(
        value_count >= 3,
        "expected at least 3 Value events from DeFi scenario write effects, got {value_count}"
    );

    // Verify Function events were emitted for named functions
    let function_names: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Function(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        function_names.iter().any(|n| n.contains("swap_exact_input")),
        "expected Function event for 'swap_exact_input', got: {:?}",
        function_names
    );
    assert!(
        function_names.iter().any(|n| n.contains("calculate_output")),
        "expected Function event for 'calculate_output', got: {:?}",
        function_names
    );
}

// ============================================================================
// M3 deliverable: Struct field conversion verification
// ============================================================================

#[test]
fn test_struct_fields_correctly_converted_point_rectangle() {
    // Verify that struct fields (Point.x, Point.y, Rectangle dimensions) are
    // correctly converted through the converter into ValueRecord::String with
    // the expected field_0/field_1/... display format.
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"create_shapes","module":{"address":"0x1","name":"geometry"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"Point"},{"type_":"Rectangle"}],"is_native":false},"gas_left":10000}}"#,
        // Create Point { x: 42, y: 99 }
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":9999,"instruction":"Pack(Point)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"0x1::geometry::Point"},"fields":[["field_0",{"type":"U64","value":42}],["field_1",{"type":"U64","value":99}]]}}}}}}}"#,
        // Create Rectangle { origin: Point { x: 10, y: 20 }, width: 100, height: 200 }
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":9998,"instruction":"Pack(Rectangle)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,1]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"0x1::geometry::Rectangle"},"fields":[["field_0",{"type":"Struct","value":{"type_":{"name":"0x1::geometry::Point"},"fields":[["field_0",{"type":"U64","value":10}],["field_1",{"type":"U64","value":20}]]}}],["field_1",{"type":"U64","value":100}],["field_2",{"type":"U64","value":200}]]}}}}}}}"#,
        r#"{"CloseFrame":{"frame_id":1,"return_":[{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"0x1::geometry::Rectangle"},"fields":[["field_0",{"type":"Struct","value":{"type_":{"name":"0x1::geometry::Point"},"fields":[["field_0",{"type":"U64","value":10}],["field_1",{"type":"U64","value":20}]]}}],["field_1",{"type":"U64","value":100}],["field_2",{"type":"U64","value":200}]]}}}}],"gas_left":9990}}"#,
    ]
    .join("\n");

    let result = run_converter_simple(&trace);
    let events = parse_trace_events(&result);

    // Extract all Value events (from Write effects).
    let value_events: Vec<&codetracer_trace_types::FullValueRecord> = events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Value(val) => Some(val),
            _ => None,
        })
        .collect();

    assert!(
        value_events.len() >= 2,
        "expected at least 2 Value events (Point + Rectangle), got {}",
        value_events.len()
    );

    // Find the Point value: should contain "0x1::geometry::Point { field_0: 42, field_1: 99 }"
    let point_value = value_events
        .iter()
        .find(|v| match &v.value {
            codetracer_trace_types::ValueRecord::String { text, .. } => {
                text.contains("Point") && text.contains("42") && text.contains("99")
            }
            _ => false,
        })
        .expect("should have a Value event for Point struct");

    match &point_value.value {
        codetracer_trace_types::ValueRecord::String { text, .. } => {
            assert!(
                text.contains("0x1::geometry::Point"),
                "Point value should include type name, got: {text}"
            );
            assert!(
                text.contains("field_0: 42"),
                "Point.x (field_0) should be 42, got: {text}"
            );
            assert!(
                text.contains("field_1: 99"),
                "Point.y (field_1) should be 99, got: {text}"
            );
        }
        other => panic!("expected String ValueRecord for Point, got: {:?}", other),
    }

    // Find the Rectangle value: should contain nested Point and dimensions.
    let rect_value = value_events
        .iter()
        .find(|v| match &v.value {
            codetracer_trace_types::ValueRecord::String { text, .. } => {
                text.contains("Rectangle") && text.contains("100") && text.contains("200")
            }
            _ => false,
        })
        .expect("should have a Value event for Rectangle struct");

    match &rect_value.value {
        codetracer_trace_types::ValueRecord::String { text, .. } => {
            assert!(
                text.contains("0x1::geometry::Rectangle"),
                "Rectangle value should include type name, got: {text}"
            );
            // field_0 is the nested Point struct, rendered inline
            assert!(
                text.contains("field_0: 0x1::geometry::Point"),
                "Rectangle.origin (field_0) should be a nested Point, got: {text}"
            );
            // field_1 is width=100, field_2 is height=200
            assert!(
                text.contains("field_1: 100"),
                "Rectangle.width (field_1) should be 100, got: {text}"
            );
            assert!(
                text.contains("field_2: 200"),
                "Rectangle.height (field_2) should be 200, got: {text}"
            );
        }
        other => panic!(
            "expected String ValueRecord for Rectangle, got: {:?}",
            other
        ),
    }

    // Also verify the return value from CloseFrame carries the Rectangle struct.
    let return_values: Vec<&codetracer_trace_types::ReturnRecord> = events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Return(ret) => Some(ret),
            _ => None,
        })
        .collect();

    // Last return is from the main frame, should be the Rectangle.
    let main_return = return_values
        .iter()
        .find(|r| match &r.return_value {
            codetracer_trace_types::ValueRecord::String { text, .. } => {
                text.contains("Rectangle")
            }
            _ => false,
        })
        .expect("should have a return value containing Rectangle");

    match &main_return.return_value {
        codetracer_trace_types::ValueRecord::String { text, .. } => {
            assert!(
                text.contains("100") && text.contains("200"),
                "Rectangle return should contain width=100 and height=200, got: {text}"
            );
        }
        other => panic!(
            "expected String return value for Rectangle, got: {:?}",
            other
        ),
    }
}

// ============================================================================
// M3 deliverable: Vector operations produce correct element values
// ============================================================================

#[test]
fn test_vector_operations_produce_correct_element_values() {
    // Verify that vector operations (create, push_back, pop_back) produce
    // Value events with the correct element content in the converted trace.
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"vec_values","module":{"address":"0x0","name":"vec_test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"vector<u64>"}],"is_native":false},"gas_left":10000}}"#,
        // Create empty vector
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":9999,"instruction":"VecPack(0)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Vector","elements":[]}}}}}}"#,
        // push_back(100)
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":9998,"instruction":"VecPushBack"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Vector","elements":[{"type":"U64","value":100}]}}}}}}"#,
        // push_back(200)
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":9997,"instruction":"VecPushBack"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Vector","elements":[{"type":"U64","value":100},{"type":"U64","value":200}]}}}}}}"#,
        // push_back(300)
        r#"{"Instruction":{"type_parameters":[],"pc":3,"gas_left":9996,"instruction":"VecPushBack"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Vector","elements":[{"type":"U64","value":100},{"type":"U64","value":200},{"type":"U64","value":300}]}}}}}}"#,
        // pop_back => removes 300, vector becomes [100, 200]
        r#"{"Instruction":{"type_parameters":[],"pc":4,"gas_left":9995,"instruction":"VecPopBack"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"Vector","elements":[{"type":"U64","value":100},{"type":"U64","value":200}]}}}}}}"#,
        r#"{"CloseFrame":{"frame_id":1,"return_":[{"RuntimeValue":{"value":{"type":"Vector","elements":[{"type":"U64","value":100},{"type":"U64","value":200}]}}}],"gas_left":9990}}"#,
    ]
    .join("\n");

    let result = run_converter_simple(&trace);
    let events = parse_trace_events(&result);

    // Extract all Value events and their string representations.
    let value_texts: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Value(val) => match &val.value {
                codetracer_trace_types::ValueRecord::String { text, .. } => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();

    assert!(
        value_texts.len() >= 5,
        "expected at least 5 Value events (empty + 3 pushes + 1 pop), got {}",
        value_texts.len()
    );

    // Verify the empty vector: "[]"
    assert!(
        value_texts.iter().any(|t| t == "[]"),
        "should have an empty vector '[]', got values: {:?}",
        value_texts
    );

    // Verify vector after first push: "[100]"
    assert!(
        value_texts.iter().any(|t| t == "[100]"),
        "should have '[100]' after first push_back, got values: {:?}",
        value_texts
    );

    // Verify vector after second push: "[100, 200]"
    assert!(
        value_texts.iter().any(|t| t == "[100, 200]"),
        "should have '[100, 200]' after second push_back, got values: {:?}",
        value_texts
    );

    // Verify vector after third push: "[100, 200, 300]"
    assert!(
        value_texts.iter().any(|t| t == "[100, 200, 300]"),
        "should have '[100, 200, 300]' after third push_back, got values: {:?}",
        value_texts
    );

    // Verify vector after pop: back to "[100, 200]"
    // Count how many times "[100, 200]" appears — should be at least 2
    // (once after second push, once after pop).
    let count_100_200 = value_texts.iter().filter(|t| t.as_str() == "[100, 200]").count();
    assert!(
        count_100_200 >= 2,
        "expected '[100, 200]' at least twice (after push and after pop), found {} times in: {:?}",
        count_100_200,
        value_texts
    );

    // Verify the return value is also the final vector state.
    let return_records: Vec<&codetracer_trace_types::ReturnRecord> = events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Return(ret) => Some(ret),
            _ => None,
        })
        .collect();

    let vec_return = return_records
        .iter()
        .find(|r| match &r.return_value {
            codetracer_trace_types::ValueRecord::String { text, .. } => text.contains("100"),
            _ => false,
        })
        .expect("should have a return value for the vector");

    match &vec_return.return_value {
        codetracer_trace_types::ValueRecord::String { text, .. } => {
            assert_eq!(
                text, "[100, 200]",
                "return value should be the final vector [100, 200], got: {text}"
            );
        }
        other => panic!(
            "expected String return value for vector, got: {:?}",
            other
        ),
    }
}

// ============================================================================
// M3 deliverable: Generic function instantiation produces correct type-specific values
// ============================================================================

#[test]
fn test_generic_function_instantiation_type_specific_values() {
    // Verify that a generic function instantiated with a specific type
    // (e.g., transfer<Coin<SUI>>) correctly passes type-specific values
    // through the converter, and that the function name and return values
    // reflect the concrete types.
    let trace = vec![
        r#"{"version":3}"#,
        // Generic function: identity<u64> — takes a u64 and returns it
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"test_generics","module":{"address":"0x1","name":"generic_mod"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"u64"},{"type_":"bool"}],"is_native":false},"gas_left":20000}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":19999,"instruction":"LdU64(42)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":42}}}}}}"#,
        // Call identity<u64>(42)
        r#"{"Instruction":{"type_parameters":[],"pc":1,"gas_left":19998,"instruction":"Call"}}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":2,"function_name":"identity","module":{"address":"0x1","name":"generic_mod"},"type_instantiation":["u64"],"parameters":[{"RuntimeValue":{"value":{"type":"U64","value":42}}}],"return_types":[],"locals_types":[{"type_":"u64"}],"is_native":false},"gas_left":19997}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":19996,"instruction":"MoveLoc(0)"}}"#,
        r#"{"Effect":{"Read":{"location":{"Local":[2,0]},"root_value_read":{"RuntimeValue":{"value":{"type":"U64","value":42}}},"moved":false}}}"#,
        r#"{"CloseFrame":{"frame_id":2,"return_":[{"RuntimeValue":{"value":{"type":"U64","value":42}}}],"gas_left":19995}}"#,
        // Call identity<bool>(true)
        r#"{"Instruction":{"type_parameters":[],"pc":2,"gas_left":19994,"instruction":"Call"}}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":3,"function_name":"identity","module":{"address":"0x1","name":"generic_mod"},"type_instantiation":["bool"],"parameters":[{"RuntimeValue":{"value":{"type":"Bool","value":true}}}],"return_types":[],"locals_types":[{"type_":"bool"}],"is_native":false},"gas_left":19993}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":19992,"instruction":"MoveLoc(0)"}}"#,
        r#"{"Effect":{"Read":{"location":{"Local":[3,0]},"root_value_read":{"RuntimeValue":{"value":{"type":"Bool","value":true}}},"moved":false}}}"#,
        r#"{"CloseFrame":{"frame_id":3,"return_":[{"RuntimeValue":{"value":{"type":"Bool","value":true}}}],"gas_left":19991}}"#,
        // Call wrap<Coin<SUI>> with a struct value
        r#"{"Instruction":{"type_parameters":[],"pc":3,"gas_left":19990,"instruction":"Call"}}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":4,"function_name":"wrap","module":{"address":"0x1","name":"generic_mod"},"type_instantiation":["0x2::coin::Coin<0x2::sui::SUI>"],"parameters":[{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"0x2::coin::Coin"},"fields":[["field_0",{"type":"U64","value":1000}]]}}}}],"return_types":[],"locals_types":[{"type_":"0x2::coin::Coin"}],"is_native":false},"gas_left":19989}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":19988,"instruction":"MoveLoc(0)"}}"#,
        r#"{"Effect":{"Read":{"location":{"Local":[4,0]},"root_value_read":{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"0x2::coin::Coin"},"fields":[["field_0",{"type":"U64","value":1000}]]}}}},"moved":false}}}"#,
        r#"{"CloseFrame":{"frame_id":4,"return_":[{"RuntimeValue":{"value":{"type":"Struct","value":{"type_":{"name":"0x1::generic_mod::Wrapper"},"fields":[["field_0",{"type":"Struct","value":{"type_":{"name":"0x2::coin::Coin"},"fields":[["field_0",{"type":"U64","value":1000}]]}}]]}}}}],"gas_left":19987}}"#,
        r#"{"CloseFrame":{"frame_id":1,"gas_left":19980}}"#,
    ]
    .join("\n");

    let result = run_converter_simple(&trace);
    let events = parse_trace_events(&result);

    // Verify we got Call events for all functions.
    let mut function_names_map: std::collections::HashMap<usize, String> =
        std::collections::HashMap::new();
    let mut next_fn_id = 0usize;
    for event in &events {
        if let TraceLowLevelEvent::Function(func) = event {
            function_names_map.insert(next_fn_id, func.name.clone());
            next_fn_id += 1;
        }
    }

    let call_fn_names: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Call(call) => {
                function_names_map.get(&call.function_id.0).cloned()
            }
            _ => None,
        })
        .collect();

    // Expect: toplevel, test_generics, identity (u64), identity (bool), wrap (Coin<SUI>)
    assert_eq!(
        call_fn_names.len(),
        5,
        "expected 5 Call events, got: {:?}",
        call_fn_names
    );
    assert_eq!(call_fn_names[0], "<toplevel>");
    assert_eq!(call_fn_names[1], "test_generics");
    assert_eq!(call_fn_names[2], "identity", "first generic call should be identity");
    assert_eq!(call_fn_names[3], "identity", "second generic call should also be identity");
    assert_eq!(call_fn_names[4], "wrap", "third generic call should be wrap");

    // Verify return values carry the correct type-specific data.
    let return_values: Vec<&codetracer_trace_types::ReturnRecord> = events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Return(ret) => Some(ret),
            _ => None,
        })
        .collect();

    // We have 4 CloseFrame events + 1 toplevel close => 5 Return events.
    assert_eq!(
        return_values.len(),
        5,
        "expected 5 Return events, got {}",
        return_values.len()
    );

    // Return from identity<u64>: should be Int(42)
    match &return_values[0].return_value {
        codetracer_trace_types::ValueRecord::Int { i, .. } => {
            assert_eq!(*i, 42, "identity<u64> should return 42, got {i}");
        }
        other => panic!(
            "expected Int return from identity<u64>, got: {:?}",
            other
        ),
    }

    // Return from identity<bool>: should be Bool(true)
    match &return_values[1].return_value {
        codetracer_trace_types::ValueRecord::Bool { b, .. } => {
            assert!(*b, "identity<bool> should return true");
        }
        other => panic!(
            "expected Bool return from identity<bool>, got: {:?}",
            other
        ),
    }

    // Return from wrap<Coin<SUI>>: should be a Wrapper struct containing a Coin struct.
    match &return_values[2].return_value {
        codetracer_trace_types::ValueRecord::String { text, .. } => {
            assert!(
                text.contains("Wrapper"),
                "wrap return should contain 'Wrapper', got: {text}"
            );
            assert!(
                text.contains("Coin"),
                "wrap return should contain nested 'Coin', got: {text}"
            );
            assert!(
                text.contains("1000"),
                "wrap return should contain coin value 1000, got: {text}"
            );
        }
        other => panic!(
            "expected String return from wrap<Coin<SUI>>, got: {:?}",
            other
        ),
    }

    // Verify type_instantiation was correctly parsed for all generic frames.
    // Parse the raw NDJSON to confirm type parameters.
    let mut lines = trace.lines();
    lines.next(); // version
    let mut type_instantiations: Vec<Vec<serde_json::Value>> = Vec::new();
    for line in lines {
        if let Ok(event) = serde_json::from_str::<TraceEvent>(line) {
            if let TraceEvent::OpenFrame { frame, .. } = event {
                if !frame.type_instantiation.is_empty() {
                    type_instantiations.push(frame.type_instantiation.clone());
                }
            }
        }
    }

    assert_eq!(
        type_instantiations.len(),
        3,
        "expected 3 frames with type_instantiation, got {}",
        type_instantiations.len()
    );
    assert_eq!(type_instantiations[0], vec![serde_json::Value::String("u64".to_string())], "identity<u64>");
    assert_eq!(type_instantiations[1], vec![serde_json::Value::String("bool".to_string())], "identity<bool>");
    assert_eq!(
        type_instantiations[2],
        vec![serde_json::Value::String("0x2::coin::Coin<0x2::sui::SUI>".to_string())],
        "wrap<Coin<SUI>>"
    );
}
