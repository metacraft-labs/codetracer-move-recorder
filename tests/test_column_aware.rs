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

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use codetracer_trace_writer_nim::NimTraceReaderHandle;

use codetracer_move_recorder::aptos_adapter::{
    AptosEnrichedEntry, AptosTraceEntry, convert_aptos_trace,
};
use codetracer_move_recorder::converter::{self, ConverterOptions};
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

// ---------------------------------------------------------------------------
// Real-Move multi-statement-per-line column verification.
//
// This is the strict end-to-end pin requested by the column-aware audit:
// build a real Sui Move package whose `#[test]` function packs three
// statements onto a single source line, record the resulting trace
// through the recorder's debug-info-enabled converter path, run
// `ct-print --full`, and assert that the multi-statement line surfaces
// >= 3 distinct columns in the decoded JSON.  Mirrors the EVM
// recorder's `test_column_aware_distinct_columns_on_one_line` against
// `test-programs/column_aware/ColumnAware.sol`.
// ---------------------------------------------------------------------------

const COLUMN_AWARE_PACKAGE: &str = "test-programs/move/column_aware";
const COLUMN_AWARE_MODULE: &str = "column_aware";
const COLUMN_AWARE_TEST_FN: &str = "test_multi_statement_line";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ct_print_path() -> PathBuf {
    manifest_dir()
        .join("..")
        .join("codetracer-trace-format-nim")
        .join("ct-print")
}

fn sui_is_available() -> bool {
    Command::new("sui")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn move_recorder_surfaces_distinct_columns_for_multi_statement_line() {
    let _guard = NIM_TEST_LOCK.lock().unwrap();

    let ct_print = ct_print_path();
    if !ct_print.exists() {
        eprintln!(
            "SKIP: move_recorder_surfaces_distinct_columns_for_multi_statement_line \
             requires ct-print at {} — only available within the metacraft workspace \
             where codetracer-trace-format-nim is a sibling.",
            ct_print.display()
        );
        return;
    }
    if !sui_is_available() {
        eprintln!(
            "SKIP: move_recorder_surfaces_distinct_columns_for_multi_statement_line \
             requires the `sui` CLI on PATH (use the Nix dev shell)."
        );
        return;
    }

    // ---- 1. Build the column_aware package + run its #[test] fn -----------
    let package_dir = manifest_dir().join(COLUMN_AWARE_PACKAGE);
    assert!(
        package_dir.join("Move.toml").exists(),
        "column_aware Move package missing at {}",
        package_dir.display()
    );

    let test_output = Command::new("sui")
        .args(["move", "test", "--trace", COLUMN_AWARE_TEST_FN])
        .current_dir(&package_dir)
        .output()
        .expect("failed to spawn `sui move test --trace`");
    assert!(
        test_output.status.success(),
        "`sui move test --trace` failed; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&test_output.stdout),
        String::from_utf8_lossy(&test_output.stderr)
    );

    let trace_zst = package_dir.join("traces").join(format!(
        "{COLUMN_AWARE_MODULE}__{COLUMN_AWARE_MODULE}__{COLUMN_AWARE_TEST_FN}.json.zst"
    ));
    assert!(
        trace_zst.exists(),
        "expected Sui to write the per-test trace at {}",
        trace_zst.display()
    );

    // Decompress.
    let zst_bytes = std::fs::read(&trace_zst).expect("read trace .zst");
    let mut decoder =
        zstd::Decoder::new(zst_bytes.as_slice()).expect("zstd decoder for trace fixture");
    let mut trace_bytes = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut trace_bytes).expect("decompress trace");

    // ---- 2. Convert through the recorder's debug-info-enabled path -------
    let source_path = package_dir
        .join("sources")
        .join(format!("{COLUMN_AWARE_MODULE}.move"));
    assert!(source_path.exists(), "source missing: {}", source_path.display());

    let tmp = tempfile::tempdir().expect("tempdir");
    let out_dir = tmp.path().join("ct-out");

    // `for_ct_record_flow()` opts into the per-package debug-info source-line
    // resolution path that carries the per-PC column from the Move compiler's
    // `code_map`.  Without this, the converter forwards `column=None` for
    // every step (legacy SourceMapResolver carries only lines).
    converter::convert_trace_with_options(
        &trace_bytes,
        &SourceMapResolver::empty(),
        &source_path,
        &out_dir,
        ConverterOptions::default().for_ct_record_flow(),
    )
    .expect("convert_trace_with_options");

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

    // ---- 3. ct-print --full + parse JSON ---------------------------------
    let dump = Command::new(&ct_print)
        .args(["--full", "--strip-paths"])
        .arg(&ct_files[0])
        .output()
        .expect("failed to spawn ct-print");
    assert!(
        dump.status.success(),
        "ct-print --full should succeed; stderr: {}",
        String::from_utf8_lossy(&dump.stderr)
    );
    let doc: serde_json::Value =
        serde_json::from_slice(&dump.stdout).expect("ct-print --full should emit valid JSON");

    // ---- 4. Assertions ---------------------------------------------------

    // (a) meta.dat bit 4 advertised through the canonical
    //     `metadata.flags.has_column_aware_steps` JSON path.
    assert_eq!(
        doc["metadata"]["flags"]["has_column_aware_steps"].as_bool(),
        Some(true),
        "trace metadata must advertise has_column_aware_steps=true; got {:?}",
        doc["metadata"]
    );

    // (b) Find the multi-statement source line: identify it by parsing the
    //     source file and locating the first line that contains three
    //     `blackbox(` call sites.  This keeps the assertion robust to the
    //     fixture's doc-comment block drifting line numbers.
    let source_text = std::fs::read_to_string(&source_path).expect("read source");
    let target_line: i64 = source_text
        .lines()
        .enumerate()
        .find_map(|(i, line)| (line.matches("blackbox(").count() >= 3).then_some((i + 1) as i64))
        .expect("fixture must contain a line with >=3 blackbox() call sites");

    // (c) Collect the set of distinct (step) columns surfaced on
    //     `target_line` in the trace.
    let events = doc["events"].as_array().expect("events array");
    let mut cols_on_target = std::collections::BTreeSet::<i64>::new();
    let mut cols_by_line: std::collections::BTreeMap<i64, std::collections::BTreeSet<i64>> =
        std::collections::BTreeMap::new();
    for ev in events {
        if ev["kind"] != "step" {
            continue;
        }
        let Some(line) = ev["line"].as_i64() else { continue };
        let Some(col) = ev["column"].as_i64() else { continue };
        cols_by_line.entry(line).or_default().insert(col);
        if line == target_line {
            cols_on_target.insert(col);
        }
    }

    assert!(
        cols_on_target.len() >= 3,
        "multi-statement line {target_line} should surface >= 3 distinct step columns, \
         got {cols_on_target:?}; full line->cols map: {cols_by_line:?}",
    );
    for col in &cols_on_target {
        assert!(
            *col >= 1,
            "step column must be >= 1 (1-based on the wire); got {col} on line {target_line}",
        );
    }

    eprintln!(
        "PASS: column-aware step emission — line {target_line} surfaces distinct columns {cols_on_target:?}"
    );

    drop(tmp);
}
