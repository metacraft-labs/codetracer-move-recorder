//! Converting an Aptos replay: the value trace `aptos move replay --trace`
//! writes, with the disassembly of every module it entered.
//!
//! The fixture `tests/fixtures/aptos-7881330395/` is a real Aptos mainnet
//! transaction — version 7,881,330,395, an APT transfer through
//! `0x1::aptos_account::transfer` — replayed with the CodeTracer build of
//! the Aptos CLI (metacraft-labs/aptos-core, branch
//! `codetracer/value-tracing`):
//!
//! * `trace.json.zst`: the trace (`--trace`), zstd-compressed;
//! * `0x…01/bytecode/<module>.mv.masm`: `aptos move disassemble` of each of
//!   the 16 framework modules the trace enters, as their bytecode was at
//!   version 7,881,330,394, the state the transaction ran on;
//! * `control-move-vm-trace.csv.zst`: what the STOCK Aptos CLI records for
//!   the same transaction (`MOVE_VM_TRACE`, one `function,pc` per line).
//!
//! Nothing is mocked: the files are what the tools wrote, read by the real
//! loader and converter.  Each assertion names a control that fails under
//! the defect it guards against (see `Verification-Harness-Traps.md`).

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use codetracer_trace_types::{EventLogKind, TraceLowLevelEvent, ValueRecord};
use codetracer_trace_writer_nim::NimTraceReaderHandle;
use codetracer_trace_writer_nim::non_streaming_trace_writer::NonStreamingTraceWriter;

use codetracer_move_recorder::aptos_adapter::{
    AptosEnrichedEntry, convert_aptos_trace, parse_move_vm_trace_csv,
};
use codetracer_move_recorder::aptos_replay::{self, convert_aptos_replay_dir};
use codetracer_move_recorder::converter::{self, ConverterOptions, MoveChain};
use codetracer_move_recorder::move_debug_info::{DebugInfo, parse_aptos_disassembly};

/// The Nim runtime behind the bundle writer and reader is process-global;
/// tests that touch it run one at a time.
static NIM_TEST_LOCK: Mutex<()> = Mutex::new(());

const FRAMEWORK: &str = "0x0000000000000000000000000000000000000000000000000000000000000001";

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/aptos-7881330395")
}

fn unzstd(path: &Path) -> Vec<u8> {
    zstd::decode_all(fs::File::open(path).unwrap()).unwrap()
}

fn trace() -> Vec<u8> {
    unzstd(&fixture().join("trace.json.zst"))
}

fn masm_path(module: &str) -> PathBuf {
    Path::new(FRAMEWORK)
        .join("bytecode")
        .join(format!("{module}.mv.masm"))
}

fn masm(module: &str) -> String {
    fs::read_to_string(fixture().join(masm_path(module))).unwrap()
}

fn debug_info() -> DebugInfo {
    let (info, rejected) = DebugInfo::from_aptos_replay_dir(&fixture()).unwrap();
    assert!(rejected.is_empty(), "{rejected:?}");
    info
}

/// The one `.ct` container a conversion wrote to `out`.
fn container(out: &Path) -> PathBuf {
    let cts: Vec<PathBuf> = fs::read_dir(out)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ct"))
        .collect();
    assert_eq!(cts.len(), 1, "{cts:?}");
    cts.into_iter().next().unwrap()
}

/// What a real bundle records: (path, line) per step and the number of
/// values attached to each step.
struct Bundle {
    steps: Vec<(PathBuf, u64)>,
    values_per_step: Vec<u64>,
    reader: NimTraceReaderHandle,
}

/// Read a bundle back.  Callers hold `NIM_TEST_LOCK`.
fn read_bundle(ct: &Path) -> Bundle {
    let reader = NimTraceReaderHandle::open(ct.to_str().unwrap()).expect("bundle opens");
    let paths: Vec<PathBuf> = (0..reader.path_count())
        .map(|id| PathBuf::from(reader.path(id).unwrap()))
        .collect();
    let n = reader.step_count();
    let (mut path_ids, mut lines, mut columns) = (
        vec![0; n as usize],
        vec![0; n as usize],
        vec![0; n as usize],
    );
    let read = if reader.has_column_aware_steps() {
        reader
            .step_locations_with_columns(0, n, &mut path_ids, &mut lines, &mut columns)
            .unwrap()
    } else {
        reader
            .step_locations(0, n, &mut path_ids, &mut lines)
            .unwrap()
    };
    assert_eq!(read, n);
    let steps = path_ids
        .iter()
        .zip(&lines)
        .map(|(&p, &l)| (paths[p as usize].clone(), l))
        .collect();
    let values_per_step = (0..n).map(|s| reader.step_value_count(s)).collect();
    Bundle {
        steps,
        values_per_step,
        reader,
    }
}

/// The 1-based line of the first instruction of function definition
/// `index` in a `.masm` text, found by reading the text rather than through
/// the loader under test: the first line after the function's header that
/// is neither a `local` declaration nor a comment.
fn first_instruction_line(masm: &str, index: u64) -> u64 {
    let marker = format!("// Function definition at index {index}");
    let lines: Vec<&str> = masm.lines().collect();
    let at = lines.iter().position(|l| l.trim() == marker).unwrap();
    let body = at + 2; // the header is the line after the marker
    let offset = lines[body..]
        .iter()
        .position(|l| {
            let t = l.trim();
            !t.starts_with("local ") && !t.starts_with("//")
        })
        .unwrap();
    (body + offset + 1) as u64
}

fn instruction_count(trace: &[u8]) -> usize {
    String::from_utf8_lossy(trace)
        .lines()
        .filter(|l| l.starts_with("{\"Instruction\""))
        .count()
}

/// Convert the fixture in memory and return the writer's events.
fn in_memory(
    trace: &[u8],
    debug_info: DebugInfo,
    options: ConverterOptions,
) -> Vec<TraceLowLevelEvent> {
    let mut writer = NonStreamingTraceWriter::new("aptos", &[]);
    converter::convert_trace_into_writer_with_debug_info(
        trace,
        Path::new("transaction.masm"),
        debug_info,
        &mut writer,
        options,
    )
    .expect("conversion succeeds");
    writer.events
}

fn events_tagged<'a>(events: &'a [TraceLowLevelEvent], tag: &str) -> Vec<&'a str> {
    events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Event(r) if r.metadata == tag => Some(r.content.as_str()),
            _ => None,
        })
        .collect()
}

fn function_names(events: &[TraceLowLevelEvent]) -> BTreeSet<String> {
    events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Function(f) => Some(f.name.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn every_entered_module_disassembly_loads() {
    let info = debug_info();
    let modules = aptos_replay::executed_modules(&trace()).unwrap();
    assert_eq!(modules.len(), 16, "{modules:?}");
    assert_eq!(info.module_count(), modules.len());
    for (address, module) in &modules {
        assert!(
            info.module(address, module).is_some(),
            "no disassembly loaded for {address}::{module}"
        );
    }
}

#[test]
fn each_instruction_steps_on_its_disassembly_line() {
    let _nim = NIM_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let out = tempfile::tempdir().unwrap();
    let conversion = convert_aptos_replay_dir(&fixture(), out.path()).unwrap();
    assert_eq!(
        (conversion.modules_loaded, conversion.modules_rejected.len()),
        (16, 0)
    );
    let bundle = read_bundle(&container(out.path()));
    let trace = trace();
    let instructions = instruction_count(&trace);

    // Every instruction is its own line of its disassembly, so every
    // instruction is a step: none is dropped and none is invented.
    assert_eq!(bundle.steps.len(), instructions, "one step per instruction");

    // Every step is on a framework disassembly, recorded relative to the
    // replay directory, at a line that exists in it.
    let texts: HashMap<PathBuf, usize> = aptos_replay::executed_modules(&trace)
        .unwrap()
        .into_iter()
        .map(|(_, m)| (masm_path(&m), masm(&m).lines().count()))
        .collect();
    for (path, line) in &bundle.steps {
        let len = texts
            .get(path)
            .unwrap_or_else(|| panic!("step on unexpected path {path:?}"));
        assert!((1..=*len as u64).contains(line), "{path:?}:{line}");
    }

    // The payload, `0x1::aptos_account::transfer` (definition 1), starts on
    // its first instruction line.
    let transfer = (
        masm_path("aptos_account"),
        first_instruction_line(&masm("aptos_account"), 1),
    );
    assert!(bundle.steps.contains(&transfer), "{transfer:?}");
    // The entry step is the prologue's first instruction.
    assert_eq!(
        bundle.steps[0],
        (
            masm_path("transaction_validation"),
            first_instruction_line(&masm("transaction_validation"), 24)
        )
    );
}

#[test]
fn control_without_disassembly_the_whole_transaction_is_one_step() {
    // The phase-1 result: the same trace with no disassembly collapses to
    // the entry step, so the step count above is the disassembly's doing.
    let _nim = NIM_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let out = tempfile::tempdir().unwrap();
    converter::convert_trace_with_debug_info(
        &trace(),
        Path::new("transaction.masm"),
        DebugInfo::empty(),
        out.path(),
        ConverterOptions::default(),
        None,
    )
    .unwrap();
    let bundle = read_bundle(&container(out.path()));
    assert_eq!(bundle.steps.len(), 1);
}

#[test]
fn typed_values_are_recorded_at_the_steps_that_produce_them() {
    let _nim = NIM_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let out = tempfile::tempdir().unwrap();
    convert_aptos_replay_dir(&fixture(), out.path()).unwrap();
    let bundle = read_bundle(&container(out.path()));

    // Values spread over the steps instead of piling up on the entry step.
    // Most instructions move a value (a load, a local access, a call's
    // arguments); the rest (branches, `ret`, `nop`, ...) leave none.
    let steps_with_values = bundle.values_per_step.iter().filter(|&&n| n > 0).count();
    assert!(
        steps_with_values * 10 > bundle.steps.len() * 8,
        "only {steps_with_values} of {} steps carry values",
        bundle.steps.len()
    );
    assert!(bundle.values_per_step[0] < 10);

    // The payload's entry step holds typed values for its parameters and
    // the locals its first instruction touches.
    let transfer_line = first_instruction_line(&masm("aptos_account"), 1);
    let step = bundle
        .steps
        .iter()
        .position(|s| *s == (masm_path("aptos_account"), transfer_line))
        .unwrap() as u64;
    let values: Vec<serde_json::Value> =
        serde_json::from_str(&bundle.reader.values_json(step).unwrap()).unwrap();
    let named: Vec<(String, String)> = values
        .iter()
        .map(|v| {
            (
                bundle
                    .reader
                    .varname(v["varname_id"].as_u64().unwrap())
                    .unwrap(),
                bundle
                    .reader
                    .type_name(v["type_id"].as_u64().unwrap())
                    .unwrap(),
            )
        })
        .collect();
    // `transfer(l0: &signer, l1: address, l2: u64)`: its typed arguments,
    // and the first instruction's read of `l1` and push of the address.
    for expected in [
        ("arg0", "&"),
        ("arg1", "address"),
        ("arg2", "u64"),
        ("local_1", "address"),
        ("stack_top", "address"),
    ] {
        assert!(
            named.contains(&(expected.0.to_string(), expected.1.to_string())),
            "{expected:?} not among the payload entry step's values {named:?}"
        );
    }
}

#[test]
fn control_the_stock_move_vm_trace_carries_no_values() {
    // The stock Aptos CLI's MOVE_VM_TRACE for the same transaction, through
    // the CSV adapter: it has steps (one per pc) but no value at any of
    // them, which is what the value trace adds.
    let _nim = NIM_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let csv = String::from_utf8(unzstd(&fixture().join("control-move-vm-trace.csv.zst"))).unwrap();
    let entries: Vec<AptosEnrichedEntry> = parse_move_vm_trace_csv(&csv)
        .into_iter()
        .map(|trace| AptosEnrichedEntry {
            trace,
            gas_cost: None,
            total_gas: None,
        })
        .collect();
    // The stock CLI saw the same execution: one entry per instruction.
    assert_eq!(entries.len(), instruction_count(&trace()));
    let out = tempfile::tempdir().unwrap();
    convert_aptos_trace(&entries, Path::new("aptos_transaction.move"), out.path()).unwrap();
    let bundle = read_bundle(&container(out.path()));
    assert!(bundle.steps.len() > 1000);
    assert_eq!(bundle.values_per_step.iter().sum::<u64>(), 0);
}

#[test]
fn events_are_detected_once_each_through_the_aptos_event_store() {
    let events = in_memory(&trace(), debug_info(), ConverterOptions::default());
    let emitted = events_tagged(&events, "MoveEvent");
    // The transfer withdraws, deposits, and the epilogue reports the fee:
    // three events, each reported once although it passes through both
    // `0x1::event::emit` and the `write_module_event_to_store` native.
    assert_eq!(emitted.len(), 3, "{emitted:?}");
    for (event, name) in emitted.iter().zip(["Withdraw", "Deposit", "FeeStatement"]) {
        assert!(
            event.contains(&format!("\"struct\":\"{name}\"")),
            "{event} should be a {name}"
        );
    }
    // CONTROL: read as a Sui trace, where events are `0x2::event::emit`,
    // the same trace reports none.
    let as_sui = in_memory(
        &trace(),
        debug_info(),
        ConverterOptions::default().with_chain(MoveChain::Sui),
    );
    assert!(events_tagged(&as_sui, "MoveEvent").is_empty());
}

#[test]
fn the_payload_frame_is_named_and_announced_as_the_payload() {
    let events = in_memory(&trace(), debug_info(), ConverterOptions::default());
    assert_eq!(
        events_tagged(&events, "MoveTransactionPhase"),
        vec![
            "prologue: 0x1::transaction_validation::versioned_prologue",
            "payload: 0x1::aptos_account::transfer",
            "epilogue: 0x1::transaction_validation::versioned_epilogue",
        ]
    );
    let names = function_names(&events);
    assert!(names.contains("0x1::aptos_account::transfer"), "{names:?}");
    // Functions that share a short name stay apart.
    assert!(names.contains("0x1::account::exists_at"), "{names:?}");
    assert!(names.contains("0x1::object::exists_at"), "{names:?}");
    // CONTROL: read as Sui, the outer module is the prologue's and every
    // framework frame keeps its bare name, so the payload is just
    // `transfer` and no phase is announced.
    let as_sui = in_memory(
        &trace(),
        debug_info(),
        ConverterOptions::default().with_chain(MoveChain::Sui),
    );
    let sui_names = function_names(&as_sui);
    assert!(sui_names.contains("transfer"));
    assert!(!sui_names.contains("0x1::aptos_account::transfer"));
    assert!(events_tagged(&as_sui, "MoveTransactionPhase").is_empty());
}

#[test]
fn the_chain_is_detected_from_the_trace() {
    // No chain given: the prologue frame identifies Aptos.
    let detected = in_memory(&trace(), DebugInfo::empty(), ConverterOptions::default());
    assert_eq!(events_tagged(&detected, "MoveEvent").len(), 3);
}

/// Aptos-shaped NDJSON (it opens with the prologue, so the converter reads
/// it as Aptos) whose frame writes one value of every signed width.
fn signed_trace() -> String {
    let write = |slot: u64, value: &str| {
        format!(
            r#"{{"Effect":{{"Write":{{"location":{{"Local":[0,{slot}]}},"root_value_after_write":{{"RuntimeValue":{{"value":{value}}}}}}}}}}}"#
        )
    };
    [
        r#"{"version":3}"#.to_string(),
        r#"{"OpenFrame":{"frame":{"frame_id":0,"function_name":"versioned_prologue","module":{"address":"0000000000000000000000000000000000000000000000000000000000000001","name":"transaction_validation"},"version_id":"0000000000000000000000000000000000000000000000000000000000000001","binary_member_index":24,"type_instantiation":[],"return_types":[],"parameters":[],"locals_types":[],"is_native":false},"gas_left":1000}}"#.to_string(),
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":1000,"instruction":"LD_I64"}}"#.to_string(),
        write(0, r#"{"type":"I8","value":-128}"#),
        write(1, r#"{"type":"I16","value":-300}"#),
        write(2, r#"{"type":"I32","value":-70000}"#),
        write(3, r#"{"type":"I64","value":-5}"#),
        write(4, r#"{"type":"I128","value":"-42"}"#),
        write(5, r#"{"type":"I128","value":"-170141183460469231731687303715884105728"}"#),
        write(
            6,
            r#"{"type":"I256","value":"-57896044618658097711785492504343953926634992332820282019728792003956564819968"}"#,
        ),
        r#"{"CloseFrame":{"frame_id":0,"return_":[],"gas_left":900}}"#.to_string(),
    ]
    .join("\n")
}

/// Each variable's recorded type name, as a real bundle names it.
fn bundle_value_types(trace: &str) -> HashMap<String, String> {
    let _nim = NIM_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let out = tempfile::tempdir().unwrap();
    converter::convert_trace_with_debug_info(
        trace.as_bytes(),
        Path::new("transaction.masm"),
        DebugInfo::empty(),
        out.path(),
        ConverterOptions::default(),
        None,
    )
    .unwrap();
    let bundle = read_bundle(&container(out.path()));
    #[derive(serde::Deserialize)]
    struct StepValue {
        varname_id: u64,
        type_id: u64,
    }
    let mut types = HashMap::new();
    for step in 0..bundle.steps.len() as u64 {
        let values: Vec<StepValue> =
            serde_json::from_str(&bundle.reader.values_json(step).unwrap()).unwrap();
        for v in values {
            types.insert(
                bundle.reader.varname(v.varname_id).unwrap(),
                bundle.reader.type_name(v.type_id).unwrap(),
            );
        }
    }
    types
}

#[test]
fn signed_integers_round_trip_with_their_types() {
    let events = in_memory(
        signed_trace().as_bytes(),
        DebugInfo::empty(),
        ConverterOptions::default(),
    );
    let types = bundle_value_types(&signed_trace());
    let mut names = Vec::new();
    let mut values: HashMap<String, (String, ValueRecord)> = HashMap::new();
    for event in &events {
        match event {
            TraceLowLevelEvent::VariableName(n) => names.push(n.clone()),
            TraceLowLevelEvent::Value(v) => {
                let name = names[v.variable_id.0].clone();
                values.insert(name.clone(), (types[&name].clone(), v.value.clone()));
            }
            _ => {}
        }
    }
    let int = |slot: &str| match &values[slot] {
        (ty, ValueRecord::Int { i, .. }) => (ty.as_str(), *i),
        other => panic!("{slot}: expected an Int, got {other:?}"),
    };
    assert_eq!(int("local_0"), ("i8", -128));
    assert_eq!(int("local_1"), ("i16", -300));
    assert_eq!(int("local_2"), ("i32", -70000));
    assert_eq!(int("local_3"), ("i64", -5));
    assert_eq!(int("local_4"), ("i128", -42));
    // An i128 wider than i64 keeps its sign and full magnitude (2^127).
    match &values["local_5"] {
        (ty, ValueRecord::BigInt { b, negative, .. }) => {
            assert_eq!(ty, "i128");
            assert!(*negative);
            let mut magnitude = vec![0x80];
            magnitude.extend([0; 15]);
            assert_eq!(b, &magnitude);
        }
        other => panic!("expected a BigInt, got {other:?}"),
    }
    match &values["local_6"] {
        (ty, ValueRecord::String { text, .. }) => {
            assert_eq!(ty, "i256");
            assert_eq!(
                text,
                "-57896044618658097711785492504343953926634992332820282019728792003956564819968"
            );
        }
        other => panic!("expected decimal text, got {other:?}"),
    }
    // CONTROL: every signed width was written, and none was read as
    // unsigned (an unsigned reading could not hold a negative value).
    assert_eq!(values.len(), 7);
}

#[test]
fn disassembly_parsing_counts_pcs_and_checks_the_markers() {
    let masm = "\
// Bytecode version v10
module 0x1::m
// Function definition at index 0
public fun f(l0: u64): u64
    local l1: u64
    copy_loc l0
    st_loc l1
    move_loc l1
    br_false l0
    ld_u64 1
    // @5
    ret
l0: ld_u64 2
    ret

// Function definition at index 1
native fun g(l0: u64)
";
    let parsed = parse_aptos_disassembly(masm).unwrap();
    assert_eq!(
        (parsed.address.as_str(), parsed.module.as_str()),
        ("0x1", "m")
    );
    let f = &parsed.functions[&0];
    assert_eq!(f.name, "f");
    assert_eq!(f.definition_line, 4);
    // pc 0 is the first instruction after the `local` line; pc 6 is the
    // labelled one, whose column is after its label.
    assert_eq!(f.pc_to_line(0), Some(6));
    assert_eq!(f.pc_to_line(5), Some(12));
    assert_eq!(f.pc_to_line(6), Some(13));
    assert_eq!(f.pc_to_column(6), Some(5));
    assert_eq!(f.pc_to_line(8), None);
    assert!(parsed.functions[&1].pc_to_line.is_empty());
    // CONTROL: a marker that disagrees with the count is refused.
    let skewed = masm.replace("// @5", "// @4");
    assert!(parse_aptos_disassembly(&skewed).unwrap_err().contains("@4"));
}

#[test]
fn a_disassembly_of_another_module_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let bytecode = dir.path().join(FRAMEWORK).join("bytecode");
    fs::create_dir_all(&bytecode).unwrap();
    // `signer`'s text filed as `event`.
    fs::write(bytecode.join("event.mv.masm"), masm("signer")).unwrap();
    let (info, rejected) = DebugInfo::from_aptos_replay_dir(dir.path()).unwrap();
    assert_eq!(info.module_count(), 0);
    assert_eq!(rejected.len(), 1, "{rejected:?}");
    // CONTROL: filed under its own name it loads.
    fs::rename(
        bytecode.join("event.mv.masm"),
        bytecode.join("signer.mv.masm"),
    )
    .unwrap();
    let (info, rejected) = DebugInfo::from_aptos_replay_dir(dir.path()).unwrap();
    assert_eq!((info.module_count(), rejected.len()), (1, 0));
}

#[test]
fn event_kind_is_a_structured_trace_event() {
    // The detected events use the same structured kind as Sui's.
    let events = in_memory(&trace(), debug_info(), ConverterOptions::default());
    let kinds: BTreeSet<String> = events
        .iter()
        .filter_map(|e| match e {
            TraceLowLevelEvent::Event(r) if r.metadata == "MoveEvent" => {
                Some(format!("{:?}", r.kind))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        BTreeSet::from([format!("{:?}", EventLogKind::TraceLogEvent)])
    );
}
