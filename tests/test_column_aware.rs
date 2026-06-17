//! Integration test for column-aware replay navigation.
//!
//! Verifies the Move recorder's column-aware step pipeline end-to-end:
//!
//!   1. `converter::convert_trace` produces a `.ct` bundle whose
//!      `meta.dat` carries `FlagHasColumnAwareSteps` (bit 4) so the
//!      column-aware reader path is engaged at replay time.
//!   2. The recorder registers the primary source path with a
//!      per-line UTF-8 byte-length table (paths.dat Layout A) so the
//!      reader can map the writer-side `global_position_index` back
//!      to `(line, column)`.
//!   3. The Aptos adapter (`MOVE_VM_TRACE` CSV path), which has no
//!      source-column information, still emits the column-aware flag
//!      so column-aware readers handle it cleanly — every step's
//!      column resolves to `None` (line-only step) per the
//!      `Stop only if writer wrapper missing.  If column info absent,
//!      still emit flag + None.` contract from the implementation
//!      plan.
//!
//! Mirrors the EVM/Solana recorder column-aware tests
//! (`codetracer-evm-recorder` M14 / `codetracer-solana-recorder` P6).
//!
//! The Nim runtime is global-state-bound and must serialise across all
//! Nim-backed tests in this binary — `NIM_TEST_LOCK` mirrors the
//! pattern from
//! `codetracer_trace_writer_nim/tests/register_step_with_column_roundtrip.rs`.

use std::path::Path;
use std::sync::Mutex;

use codetracer_trace_writer_nim::NimTraceReaderHandle;

use codetracer_move_recorder::aptos_adapter::{
    AptosEnrichedEntry, AptosTraceEntry, convert_aptos_trace,
};
use codetracer_move_recorder::converter;
use codetracer_move_recorder::source_map::SourceMapResolver;

static NIM_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Minimal NDJSON v3 trace fragment exercising a single function +
/// instruction so the recorder hits its `register_step_with_column`
/// path at least once.  The source-map resolver below pins
/// `pc=0` to `line=3` so the resulting step is deterministic.
fn synthetic_trace() -> String {
    [
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"f","module":{"address":"0x0","name":"flow_test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"u64"}],"is_native":false},"gas_left":1000}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":990,"instruction":"LdU64(1)"}}"#,
        r#"{"Effect":{"Write":{"location":{"Local":[1,0]},"root_value_after_write":{"RuntimeValue":{"value":{"type":"U64","value":1}}}}}}"#,
        r#"{"CloseFrame":{"frame_id":1,"return_":[],"gas_left":980}}"#,
    ]
    .join("\n")
}

/// Write a fake `flow_test.move` to `dir` so the writer can read
/// per-line byte counts when computing the `paths.dat` Layout A
/// table.  The content shape (4 lines of varying length) is what the
/// reader's `line_length` query is asserted against below.
fn write_fixture_source(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("flow_test.move");
    // 4 lines with byte lengths [9, 8, 18, 1]
    //   line 1: "module {"           — 9 bytes (\n excluded)
    //   line 2: "  fun f {"          — 8 bytes  (wait — recount)
    //   line 3: "    let _x = 1;"    — 18 bytes
    //   line 4: "}"                  — 1 byte
    // We pin the expected lengths from `compute_line_lengths` rather
    // than counting by hand below to avoid fencepost mistakes in the
    // assertions.
    let content = "module {\n  fun f {\n    let _x = 1;\n}";
    std::fs::write(&path, content).expect("write fixture source");
    path
}

#[test]
fn move_converter_enables_column_aware_steps_and_registers_line_lengths() {
    let _guard = NIM_TEST_LOCK.lock().unwrap();

    let tmp = tempfile::tempdir().expect("tempdir");
    let source_path = write_fixture_source(tmp.path());
    let out_dir = tmp.path().join("ct-out");

    // Pin `pc=0` to line 3 so the recorder hits its column-aware step
    // emission path against a known line.  Aptos has no column source
    // here (SourceMapResolver only carries lines) — the converter
    // therefore forwards `column=None`, which is exactly the
    // "emit flag + None when column info absent" branch we want to
    // pin from the integration side.
    let source_map = SourceMapResolver::from_entries(vec![(
        "flow_test".to_string(),
        0,
        source_path.to_string_lossy().into_owned(),
        3,
    )]);

    converter::convert_trace(
        synthetic_trace().as_bytes(),
        &source_map,
        &source_path,
        &out_dir,
    )
    .expect("convert_trace");

    // Locate the .ct bundle.
    let ct_files: Vec<_> = std::fs::read_dir(&out_dir)
        .expect("read out_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ct"))
        .collect();
    assert!(
        !ct_files.is_empty(),
        "expected a .ct container in {}",
        out_dir.display()
    );

    // Keep the tempdir alive through the reader assertions: persist a
    // local copy of the path inside the still-live `tmp` guard.
    let ct_path = ct_files[0].clone();
    let reader =
        NimTraceReaderHandle::open(ct_path.to_str().expect("ct path utf-8")).expect("reader open");

    // (1) The recorder's `enable_column_aware_steps` call must surface
    // as `FlagHasColumnAwareSteps` (meta.dat bit 4) so column-aware
    // readers engage their decode path.  Pre-implementation this flag
    // was always false because the move recorder used the column-less
    // `register_step` entry point.
    assert!(
        reader.has_column_aware_steps(),
        "Move recorder must opt the trace into column-aware mode (meta.dat bit 4 / \
         FlagHasColumnAwareSteps); the column-aware reader otherwise refuses to \
         decode DeltaColumn events for this trace."
    );

    // (2) The primary source path must be registered with a per-line
    // UTF-8 byte-length table so the reader's GLI→(line, column)
    // resolution has bounds to clamp against.  `flow_test.move` is
    // line 1 of the registered table — assert the byte counts that
    // `compute_line_lengths` derived from the fixture source above.
    //
    // Expected layout (1-based lines, 0-based reader indices):
    //   line 1: "module {"      -> 8 bytes
    //   line 2: "  fun f {"     -> 9 bytes
    //   line 3: "    let _x = 1;" -> 15 bytes
    //   line 4: "}"             -> 1 byte
    assert!(reader.path_count() >= 1, "trace must register the source path");
    assert_eq!(
        reader.line_length(0, 0),
        Some(8),
        "line 1 byte count from paths.dat Layout A"
    );
    assert_eq!(
        reader.line_length(0, 1),
        Some(9),
        "line 2 byte count from paths.dat Layout A"
    );
    assert_eq!(
        reader.line_length(0, 2),
        Some(15),
        "line 3 byte count from paths.dat Layout A"
    );
    assert_eq!(
        reader.line_length(0, 3),
        Some(1),
        "line 4 byte count from paths.dat Layout A"
    );
    assert_eq!(
        reader.line_length(0, 4),
        None,
        "queries past the registered line range MUST return None \
         (back-compat-safe default per P6.5)"
    );

    drop(reader);
    drop(tmp);
}

#[test]
fn aptos_adapter_enables_column_aware_steps_for_column_less_trace() {
    let _guard = NIM_TEST_LOCK.lock().unwrap();

    let tmp = tempfile::tempdir().expect("tempdir");
    let source_path = write_fixture_source(tmp.path());
    let out_dir = tmp.path().join("ct-out");

    // Aptos MOVE_VM_TRACE entries have no source-column information;
    // synthesise a single (function, PC) row so the adapter hits its
    // `register_step_with_column` call site.
    let entries = vec![AptosEnrichedEntry {
        trace: AptosTraceEntry {
            function_name: "0x1::flow_test::f".to_string(),
            pc: 0,
        },
        gas_cost: None,
        total_gas: None,
    }];

    convert_aptos_trace(&entries, &source_path, &out_dir).expect("convert_aptos_trace");

    let ct_files: Vec<_> = std::fs::read_dir(&out_dir)
        .expect("read out_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ct"))
        .collect();
    assert!(
        !ct_files.is_empty(),
        "expected a .ct container in {}",
        out_dir.display()
    );

    let reader =
        NimTraceReaderHandle::open(ct_files[0].to_str().expect("ct path utf-8")).expect("reader open");

    // The Aptos adapter must still flip the column-aware flag even
    // though every emitted step carries `column=None`.  This
    // guarantees the resulting trace is wire-compatible with the
    // column-aware replay reader and the `None` column propagates
    // cleanly back at read time (rather than the reader rejecting the
    // bundle for missing per-line tables).
    assert!(
        reader.has_column_aware_steps(),
        "Aptos adapter MUST enable column-aware mode even when no source-column \
         info is available (every step emits column=None) — the recorder plan \
         contract is `still emit flag + None`."
    );
    assert!(
        reader.path_count() >= 1,
        "Aptos adapter MUST register the source path so the column-aware reader \
         has a path-table entry to clamp GLI lookups against"
    );

    drop(reader);
    drop(tmp);
}
