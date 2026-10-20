//! Converting a `sui replay --trace` output directory.
//!
//! `sui replay` writes, beside `trace.json.zst`, one directory per package
//! storage id holding each module's disassembly (`bytecode/<m>.mvb`) and
//! debug info (`bytecode/<m>.json`) whose byte ranges index into that
//! disassembly.  These tests build such a directory by hand, in the exact
//! shape `sui replay` 1.81 writes it (version-2 debug info,
//! `from_file_path: null`, `file_hash` = SHA-256 of the `.mvb`), and drive
//! the converter over it into the in-memory `NonStreamingTraceWriter`.
//!
//! No mock stands in for anything the recorder calls: the directory is
//! real files read by the real loader.  Paths and steps are checked in a
//! real `.ct` bundle read back through the Nim reader, because the
//! in-memory test writer emits a `Path` event for every registration
//! without interning it, so its event order is not its path-id space.
//! Variable names are checked through the in-memory writer, whose
//! variable interning is exact.  Building the directory by hand rather than committing a
//! real replay is deliberate: a real one is ~6 MB of third-party
//! disassembly, and these tests need two packages that SHARE a module name,
//! which is what makes the package-qualified lookup observable.
//!
//! Every assertion names a control that would make it fail under the
//! defect it guards against (see `Verification-Harness-Traps.md`).

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use codetracer_trace_types::{TraceLowLevelEvent, ValueRecord};
use codetracer_trace_writer_nim::NimTraceReaderHandle;
use codetracer_trace_writer_nim::non_streaming_trace_writer::NonStreamingTraceWriter;
use sha2::{Digest, Sha256};

use codetracer_move_recorder::converter::{self, ConverterOptions};
use codetracer_move_recorder::move_debug_info::{DebugInfo, ReplayModuleRejection};

/// The Nim runtime behind the bundle writer and reader is process-global;
/// tests that touch it run one at a time.
static NIM_TEST_LOCK: Mutex<()> = Mutex::new(());

const PKG_A: &str = "0x00000000000000000000000000000000000000000000000000000000000000aa";
const PKG_B: &str = "0x00000000000000000000000000000000000000000000000000000000000000bb";
const PKG_BAD: &str = "0x00000000000000000000000000000000000000000000000000000000000000cc";

/// Disassembly of `m` in package A: `f(n)` with one local `a`.
const MVB_A: &str = "// Move bytecode v6\nmodule aa.m {\n\npublic f(n: u64) {\nL0:\n\tLdU64(1)\n\tStLoc[1](a: u64)\n}\n}\n";
/// Disassembly of `m` in package B: `g(x)` with one local `y`.  Same module
/// name as A, different text and different names.
const MVB_B: &str = "// Move bytecode v6\nmodule bb.m {\n\n\npublic g(x: u64) {\nL0:\n\tLdU64(2)\n\tStLoc[1](y: u64)\n\tRet\n}\n}\n";

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Byte range of the first occurrence of `needle` in `text`.
fn range(text: &str, needle: &str) -> (usize, usize) {
    let start = text
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in text"));
    (start, start + needle.len())
}

/// Debug info for a module with one function `name(param)` whose single
/// local is `local`; pc 0 maps to the `LdU64` line and pc 1 to the `StLoc`
/// line of `mvb`.  `hash` is written as the file hash, so a caller can
/// make it disagree with the text.
fn debug_json(pkg: &str, mvb: &str, name: &str, param: &str, local: &str, hash: &str) -> String {
    let loc =
        |(s, e): (usize, usize)| serde_json::json!({ "file_hash": hash, "start": s, "end": e });
    serde_json::json!({
        "version": 2,
        "from_file_path": null,
        "definition_location": loc((0, mvb.len())),
        "module_name": [pkg.trim_start_matches("0x"), "m"],
        "struct_map": {},
        "enum_map": {},
        "function_map": {
            "0": {
                "location": loc((0, mvb.len())),
                "definition_location": loc(range(mvb, name)),
                "type_parameters": [],
                "parameters": [[param, loc(range(mvb, param))]],
                "locals": [[local, loc(range(mvb, local))]],
                "returns": [],
                "nops": {},
                "code_map": {
                    "0": loc(range(mvb, "LdU64")),
                    "1": loc(range(mvb, "StLoc")),
                },
                "is_native": false
            }
        },
        "constant_map": {}
    })
    .to_string()
}

fn write_module(tx_dir: &Path, pkg: &str, mvb: &str, json: &str) {
    let dir = tx_dir.join(pkg).join("bytecode");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("m.mvb"), mvb).unwrap();
    fs::write(dir.join("m.json"), json).unwrap();
}

/// A replay directory with packages A and B (both defining module `m`)
/// and package BAD, whose debug info names a hash its text does not have.
fn replay_dir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let tx = tmp.path();
    let hash_a = sha256_hex(MVB_A.as_bytes());
    let hash_b = sha256_hex(MVB_B.as_bytes());
    write_module(
        tx,
        PKG_A,
        MVB_A,
        &debug_json(PKG_A, MVB_A, "f", "n", "a", &hash_a),
    );
    write_module(
        tx,
        PKG_B,
        MVB_B,
        &debug_json(PKG_B, MVB_B, "g", "x", "y", &hash_b),
    );
    // BAD carries A's debug info (and A's hash) over B's text.
    write_module(
        tx,
        PKG_BAD,
        MVB_B,
        &debug_json(PKG_BAD, MVB_A, "f", "n", "a", &hash_a),
    );
    tmp
}

fn open_frame(frame_id: u64, function: &str, pkg: &str) -> String {
    format!(
        r#"{{"OpenFrame":{{"frame":{{"frame_id":{frame_id},"function_name":"{function}","module":{{"address":"{pkg}","name":"m"}},"version_id":"{}","binary_member_index":0,"type_instantiation":[],"parameters":[{{"RuntimeValue":{{"value":{{"type":"U64","value":7}}}}}}],"return_types":[],"locals_types":[],"is_native":false}},"gas_left":1000}}}}"#,
        pkg.trim_start_matches("0x")
    )
}

fn instruction(pc: u64, op: &str) -> String {
    format!(
        r#"{{"Instruction":{{"type_parameters":[],"pc":{pc},"gas_left":900,"instruction":"{op}"}}}}"#
    )
}

fn write(location: &str, value: u64) -> String {
    format!(
        r#"{{"Effect":{{"Write":{{"location":{location},"root_value_after_write":{{"RuntimeValue":{{"value":{{"type":"U64","value":{value}}}}}}}}}}}}}"#
    )
}

fn close_frame(frame_id: u64) -> String {
    format!(r#"{{"CloseFrame":{{"frame_id":{frame_id},"return_":[],"gas_left":800}}}}"#)
}

/// `A::m::f` (frame 1) runs pc 0..1 and writes its param slot 0 and local
/// slot 1, then calls `B::m::g` (frame 4), which runs pc 0..1, writes its
/// own local slot 1, writes the CALLER's local (frame 1, slot 1) through a
/// `&mut`, and writes a storage reference.
fn trace() -> String {
    [
        r#"{"version":3}"#.to_string(),
        open_frame(1, "f", PKG_A),
        instruction(0, "LdU64"),
        write(r#"{"Local":[1,0]}"#, 10),
        instruction(1, "StLoc"),
        write(r#"{"Local":[1,1]}"#, 11),
        open_frame(4, "g", PKG_B),
        instruction(0, "LdU64"),
        instruction(1, "StLoc"),
        write(r#"{"Local":[4,1]}"#, 12),
        write(r#"{"Indexed":[{"Local":[1,1]},0]}"#, 13),
        write(r#"{"Global":5}"#, 14),
        close_frame(4),
        close_frame(1),
    ]
    .join("\n")
}

/// (variable name, value) of every integer `Value`, in order, from the
/// in-memory writer.
fn converted_values(debug_info: DebugInfo, trace: &str) -> Vec<(String, i64)> {
    let mut writer = NonStreamingTraceWriter::new("m", &[]);
    converter::convert_trace_into_writer_with_debug_info(
        trace.as_bytes(),
        Path::new("primary.mvb"),
        debug_info,
        &mut writer,
        ConverterOptions::default(),
    )
    .expect("conversion succeeds");

    let mut names = Vec::new();
    let mut values = Vec::new();
    for event in &writer.events {
        match event {
            TraceLowLevelEvent::VariableName(n) => names.push(n.clone()),
            TraceLowLevelEvent::Value(v) => {
                if let ValueRecord::Int { i, .. } = v.value {
                    values.push((names[v.variable_id.0].clone(), i));
                }
            }
            _ => {}
        }
    }
    values
}

/// What a real bundle records: its path table, and (path, line) per step.
struct Bundle {
    paths: Vec<PathBuf>,
    steps: Vec<(PathBuf, i64)>,
}

/// Write a real `.ct` bundle and read it back.  Callers hold
/// `NIM_TEST_LOCK`.
fn converted_bundle(debug_info: DebugInfo, trace: &str) -> Bundle {
    let out = tempfile::tempdir().unwrap();
    converter::convert_trace_with_debug_info(
        trace.as_bytes(),
        Path::new("primary.mvb"),
        debug_info,
        out.path(),
        ConverterOptions::default(),
    )
    .expect("conversion succeeds");
    let ct = out.path().join("primary.ct");
    let reader = NimTraceReaderHandle::open(ct.to_str().unwrap()).expect("bundle opens");
    let paths: Vec<PathBuf> = (0..reader.path_count())
        .map(|id| PathBuf::from(reader.path(id).unwrap()))
        .collect();
    // The recorder writes column-aware steps, whose positions only the
    // column-aware bulk read decodes into lines.
    assert!(reader.has_column_aware_steps());
    let n = reader.step_count();
    let (mut path_ids, mut lines, mut columns) = (
        vec![0; n as usize],
        vec![0; n as usize],
        vec![0; n as usize],
    );
    let written = reader
        .step_locations_with_columns(0, n, &mut path_ids, &mut lines, &mut columns)
        .unwrap();
    assert_eq!(written, n);
    let steps = path_ids
        .iter()
        .zip(&lines)
        .map(|(&path_id, &line)| (paths[path_id as usize].clone(), line as i64))
        .collect();
    Bundle { paths, steps }
}

fn load(dir: &Path) -> (DebugInfo, Vec<ReplayModuleRejection>) {
    DebugInfo::from_sui_replay_dir(dir).expect("replay dir readable")
}

fn relative_mvb(pkg: &str) -> PathBuf {
    Path::new(pkg).join("bytecode").join("m.mvb")
}

fn line_of(text: &str, needle: &str) -> i64 {
    text[..text.find(needle).unwrap()].matches('\n').count() as i64 + 1
}

#[test]
fn a_module_whose_text_does_not_match_its_hash_is_refused_by_name() {
    let dir = replay_dir();
    let (info, rejected) = load(dir.path());
    // A and B load; BAD is refused for the hash, and named.
    assert_eq!(info.module_count(), 2);
    assert_eq!(rejected.len(), 1, "{rejected:?}");
    match &rejected[0] {
        ReplayModuleRejection::HashMismatch {
            json,
            expected,
            actual,
        } => {
            assert!(json.starts_with(dir.path().join(PKG_BAD)), "{json:?}");
            assert_eq!(expected, &sha256_hex(MVB_A.as_bytes()));
            assert_eq!(actual, &sha256_hex(MVB_B.as_bytes()));
        }
        other => panic!("expected HashMismatch, got {other:?}"),
    }
    // CONTROL: the same text with its own hash loads, so the refusal is
    // about the mismatch rather than about the package.
    let hash_b = sha256_hex(MVB_B.as_bytes());
    write_module(
        dir.path(),
        PKG_BAD,
        MVB_B,
        &debug_json(PKG_BAD, MVB_B, "g", "x", "y", &hash_b),
    );
    let (info, rejected) = load(dir.path());
    assert_eq!((info.module_count(), rejected.len()), (3, 0));
}

#[test]
fn each_step_lands_in_the_disassembly_of_the_package_that_ran() {
    let _nim = NIM_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = replay_dir();
    let (info, _) = load(dir.path());
    let out = converted_bundle(info, &trace());

    // The entry step is A's pc 0 (seeded by `start`); then A's pc 1, and
    // B's pc 0 and 1, each on its own module's disassembly.
    let a = relative_mvb(PKG_A);
    let b = relative_mvb(PKG_B);
    assert_eq!(
        out.steps,
        vec![
            (PathBuf::from("primary.mvb"), line_of(MVB_A, "LdU64")),
            (a.clone(), line_of(MVB_A, "StLoc")),
            (b.clone(), line_of(MVB_B, "LdU64")),
            (b.clone(), line_of(MVB_B, "StLoc")),
        ]
    );
    // CONTROL: the two modules share the short name `m` and their lines
    // differ, so a short-name lookup would put B's steps on A's file or
    // on A's line numbers; the assertion above distinguishes them.
    assert_ne!(line_of(MVB_A, "StLoc"), line_of(MVB_B, "StLoc"));
}

#[test]
fn recorded_paths_are_relative_to_the_replay_directory() {
    let _nim = NIM_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = replay_dir();
    let (info, _) = load(dir.path());
    let out = converted_bundle(info, &trace());
    let expected: BTreeSet<PathBuf> = [
        relative_mvb(PKG_A),
        relative_mvb(PKG_B),
        PathBuf::from("primary.mvb"),
    ]
    .into();
    assert_eq!(out.paths.iter().cloned().collect::<BTreeSet<_>>(), expected);
    // CONTROL: the replay directory itself is absolute, so a loader that
    // recorded the paths it read would fail the equality above.
    assert!(dir.path().is_absolute());
}

#[test]
fn slots_are_named_parameters_first_then_locals_by_the_frame_that_owns_them() {
    let dir = replay_dir();
    let (info, _) = load(dir.path());
    let by_value: HashMap<i64, String> = converted_values(info, &trace())
        .into_iter()
        .map(|(n, v)| (v, n))
        .collect();

    // Slot 0 of f is its parameter `n`; slot 1 its first local `a`.
    assert_eq!(by_value[&10], "n");
    assert_eq!(by_value[&11], "a");
    // Slot 1 of g is its local `y`.
    assert_eq!(by_value[&12], "y");
    // g writes f's slot 1 through a `&mut`: the owner is frame 1, so the
    // name is f's `a`, not g's `y`.
    assert_eq!(by_value[&13], "a");
    // A storage reference has no local name.
    assert_eq!(by_value[&14], "global_5");
    // CONTROL: the parameters-first numbering is observable only because
    // each function has a parameter; with `locals[slot]` slot 1 would be
    // unnamed and slot 0 would read `a`/`y`.
    assert_ne!(by_value[&10], by_value[&11]);
}

#[test]
fn identical_inputs_produce_identical_events() {
    // Path ids follow registration order; registering from a hash map
    // would reorder them between runs.  Several fresh loads make that
    // visible with high probability.
    let _nim = NIM_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = replay_dir();
    let first = {
        let (info, _) = load(dir.path());
        converted_bundle(info, &trace()).paths
    };
    for _ in 0..8 {
        let (info, _) = load(dir.path());
        assert_eq!(converted_bundle(info, &trace()).paths, first);
    }
}

#[test]
fn a_trace_with_no_events_is_refused_by_name_and_writes_nothing() {
    // `sui replay` writes a header-only trace for transactions that ran on
    // execution layers without trace hooks (mainnet protocol < 38).
    let dir = replay_dir();
    let write_trace = |text: &str| {
        let mut enc = zstd::Encoder::new(Vec::new(), 1).unwrap();
        std::io::Write::write_all(&mut enc, text.as_bytes()).unwrap();
        fs::write(dir.path().join("trace.json.zst"), enc.finish().unwrap()).unwrap();
    };
    let out = tempfile::tempdir().unwrap();

    write_trace("{\"version\":3}\n");
    let err = codetracer_move_recorder::replay::convert_sui_replay_dir(dir.path(), out.path())
        .expect_err("a header-only trace must be refused");
    assert!(
        err.to_string()
            .starts_with(codetracer_move_recorder::replay::NO_TRACE_EVENTS),
        "{err}"
    );
    assert_eq!(
        fs::read_dir(out.path()).unwrap().count(),
        0,
        "nothing written"
    );

    // CONTROL: the same directory with events converts, so the refusal is
    // about the empty trace and not about the directory.
    let _nim = NIM_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    write_trace(&trace());
    codetracer_move_recorder::replay::convert_sui_replay_dir(dir.path(), out.path())
        .expect("a trace with events converts");
    assert!(fs::read_dir(out.path()).unwrap().count() > 0);
}
