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
/// returning the parsed trace.bin content, metadata, and paths as JSON values.
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
        std::fs::read_to_string(out_dir.join("trace.bin")).expect("read trace.bin");
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

/// Run convert_trace and verify it succeeds, returning trace.bin content string.
fn run_converter_simple(ndjson: &str) -> String {
    let (trace, _, _) = run_converter(ndjson, &SourceMapResolver::empty(), "test.move");
    trace
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
            TraceEvent::Effect { .. } => effect += 1,
            TraceEvent::External { .. } => {}
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
    // serde_json does not support u128 deserialization without the
    // "arbitrary_precision" feature. This test documents that limitation:
    // U128 values in real traces would need a custom deserializer or
    // string-encoded representation (similar to U256).
    let result: Result<SerializableMoveValue, _> =
        serde_json::from_str(r#"{"type":"U128","value":42}"#);
    // This is expected to fail with standard serde_json.
    // When arbitrary_precision is enabled or a custom deser is added,
    // this test should be updated to assert success.
    assert!(
        result.is_err(),
        "U128 deserialization is not yet supported by serde_json without arbitrary_precision"
    );
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
        r#"{"type":"Struct","fields":[{"type":"U64","value":100},{"type":"Bool","value":true},{"type":"Address","value":"0xCAFE"}],"type_":"0x2::coin::Coin"}"#,
    )
    .unwrap();
    match v {
        SerializableMoveValue::Struct { fields, type_ } => {
            assert_eq!(type_, "0x2::coin::Coin");
            assert_eq!(fields.len(), 3);
            match &fields[0] {
                SerializableMoveValue::U64 { value } => assert_eq!(*value, 100),
                _ => panic!("expected U64 in field 0"),
            }
            match &fields[1] {
                SerializableMoveValue::Bool { value } => assert!(*value),
                _ => panic!("expected Bool in field 1"),
            }
            match &fields[2] {
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
        r#"{"type":"Vector","elements":[{"type":"Struct","fields":[{"type":"U64","value":10}],"type_":"Item"},{"type":"Struct","fields":[{"type":"U64","value":20}],"type_":"Item"}]}"#,
    )
    .unwrap();
    match v {
        SerializableMoveValue::Vector { elements } => {
            assert_eq!(elements.len(), 2);
            for elem in &elements {
                match elem {
                    SerializableMoveValue::Struct { type_, .. } => {
                        assert_eq!(type_, "Item");
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
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"all_types","module":{"address":"0x0","name":"types_test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":["u8","u16","u32","u64","u128","u256","bool","address"],"is_native":false},"gas_left":1000000}"#,
        // U8
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U8","value":42}}}}"#,
        // U16
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":1},"value":{"type":"RuntimeValue","value":{"type":"U16","value":1000}}}}"#,
        // U32
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":2},"value":{"type":"RuntimeValue","value":{"type":"U32","value":100000}}}}"#,
        // U64
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":3},"value":{"type":"RuntimeValue","value":{"type":"U64","value":9999999}}}}"#,
        // Note: U128 skipped here because serde_json does not support u128 deserialization.
        // U256
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":5},"value":{"type":"RuntimeValue","value":{"type":"U256","value":"115792089237316195423570985008687907853269984665640564039457584007913129639935"}}}}"#,
        // Bool
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":6},"value":{"type":"RuntimeValue","value":{"type":"Bool","value":true}}}}"#,
        // Address
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":7},"value":{"type":"RuntimeValue","value":{"type":"Address","value":"0xDEADBEEF"}}}}"#,
        // Struct via Push
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"Struct","fields":[{"type":"U64","value":100},{"type":"Bool","value":false}],"type_":"MyStruct"}}}}"#,
        // Vector via Push
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"Vector","elements":[{"type":"U8","value":1},{"type":"U8","value":2}]}}}}"#,
        // Variant via Push
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"Variant","tag":1,"fields":[{"type":"U64","value":99}],"type_":"Option"}}}}"#,
        r#"{"type":"CloseFrame","frame_id":1,"gas_left":999000}"#,
    ]
    .join("\n");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty(), "trace.bin should not be empty");
}

// ============================================================================
// 2. Call trace patterns
// ============================================================================

#[test]
fn test_simple_function_call() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"simple_fn","module":{"address":"0x1","name":"my_module"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":["u64"],"is_native":false},"gas_left":1000}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":999,"instruction":"LdU64(5)"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"U64","value":5}}}}"#,
        r#"{"type":"CloseFrame","frame_id":1,"return_":[{"type":"U64","value":5}],"gas_left":998}"#,
    ]
    .join("\n");

    let (open, close, instr, effect) = count_events(&trace);
    assert_eq!(open, 1);
    assert_eq!(close, 1);
    assert_eq!(instr, 1);
    assert_eq!(effect, 1);

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
}

#[test]
fn test_nested_calls_a_calls_b_calls_c() {
    let trace = vec![
        r#"{"version":3}"#,
        // A opens
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"func_a","module":{"address":"0x1","name":"mod_a"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":10000}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":9999,"instruction":"Call"}"#,
        // B opens
        r#"{"type":"OpenFrame","frame":{"frame_id":2,"function_name":"func_b","module":{"address":"0x1","name":"mod_b"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":9998}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":9997,"instruction":"Call"}"#,
        // C opens
        r#"{"type":"OpenFrame","frame":{"frame_id":3,"function_name":"func_c","module":{"address":"0x1","name":"mod_c"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":9996}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":9995,"instruction":"LdU64(99)"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"U64","value":99}}}}"#,
        // C closes
        r#"{"type":"CloseFrame","frame_id":3,"return_":[{"type":"U64","value":99}],"gas_left":9994}"#,
        // B closes
        r#"{"type":"CloseFrame","frame_id":2,"return_":[{"type":"U64","value":99}],"gas_left":9993}"#,
        // A closes
        r#"{"type":"CloseFrame","frame_id":1,"return_":[{"type":"U64","value":99}],"gas_left":9992}"#,
    ]
    .join("\n");

    let (open, close, _, _) = count_events(&trace);
    assert_eq!(open, 3, "3 nested OpenFrame events");
    assert_eq!(close, 3, "3 nested CloseFrame events");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
}

#[test]
fn test_generic_function_instantiation() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"transfer","module":{"address":"0x2","name":"transfer"},"type_instantiation":["0x2::coin::Coin<0x2::sui::SUI>"],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":5000}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":4999,"instruction":"MoveLoc(0)"}"#,
        r#"{"type":"CloseFrame","frame_id":1,"gas_left":4998}"#,
    ]
    .join("\n");

    // Verify parsing of type_instantiation
    let mut lines = trace.lines();
    lines.next(); // skip version
    let event: TraceEvent = serde_json::from_str(lines.next().unwrap()).unwrap();
    match event {
        TraceEvent::OpenFrame { frame, .. } => {
            assert_eq!(frame.type_instantiation.len(), 1);
            assert!(frame.type_instantiation[0].contains("Coin"));
        }
        _ => panic!("expected OpenFrame"),
    }

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
}

#[test]
fn test_entry_function_with_parameters() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"entry_transfer","module":{"address":"0x2","name":"pay"},"type_instantiation":[],"parameters":[{"type":"RuntimeValue","value":{"type":"Address","value":"0xABCD"}},{"type":"RuntimeValue","value":{"type":"U64","value":1000}}],"return_types":[],"locals_types":["address","u64"],"is_native":false},"gas_left":10000}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":9999,"instruction":"CopyLoc(0)"}"#,
        r#"{"type":"Effect","effect":{"type":"Read","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"Address","value":"0xABCD"}}}}"#,
        r#"{"type":"CloseFrame","frame_id":1,"gas_left":9990}"#,
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
}

#[test]
fn test_module_crossing_calls() {
    let trace = vec![
        r#"{"version":3}"#,
        // coin::transfer calls balance::withdraw
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"transfer","module":{"address":"0x2","name":"coin"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":10000}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":9999,"instruction":"Call"}"#,
        // Cross-module call into balance
        r#"{"type":"OpenFrame","frame":{"frame_id":2,"function_name":"withdraw","module":{"address":"0x2","name":"balance"},"type_instantiation":["0x2::sui::SUI"],"parameters":[],"return_types":[],"locals_types":["u64"],"is_native":false},"gas_left":9998}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":9997,"instruction":"LdU64(500)"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"U64","value":500}}}}"#,
        r#"{"type":"CloseFrame","frame_id":2,"return_":[{"type":"U64","value":500}],"gas_left":9996}"#,
        // Back in coin module, call transfer::transfer_internal
        r#"{"type":"Instruction","type_parameters":[],"pc":1,"gas_left":9995,"instruction":"Call"}"#,
        r#"{"type":"OpenFrame","frame":{"frame_id":3,"function_name":"transfer_internal","module":{"address":"0x2","name":"transfer"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":9994}"#,
        r#"{"type":"CloseFrame","frame_id":3,"gas_left":9993}"#,
        r#"{"type":"CloseFrame","frame_id":1,"gas_left":9992}"#,
    ]
    .join("\n");

    let (open, close, _, _) = count_events(&trace);
    assert_eq!(open, 3);
    assert_eq!(close, 3);

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
}

#[test]
fn test_recursive_function_calls() {
    let trace = vec![
        r#"{"version":3}"#,
        // factorial(3)
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"factorial","module":{"address":"0x1","name":"math"},"type_instantiation":[],"parameters":[{"type":"RuntimeValue","value":{"type":"U64","value":3}}],"return_types":[],"locals_types":["u64"],"is_native":false},"gas_left":10000}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":9999,"instruction":"CopyLoc(0)"}"#,
        r#"{"type":"Effect","effect":{"type":"Read","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":3}}}}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":1,"gas_left":9998,"instruction":"Call"}"#,
        // factorial(2) - recursive call
        r#"{"type":"OpenFrame","frame":{"frame_id":2,"function_name":"factorial","module":{"address":"0x1","name":"math"},"type_instantiation":[],"parameters":[{"type":"RuntimeValue","value":{"type":"U64","value":2}}],"return_types":[],"locals_types":["u64"],"is_native":false},"gas_left":9997}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":9996,"instruction":"CopyLoc(0)"}"#,
        r#"{"type":"Effect","effect":{"type":"Read","location":{"frame_id":2,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":2}}}}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":1,"gas_left":9995,"instruction":"Call"}"#,
        // factorial(1) - base case
        r#"{"type":"OpenFrame","frame":{"frame_id":3,"function_name":"factorial","module":{"address":"0x1","name":"math"},"type_instantiation":[],"parameters":[{"type":"RuntimeValue","value":{"type":"U64","value":1}}],"return_types":[],"locals_types":["u64"],"is_native":false},"gas_left":9994}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":9993,"instruction":"LdU64(1)"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"U64","value":1}}}}"#,
        r#"{"type":"CloseFrame","frame_id":3,"return_":[{"type":"U64","value":1}],"gas_left":9992}"#,
        // factorial(2) multiplies: 2 * 1 = 2
        r#"{"type":"Instruction","type_parameters":[],"pc":2,"gas_left":9991,"instruction":"Mul"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"U64","value":2}}}}"#,
        r#"{"type":"CloseFrame","frame_id":2,"return_":[{"type":"U64","value":2}],"gas_left":9990}"#,
        // factorial(3) multiplies: 3 * 2 = 6
        r#"{"type":"Instruction","type_parameters":[],"pc":2,"gas_left":9989,"instruction":"Mul"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"U64","value":6}}}}"#,
        r#"{"type":"CloseFrame","frame_id":1,"return_":[{"type":"U64","value":6}],"gas_left":9988}"#,
    ]
    .join("\n");

    let (open, close, _, _) = count_events(&trace);
    assert_eq!(open, 3, "3 recursive OpenFrame events");
    assert_eq!(close, 3, "3 recursive CloseFrame events");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
}

// ============================================================================
// 3. Variable tracking via Effects
// ============================================================================

#[test]
fn test_effect_push_pop() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"push_pop_test","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":999,"instruction":"LdU64(10)"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"U64","value":10}}}}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":1,"gas_left":998,"instruction":"LdU64(20)"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"U64","value":20}}}}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":2,"gas_left":997,"instruction":"Add"}"#,
        r#"{"type":"Effect","effect":{"type":"Pop","value":{"type":"RuntimeValue","value":{"type":"U64","value":20}}}}"#,
        r#"{"type":"Effect","effect":{"type":"Pop","value":{"type":"RuntimeValue","value":{"type":"U64","value":10}}}}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"U64","value":30}}}}"#,
        r#"{"type":"CloseFrame","frame_id":1,"return_":[{"type":"U64","value":30}],"gas_left":996}"#,
    ]
    .join("\n");

    let (_, _, _, effect) = count_events(&trace);
    assert_eq!(effect, 5, "2 pushes + 2 pops + 1 push result");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
}

#[test]
fn test_effect_read_write_locals() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"rw_test","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":["u64","u64"],"is_native":false},"gas_left":1000}"#,
        // Write to local_0
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":999,"instruction":"LdU64(42)"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":42}}}}"#,
        // Read local_0
        r#"{"type":"Instruction","type_parameters":[],"pc":1,"gas_left":998,"instruction":"CopyLoc(0)"}"#,
        r#"{"type":"Effect","effect":{"type":"Read","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":42}}}}"#,
        // Write to local_1
        r#"{"type":"Instruction","type_parameters":[],"pc":2,"gas_left":997,"instruction":"StLoc(1)"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":1},"value":{"type":"RuntimeValue","value":{"type":"U64","value":42}}}}"#,
        // Read local_1
        r#"{"type":"Instruction","type_parameters":[],"pc":3,"gas_left":996,"instruction":"MoveLoc(1)"}"#,
        r#"{"type":"Effect","effect":{"type":"Read","location":{"frame_id":1,"local_index":1},"value":{"type":"RuntimeValue","value":{"type":"U64","value":42}}}}"#,
        r#"{"type":"CloseFrame","frame_id":1,"return_":[{"type":"U64","value":42}],"gas_left":995}"#,
    ]
    .join("\n");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
}

#[test]
fn test_effect_mut_ref_tracking() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"mutref_test","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":["u64"],"is_native":false},"gas_left":1000}"#,
        // Write initial value
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":10}}}}"#,
        // MutRef borrow
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":999,"instruction":"MutBorrowLoc(0)"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"MutRef","location":{"frame_id":1,"local_index":0},"snapshot":{"type":"U64","value":10}}}}"#,
        // Write through ref (value changes)
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":20}}}}"#,
        r#"{"type":"CloseFrame","frame_id":1,"gas_left":998}"#,
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
        TraceEvent::Effect { effect } => match effect {
            codetracer_move_recorder::move_types::Effect::Push { value } => {
                match &value {
                    TraceValue::MutRef { location, snapshot } => {
                        assert_eq!(location.local_index, 0);
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
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"immref_test","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":["u64"],"is_native":false},"gas_left":1000}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":77}}}}"#,
        // ImmRef borrow
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":999,"instruction":"ImmBorrowLoc(0)"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"ImmRef","location":{"frame_id":1,"local_index":0},"snapshot":{"type":"U64","value":77}}}}"#,
        // Read through ref
        r#"{"type":"Effect","effect":{"type":"Read","location":{"frame_id":1,"local_index":0},"value":{"type":"ImmRef","location":{"frame_id":1,"local_index":0},"snapshot":{"type":"U64","value":77}}}}"#,
        r#"{"type":"CloseFrame","frame_id":1,"gas_left":998}"#,
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
        TraceEvent::Effect { effect } => match effect {
            codetracer_move_recorder::move_types::Effect::Push { value } => {
                match &value {
                    TraceValue::ImmRef { location, snapshot } => {
                        assert_eq!(location.local_index, 0);
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
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"linear_fn","module":{"address":"0x0","name":"linear"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":["u64","u64","u64","u64","u64"],"is_native":false},"gas_left":10000}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":9999,"instruction":"LdU64(1)"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":1}}}}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":1,"gas_left":9998,"instruction":"LdU64(2)"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":1},"value":{"type":"RuntimeValue","value":{"type":"U64","value":2}}}}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":2,"gas_left":9997,"instruction":"LdU64(3)"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":2},"value":{"type":"RuntimeValue","value":{"type":"U64","value":3}}}}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":3,"gas_left":9996,"instruction":"LdU64(4)"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":3},"value":{"type":"RuntimeValue","value":{"type":"U64","value":4}}}}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":4,"gas_left":9995,"instruction":"LdU64(5)"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":4},"value":{"type":"RuntimeValue","value":{"type":"U64","value":5}}}}"#,
        r#"{"type":"CloseFrame","frame_id":1,"gas_left":9990}"#,
    ]
    .join("\n");

    let (trace_content, metadata, _) = run_converter(&trace, &source_map, "linear.move");
    assert!(!trace_content.is_empty());
    assert!(metadata.get("program").is_some());
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
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"branch_fn","module":{"address":"0x0","name":"branch"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":["u64","u64"],"is_native":false},"gas_left":1000}"#,
        // Load x = 3
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":999,"instruction":"LdU64(3)"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":3}}}}"#,
        // Compare: x > 5 => false
        r#"{"type":"Instruction","type_parameters":[],"pc":1,"gas_left":998,"instruction":"Gt"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"Bool","value":false}}}}"#,
        // BrTrue (false, so fall through to else)
        r#"{"type":"Instruction","type_parameters":[],"pc":2,"gas_left":997,"instruction":"BrTrue(4)"}"#,
        r#"{"type":"Effect","effect":{"type":"Pop","value":{"type":"RuntimeValue","value":{"type":"Bool","value":false}}}}"#,
        // Else branch: a = 20 (jumps to pc=5)
        r#"{"type":"Instruction","type_parameters":[],"pc":5,"gas_left":996,"instruction":"LdU64(20)"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":1},"value":{"type":"RuntimeValue","value":{"type":"U64","value":20}}}}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":6,"gas_left":995,"instruction":"Ret"}"#,
        r#"{"type":"CloseFrame","frame_id":1,"return_":[{"type":"U64","value":20}],"gas_left":994}"#,
    ]
    .join("\n");

    let (trace_content, _, _) = run_converter(&trace, &source_map, "branch.move");
    assert!(!trace_content.is_empty());
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
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"loop_fn","module":{"address":"0x0","name":"loop_mod"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":["u64","u64"],"is_native":false},"gas_left":10000}"#,
        // i = 0
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":9999,"instruction":"LdU64(0)"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":0}}}}"#,
        // sum = 0
        r#"{"type":"Instruction","type_parameters":[],"pc":1,"gas_left":9998,"instruction":"LdU64(0)"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":1},"value":{"type":"RuntimeValue","value":{"type":"U64","value":0}}}}"#,
        // Iteration 1: i=0, check i<3
        r#"{"type":"Instruction","type_parameters":[],"pc":2,"gas_left":9997,"instruction":"Lt"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"Bool","value":true}}}}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":3,"gas_left":9996,"instruction":"Add"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":1},"value":{"type":"RuntimeValue","value":{"type":"U64","value":0}}}}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":4,"gas_left":9995,"instruction":"Add"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":1}}}}"#,
        // Iteration 2: i=1, check i<3
        r#"{"type":"Instruction","type_parameters":[],"pc":2,"gas_left":9994,"instruction":"Lt"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"Bool","value":true}}}}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":3,"gas_left":9993,"instruction":"Add"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":1},"value":{"type":"RuntimeValue","value":{"type":"U64","value":1}}}}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":4,"gas_left":9992,"instruction":"Add"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":2}}}}"#,
        // Iteration 3: i=2, check i<3
        r#"{"type":"Instruction","type_parameters":[],"pc":2,"gas_left":9991,"instruction":"Lt"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"Bool","value":true}}}}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":3,"gas_left":9990,"instruction":"Add"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":1},"value":{"type":"RuntimeValue","value":{"type":"U64","value":3}}}}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":4,"gas_left":9989,"instruction":"Add"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":3}}}}"#,
        // Exit: i=3, check i<3 => false
        r#"{"type":"Instruction","type_parameters":[],"pc":2,"gas_left":9988,"instruction":"Lt"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"Bool","value":false}}}}"#,
        // After loop
        r#"{"type":"Instruction","type_parameters":[],"pc":6,"gas_left":9987,"instruction":"Ret"}"#,
        r#"{"type":"CloseFrame","frame_id":1,"return_":[{"type":"U64","value":3}],"gas_left":9986}"#,
    ]
    .join("\n");

    let (trace_content, _, _) = run_converter(&trace, &source_map, "loop.move");
    assert!(!trace_content.is_empty());

    // Verify event counts: should have repeated pc=2 four times (3 true + 1 false)
    let (_, _, instr, _) = count_events(&trace);
    assert!(instr >= 10, "loop should produce many instruction events, got {instr}");
}

#[test]
fn test_execution_error_abort() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"will_abort","module":{"address":"0x0","name":"abort_mod"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":["u64"],"is_native":false},"gas_left":1000}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":999,"instruction":"LdU64(0)"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":0}}}}"#,
        // Abort instruction triggers ExecutionError
        r#"{"type":"Instruction","type_parameters":[],"pc":1,"gas_left":998,"instruction":"Abort"}"#,
        r#"{"type":"Effect","effect":{"type":"ExecutionError","error":"ABORT with code 42"}}"#,
        r#"{"type":"CloseFrame","frame_id":1,"gas_left":997}"#,
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
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"transfer","module":{"address":"0x2","name":"coin"},"type_instantiation":["0x2::sui::SUI"],"parameters":[{"type":"RuntimeValue","value":{"type":"Struct","fields":[{"type":"Struct","fields":[{"type":"Address","value":"0xOBJ1"}],"type_":"0x2::object::UID"},{"type":"Struct","fields":[{"type":"U64","value":1000}],"type_":"0x2::balance::Balance"}],"type_":"0x2::coin::Coin"}},{"type":"RuntimeValue","value":{"type":"Address","value":"0xRECIPIENT"}},{"type":"RuntimeValue","value":{"type":"U64","value":500}}],"return_types":[],"locals_types":["0x2::coin::Coin","address","u64","0x2::balance::Balance"],"is_native":false},"gas_left":100000}"#,
        // Read the coin struct
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":99999,"instruction":"CopyLoc(0)"}"#,
        r#"{"type":"Effect","effect":{"type":"Read","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"Struct","fields":[{"type":"Struct","fields":[{"type":"Address","value":"0xOBJ1"}],"type_":"0x2::object::UID"},{"type":"Struct","fields":[{"type":"U64","value":1000}],"type_":"0x2::balance::Balance"}],"type_":"0x2::coin::Coin"}}}}"#,
        // Call balance::split to extract amount
        r#"{"type":"Instruction","type_parameters":[],"pc":1,"gas_left":99998,"instruction":"Call"}"#,
        r#"{"type":"OpenFrame","frame":{"frame_id":2,"function_name":"split","module":{"address":"0x2","name":"balance"},"type_instantiation":["0x2::sui::SUI"],"parameters":[],"return_types":[],"locals_types":["u64"],"is_native":false},"gas_left":99997}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":99996,"instruction":"LdU64(500)"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":2,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":500}}}}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":1,"gas_left":99995,"instruction":"Ret"}"#,
        r#"{"type":"CloseFrame","frame_id":2,"return_":[{"type":"Struct","fields":[{"type":"U64","value":500}],"type_":"0x2::balance::Balance"}],"gas_left":99994}"#,
        // Store split balance
        r#"{"type":"Instruction","type_parameters":[],"pc":2,"gas_left":99993,"instruction":"StLoc(3)"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":3},"value":{"type":"RuntimeValue","value":{"type":"Struct","fields":[{"type":"U64","value":500}],"type_":"0x2::balance::Balance"}}}}"#,
        // Transfer to recipient
        r#"{"type":"Instruction","type_parameters":[],"pc":3,"gas_left":99992,"instruction":"Call"}"#,
        r#"{"type":"CloseFrame","frame_id":1,"gas_left":99991}"#,
    ]
    .join("\n");

    let (trace_content, metadata, _) = run_converter(&trace, &source_map, "coin.move");
    assert!(!trace_content.is_empty());
    assert!(metadata.get("program").is_some());

    let (open, close, _, _) = count_events(&trace);
    assert_eq!(open, 2, "outer transfer + inner split");
    assert_eq!(close, 2);
}

#[test]
fn test_scenario_object_creation() {
    // Simulates creating an object with a UID
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"create","module":{"address":"0x1","name":"nft"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":["0x2::object::UID","0x1::nft::NFT"],"is_native":false},"gas_left":50000}"#,
        // Create UID via object::new
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":49999,"instruction":"Call"}"#,
        r#"{"type":"OpenFrame","frame":{"frame_id":2,"function_name":"new","module":{"address":"0x2","name":"object"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":49998}"#,
        r#"{"type":"CloseFrame","frame_id":2,"return_":[{"type":"Struct","fields":[{"type":"Address","value":"0xUID_ADDR_123"}],"type_":"0x2::object::UID"}],"gas_left":49997}"#,
        // Store UID
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"Struct","fields":[{"type":"Address","value":"0xUID_ADDR_123"}],"type_":"0x2::object::UID"}}}}"#,
        // Pack NFT struct: NFT { id: uid, name_length: 5, value: 100 }
        r#"{"type":"Instruction","type_parameters":[],"pc":1,"gas_left":49996,"instruction":"Pack(NFT)"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"Struct","fields":[{"type":"Struct","fields":[{"type":"Address","value":"0xUID_ADDR_123"}],"type_":"0x2::object::UID"},{"type":"U64","value":5},{"type":"U64","value":100}],"type_":"0x1::nft::NFT"}}}}"#,
        // Store NFT
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":1},"value":{"type":"RuntimeValue","value":{"type":"Struct","fields":[{"type":"Struct","fields":[{"type":"Address","value":"0xUID_ADDR_123"}],"type_":"0x2::object::UID"},{"type":"U64","value":5},{"type":"U64","value":100}],"type_":"0x1::nft::NFT"}}}}"#,
        // Transfer the NFT
        r#"{"type":"Instruction","type_parameters":[],"pc":2,"gas_left":49995,"instruction":"Call"}"#,
        r#"{"type":"CloseFrame","frame_id":1,"gas_left":49990}"#,
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
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"vec_ops","module":{"address":"0x0","name":"vec_test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":["vector<u64>","u64"],"is_native":false},"gas_left":10000}"#,
        // Create empty vector
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":9999,"instruction":"VecPack(0)"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"Vector","elements":[]}}}}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"Vector","elements":[]}}}}"#,
        // push_back(10)
        r#"{"type":"Instruction","type_parameters":[],"pc":1,"gas_left":9998,"instruction":"VecPushBack"}"#,
        r#"{"type":"Effect","effect":{"type":"Pop","value":{"type":"RuntimeValue","value":{"type":"U64","value":10}}}}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"Vector","elements":[{"type":"U64","value":10}]}}}}"#,
        // push_back(20)
        r#"{"type":"Instruction","type_parameters":[],"pc":2,"gas_left":9997,"instruction":"VecPushBack"}"#,
        r#"{"type":"Effect","effect":{"type":"Pop","value":{"type":"RuntimeValue","value":{"type":"U64","value":20}}}}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"Vector","elements":[{"type":"U64","value":10},{"type":"U64","value":20}]}}}}"#,
        // push_back(30)
        r#"{"type":"Instruction","type_parameters":[],"pc":3,"gas_left":9996,"instruction":"VecPushBack"}"#,
        r#"{"type":"Effect","effect":{"type":"Pop","value":{"type":"RuntimeValue","value":{"type":"U64","value":30}}}}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"Vector","elements":[{"type":"U64","value":10},{"type":"U64","value":20},{"type":"U64","value":30}]}}}}"#,
        // pop_back => 30
        r#"{"type":"Instruction","type_parameters":[],"pc":4,"gas_left":9995,"instruction":"VecPopBack"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"U64","value":30}}}}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"Vector","elements":[{"type":"U64","value":10},{"type":"U64","value":20}]}}}}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":1},"value":{"type":"RuntimeValue","value":{"type":"U64","value":30}}}}"#,
        // vector::length => 2
        r#"{"type":"Instruction","type_parameters":[],"pc":5,"gas_left":9994,"instruction":"VecLen"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"U64","value":2}}}}"#,
        r#"{"type":"CloseFrame","frame_id":1,"return_":[{"type":"U64","value":2}],"gas_left":9993}"#,
    ]
    .join("\n");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
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
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"pay","module":{"address":"0x2","name":"pay"},"type_instantiation":[],"parameters":[{"type":"RuntimeValue","value":{"type":"U64","value":100}},{"type":"RuntimeValue","value":{"type":"U64","value":500}}],"return_types":[],"locals_types":["u64","u64"],"is_native":false},"gas_left":5000}"#,
        // Load balance = 100
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":4999,"instruction":"CopyLoc(0)"}"#,
        r#"{"type":"Effect","effect":{"type":"Read","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":100}}}}"#,
        // Load amount = 500
        r#"{"type":"Instruction","type_parameters":[],"pc":1,"gas_left":4998,"instruction":"CopyLoc(1)"}"#,
        r#"{"type":"Effect","effect":{"type":"Read","location":{"frame_id":1,"local_index":1},"value":{"type":"RuntimeValue","value":{"type":"U64","value":500}}}}"#,
        // Check balance >= amount => false, abort
        r#"{"type":"Instruction","type_parameters":[],"pc":2,"gas_left":4997,"instruction":"Abort"}"#,
        r#"{"type":"Effect","effect":{"type":"ExecutionError","error":"ABORT with code 1 (EInsufficientBalance)"}}"#,
        r#"{"type":"CloseFrame","frame_id":1,"gas_left":4996}"#,
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
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"void_fn","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000}"#,
        // No return values (void function)
        r#"{"type":"CloseFrame","frame_id":1,"gas_left":999}"#,
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
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"no_ret","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000}"#,
        r#"{"type":"CloseFrame","frame_id":1,"return_":null,"gas_left":999}"#,
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
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"multi_ret","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000}"#,
        r#"{"type":"CloseFrame","frame_id":1,"return_":[{"type":"U64","value":1},{"type":"Bool","value":true},{"type":"Address","value":"0xABC"}],"gas_left":999}"#,
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
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"main","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":10000}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":9999,"instruction":"Call"}"#,
        // Native function call
        r#"{"type":"OpenFrame","frame":{"frame_id":2,"function_name":"native_hash","module":{"address":"0x1","name":"hash"},"type_instantiation":[],"parameters":[{"type":"RuntimeValue","value":{"type":"Vector","elements":[{"type":"U8","value":1},{"type":"U8","value":2}]}}],"return_types":[],"locals_types":[],"is_native":true},"gas_left":9998}"#,
        r#"{"type":"CloseFrame","frame_id":2,"return_":[{"type":"Vector","elements":[{"type":"U8","value":100},{"type":"U8","value":200}]}],"gas_left":9997}"#,
        r#"{"type":"CloseFrame","frame_id":1,"gas_left":9996}"#,
    ]
    .join("\n");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
}

#[test]
fn test_data_load_effect() {
    let trace = vec![
        r#"{"version":3}"#,
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"load_test","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000}"#,
        r#"{"type":"Effect","effect":{"type":"DataLoad","address":"0xSOME_OBJ_ADDR"}}"#,
        r#"{"type":"CloseFrame","frame_id":1,"gas_left":999}"#,
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
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"ext_test","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000}"#,
        r#"{"type":"External","effect":{"kind":"transfer_object"}}"#,
        r#"{"type":"CloseFrame","frame_id":1,"gas_left":999}"#,
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
        r#"{"type":"Struct","fields":[{"type":"Struct","fields":[{"type":"Struct","fields":[{"type":"U64","value":42}],"type_":"Inner"}],"type_":"Middle"}],"type_":"Outer"}"#,
    )
    .unwrap();
    match v {
        SerializableMoveValue::Struct { fields, type_ } => {
            assert_eq!(type_, "Outer");
            match &fields[0] {
                SerializableMoveValue::Struct { fields, type_ } => {
                    assert_eq!(type_, "Middle");
                    match &fields[0] {
                        SerializableMoveValue::Struct { fields, type_ } => {
                            assert_eq!(type_, "Inner");
                            match &fields[0] {
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
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"deep_test","module":{"address":"0x0","name":"test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"Struct","fields":[{"type":"Struct","fields":[{"type":"Struct","fields":[{"type":"U64","value":42}],"type_":"Inner"}],"type_":"Middle"}],"type_":"Outer"}}}}"#,
        r#"{"type":"CloseFrame","frame_id":1,"return_":[{"type":"Struct","fields":[{"type":"Struct","fields":[{"type":"Struct","fields":[{"type":"U64","value":42}],"type_":"Inner"}],"type_":"Middle"}],"type_":"Outer"}],"gas_left":999}"#,
    ]
    .join("\n");

    let result = run_converter_simple(&trace);
    assert!(!result.is_empty());
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
        r#"{"type":"Struct","fields":[{"type":"U64","value":1}]}"#,
    )
    .unwrap();
    match v {
        SerializableMoveValue::Struct { type_, .. } => {
            assert!(type_.is_empty(), "type_ should default to empty string");
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
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"dedup_fn","module":{"address":"0x0","name":"dedup"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":999,"instruction":"LdU64(1)"}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":1,"gas_left":998,"instruction":"LdU64(2)"}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":2,"gas_left":997,"instruction":"Add"}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":3,"gas_left":996,"instruction":"Ret"}"#,
        r#"{"type":"CloseFrame","frame_id":1,"gas_left":995}"#,
    ]
    .join("\n");

    // This should succeed - the converter deduplicates steps on the same line
    let (trace_content, _, _) = run_converter(&trace, &source_map, "dedup.move");
    assert!(!trace_content.is_empty());

    // Parse trace.bin and verify deduplication: consecutive instructions on the
    // same line should produce only one Step event per line transition.
    // The converter emits an initial Step(line 1) from start(), then:
    // pc 0,1,2 all map to line 5 => one Step(line 5)
    // pc 3 maps to line 6 => one Step(line 6)
    // Total: 3 Step events (initial + 2 from instructions), NOT 5 (initial + 4 per-instruction)
    let events: Vec<TraceLowLevelEvent> =
        serde_json::from_str(&trace_content).expect("trace.bin should be valid JSON array");

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
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"no_map","module":{"address":"0x0","name":"unknown"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":["u64"],"is_native":false},"gas_left":1000}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":999,"instruction":"LdU64(1)"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":1}}}}"#,
        r#"{"type":"CloseFrame","frame_id":1,"return_":[{"type":"U64","value":1}],"gas_left":998}"#,
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
        r#"{"type":"OpenFrame","frame":{"frame_id":1,"function_name":"swap_exact_input","module":{"address":"0x3","name":"dex"},"type_instantiation":["0x2::sui::SUI","0x3::usdc::USDC"],"parameters":[{"type":"RuntimeValue","value":{"type":"Struct","fields":[{"type":"Struct","fields":[{"type":"Address","value":"0xPOOL_ID"}],"type_":"UID"},{"type":"U64","value":1000000},{"type":"U64","value":2000000}],"type_":"0x3::dex::Pool"}},{"type":"RuntimeValue","value":{"type":"U64","value":100}},{"type":"RuntimeValue","value":{"type":"U64","value":50}}],"return_types":[],"locals_types":["0x3::dex::Pool","u64","u64","u64","bool"],"is_native":false},"gas_left":500000}"#,
        // Read input amount
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":499999,"instruction":"CopyLoc(1)"}"#,
        r#"{"type":"Effect","effect":{"type":"Read","location":{"frame_id":1,"local_index":1},"value":{"type":"RuntimeValue","value":{"type":"U64","value":100}}}}"#,
        // Call pool::calculate_output
        r#"{"type":"Instruction","type_parameters":[],"pc":1,"gas_left":499998,"instruction":"Call"}"#,
        r#"{"type":"OpenFrame","frame":{"frame_id":2,"function_name":"calculate_output","module":{"address":"0x3","name":"pool"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":["u64"],"is_native":false},"gas_left":499997}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":0,"gas_left":499996,"instruction":"Mul"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"U64","value":198}}}}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":1,"gas_left":499995,"instruction":"Div"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":2,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"U64","value":198}}}}"#,
        r#"{"type":"Instruction","type_parameters":[],"pc":2,"gas_left":499994,"instruction":"Ret"}"#,
        r#"{"type":"CloseFrame","frame_id":2,"return_":[{"type":"U64","value":198}],"gas_left":499993}"#,
        // Store output amount
        r#"{"type":"Instruction","type_parameters":[],"pc":2,"gas_left":499992,"instruction":"StLoc(3)"}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":3},"value":{"type":"RuntimeValue","value":{"type":"U64","value":198}}}}"#,
        // Check output >= min_out (198 >= 50 => true)
        r#"{"type":"Instruction","type_parameters":[],"pc":3,"gas_left":499991,"instruction":"Ge"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"RuntimeValue","value":{"type":"Bool","value":true}}}}"#,
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":4},"value":{"type":"RuntimeValue","value":{"type":"Bool","value":true}}}}"#,
        // Update pool reserves (mutable ref)
        r#"{"type":"Instruction","type_parameters":[],"pc":4,"gas_left":499990,"instruction":"MutBorrowLoc(0)"}"#,
        r#"{"type":"Effect","effect":{"type":"Push","value":{"type":"MutRef","location":{"frame_id":1,"local_index":0},"snapshot":{"type":"Struct","fields":[{"type":"Struct","fields":[{"type":"Address","value":"0xPOOL_ID"}],"type_":"UID"},{"type":"U64","value":1000000},{"type":"U64","value":2000000}],"type_":"0x3::dex::Pool"}}}}"#,
        // Write updated pool reserves
        r#"{"type":"Effect","effect":{"type":"Write","location":{"frame_id":1,"local_index":0},"value":{"type":"RuntimeValue","value":{"type":"Struct","fields":[{"type":"Struct","fields":[{"type":"Address","value":"0xPOOL_ID"}],"type_":"UID"},{"type":"U64","value":1000100},{"type":"U64","value":1999802}],"type_":"0x3::dex::Pool"}}}}"#,
        // Return output coin
        r#"{"type":"Instruction","type_parameters":[],"pc":5,"gas_left":499989,"instruction":"Ret"}"#,
        r#"{"type":"CloseFrame","frame_id":1,"return_":[{"type":"Struct","fields":[{"type":"Struct","fields":[{"type":"Address","value":"0xCOIN_OUT"}],"type_":"UID"},{"type":"Struct","fields":[{"type":"U64","value":198}],"type_":"Balance"}],"type_":"0x2::coin::Coin"}],"gas_left":499988}"#,
    ]
    .join("\n");

    let (trace_content, metadata, paths) = run_converter(&trace, &source_map, "dex.move");
    assert!(!trace_content.is_empty(), "trace.bin should have content");
    assert!(metadata.get("program").is_some(), "metadata should have program");
    // Paths should be valid JSON
    assert!(paths.is_object() || paths.is_array(), "paths should be structured JSON");

    let (open, close, instr, effect) = count_events(&trace);
    assert_eq!(open, 2, "dex::swap + pool::calculate_output");
    assert_eq!(close, 2);
    assert!(instr >= 8, "many instructions in swap scenario");
    assert!(effect >= 8, "many effects in swap scenario");
}
