//! Per-program `ct print --full` coverage tests for the Move recorder.
//!
//! These tests follow `metacraft-specs/policies/recorder-test-requirements.md`:
//!
//! * Each test consumes one of the pre-recorded NDJSON traces shipped
//!   under `test-programs/move/flow_test/traces/` (a real
//!   `sui move test --trace-execution` capture against the `flow_test`
//!   Move package — the recorder's normal entry point).  The Move
//!   recorder is a converter: it accepts NDJSON `move-trace-format` v3
//!   data and emits a CTFS bundle.  The fixtures are checked in so the
//!   tests are reproducible without the (un-Nix-packaged) `sui` CLI.
//!   The end-to-end pipeline that calls `sui move test --trace-execution`
//!   itself is exercised by `test_sui_integration::test_sui_move_trace_integration`.
//! * The produced `.ct` is piped through `ct-print --full --strip-paths`.
//! * Assertions are made on the **decoded JSON document** with EXACT
//!   counts (`assert_eq!(events.len(), N)` — never `>=`), EXACT
//!   ordering, and EXACT decoded values
//!   (`value["i"] == 42`, `value["kind"] == "Int"`).
//!
//! `ValueRecord` variants outside the expected set are rejected with
//! a hard error message asking the test author to extend the test
//! rather than weaken the assertion.
//!
//! Where the recorder's current behaviour deviates from what the
//! Move semantics dictate (single merged step event for the whole
//! function body; struct/vector values surface as `Raw` strings rather
//! than typed `Struct`/`Sequence` `ValueRecord` variants; etc.), the
//! deviation is documented inline as `RECORDER BUG: ...` and a parallel
//! `#[ignore]`-d assertion captures the spec-correct expectation so it
//! surfaces the moment the recorder catches up.
//!
//! Coverage matrix (universal checklist):
//!
//! | Category                | Test                                          |
//! |-------------------------|-----------------------------------------------|
//! | Control flow (if/while/loop/break/early-return) | `test_loops_*`, `test_fibonacci_*` |
//! | Function calls (≥3 deep)| `test_nested_calls_*`                         |
//! | Recursive / repeated    | `test_fibonacci_*` (5 calls, varied args)     |
//! | Tuple return            | `test_nested_calls_*` (compute_triple)        |
//! | Struct return           | `test_structs_*` (add_points -> Point)        |
//! | Void return             | every test_* (test entry returns Void)        |
//! | Abort / error path      | `test_abort_*` (asserts ioError event)        |
//! | Collections (vector)    | `test_vectors_*`                              |
//! | Collections (struct)    | `test_structs_*`                              |
//! | Generics                | `test_generics_*`                             |
//! | References (& / &mut)   | `test_references_*`                           |
//! | Wider integers + bool   | `test_boolean_and_integers_*`                 |
//!
//! Concurrency: Move has no concurrency primitives.  Documented skip.

use std::path::{Path, PathBuf};
use std::process::Command;

use codetracer_move_recorder::converter;
use codetracer_move_recorder::source_map::SourceMapResolver;

// ===========================================================================
// Helpers
// ===========================================================================

/// Path to the `ct-print` binary shipped with `codetracer-trace-format-nim`.
fn ct_print_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("codetracer-trace-format-nim")
        .join("ct-print")
}

/// Path to the `flow_test` Move source file.
fn flow_test_source() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test-programs/move/flow_test/sources/flow_test.move"
    ))
}

/// Path to a sibling Move source file under `test-programs/move/flow_test/sources/`.
/// Used by the M9 fixtures (variant_constructors, wide_integer, resources,
/// object_lifecycle, abilities) — each ships as a self-contained `.move`
/// source plus a synthetic NDJSON trace, so the converter sees a stable
/// `metadata.program` matching the source's stem.
fn flow_test_named_source(file_stem: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test-programs/move/flow_test/sources")
        .join(format!("{file_stem}.move"))
}

/// Path to a pre-captured NDJSON trace for the given `#[test]` function
/// inside the `flow_test::flow_test` Move module.
fn flow_test_trace_fixture(test_name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test-programs/move/flow_test/traces")
        .join(format!("flow_test__flow_test__{test_name}.json.zst"))
}

/// Skip-helper: returns `Some(path)` to ct-print or logs a clear
/// `SKIP:` diagnostic and returns `None`.  The
/// `verify-cli-convention-no-silent-skip.sh` script greps for the
/// literal `SKIP:` token, so silent skips remain forbidden.
fn ct_print_or_skip(test_name: &str) -> Option<PathBuf> {
    let p = ct_print_path();
    if !p.exists() {
        eprintln!(
            "SKIP: {test_name} requires ct-print at {} — only available \
             within the metacraft workspace where codetracer-trace-format-nim \
             is a sibling.",
            p.display()
        );
        return None;
    }
    Some(p)
}

/// Decompress a zstd-compressed trace fixture into raw NDJSON bytes.
fn read_decompressed_trace(zst_path: &Path) -> Vec<u8> {
    let zst_bytes = std::fs::read(zst_path)
        .unwrap_or_else(|e| panic!("failed to read trace fixture {}: {e}", zst_path.display()));
    let mut decoder = zstd::Decoder::new(zst_bytes.as_slice())
        .expect("zstd::Decoder::new should succeed on a valid .zst fixture");
    let mut decompressed = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decompressed)
        .expect("zstd decompression should succeed");
    decompressed
}

/// Convert one of the `flow_test` NDJSON fixtures through
/// `converter::convert_trace`, then run `ct-print --full --strip-paths`
/// on the produced `.ct` container and parse the resulting JSON.
///
/// Returns `None` only when `ct-print` is unavailable (after emitting a
/// `SKIP:` diagnostic via `ct_print_or_skip`).  Any other failure
/// (decompression, conversion, ct-print non-zero exit, JSON parse
/// failure) is a hard panic — it indicates a recorder regression, not
/// a missing dependency.
fn record_and_dump_full(test_name: &str, move_test: &str) -> Option<(serde_json::Value, PathBuf)> {
    record_and_dump_full_with_source(test_name, move_test, flow_test_source())
}

/// Variant of `record_and_dump_full` that lets the caller pin a specific
/// Move source file path (so `metadata.program` mirrors the source stem
/// and `paths` carries the right `.move` filename).  Used by the M9
/// fixtures whose sources live alongside `flow_test.move` in the same
/// `sources/` directory.
fn record_and_dump_full_with_source(
    test_name: &str,
    move_test: &str,
    source_path: PathBuf,
) -> Option<(serde_json::Value, PathBuf)> {
    let ct_print = ct_print_or_skip(test_name)?;

    let trace_zst = flow_test_trace_fixture(move_test);
    let trace_bytes = read_decompressed_trace(&trace_zst);

    let tmp_dir = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp_dir.path().join("ct-out");

    converter::convert_trace(
        &trace_bytes,
        &SourceMapResolver::empty(),
        &source_path,
        &out_dir,
    )
    .unwrap_or_else(|e| panic!("convert_trace failed for {move_test}: {e}"));

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

    let output = Command::new(&ct_print)
        .args(["--full", "--strip-paths"])
        .arg(&ct_files[0])
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn ct-print: {e}"));

    assert!(
        output.status.success(),
        "ct-print --full should succeed for {move_test}; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let doc: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("ct-print --full should emit valid JSON");

    drop(tmp_dir);
    Some((doc, source_path))
}

/// Decode the `call_entry` sequence as a vector of function names, in
/// emission order.
///
/// NOTE: the Move recorder emits `call_entry` events at `CloseFrame`
/// time (i.e. when the frame finishes), so the sequence is effectively
/// LIFO — innermost (first to close) first.  This is itself a recorder
/// oddity vs. spec ("entry_step" semantics imply call-time ordering),
/// but it is the present-day stable behaviour and the tests pin it
/// exactly so any future re-ordering is caught.
fn observed_call_sequence(doc: &serde_json::Value) -> Vec<String> {
    doc["events"]
        .as_array()
        .expect("events array")
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .map(|e| {
            e["function"]
                .as_str()
                .expect("call_entry.function str")
                .to_string()
        })
        .collect()
}

/// Decode the `call_exit` sequence as `(function, return_value)` pairs
/// in emission order.
fn observed_exit_sequence(doc: &serde_json::Value) -> Vec<(String, serde_json::Value)> {
    doc["events"]
        .as_array()
        .expect("events array")
        .iter()
        .filter(|e| e["kind"] == "call_exit")
        .map(|e| {
            (
                e["function"].as_str().expect("function str").to_string(),
                e["return_value"].clone(),
            )
        })
        .collect()
}

/// Assert that every `step` event carries a strictly non-decreasing
/// `step_index`.
fn assert_step_indices_monotonic(doc: &serde_json::Value) {
    let mut last = -1i64;
    for ev in doc["events"].as_array().expect("events array") {
        if ev["kind"] != "step" {
            continue;
        }
        let idx = ev["step_index"]
            .as_i64()
            .expect("step_index must be present on step events");
        assert!(
            idx > last,
            "step_index must strictly increase; got {idx} after {last}"
        );
        last = idx;
    }
}

/// Assert `metadata.program` matches the Move recorder's convention of
/// using the source file's basename without the `.move` extension.
fn assert_metadata_program_is(doc: &serde_json::Value, want: &str) {
    let prog = doc["metadata"]["program"]
        .as_str()
        .expect("metadata.program str");
    assert_eq!(
        prog, want,
        "metadata.program should be the source file's stem (no extension)",
    );
}

/// Assert the path table lists exactly the `flow_test.move` source.
fn assert_paths_contains_flow_test(doc: &serde_json::Value) {
    let paths: Vec<&str> = doc["paths"]
        .as_array()
        .expect("paths array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        paths.iter().any(|p| p.ends_with("flow_test.move")),
        "expected flow_test.move in paths table; got {paths:?}",
    );
}

/// Collect all (varname, value) pairs from the merged `step` event in
/// emission order.  Asserts that every value's `kind` is one of the
/// expected set.  An unexpected kind is a hard error so the test
/// author is forced to extend the test rather than weaken it.
fn collect_step_vars(
    doc: &serde_json::Value,
    allowed_kinds: &[&str],
) -> Vec<(String, serde_json::Value)> {
    let mut out = Vec::new();
    for ev in doc["events"].as_array().expect("events array") {
        if ev["kind"] != "step" {
            continue;
        }
        let Some(vars) = ev["vars"].as_array() else {
            continue;
        };
        for v in vars {
            let name = v["varname"].as_str().expect("varname").to_string();
            let value = &v["value"];
            let kind = value["kind"].as_str().expect("value.kind");
            assert!(
                allowed_kinds.contains(&kind),
                "variable `{name}` has unexpected ValueRecord kind `{kind}` \
                 (allowed = {allowed_kinds:?}); if a new variant has landed, \
                 extend this test to assert on it explicitly rather than \
                 weakening the check.  Full value: {value}",
            );
            out.push((name, value.clone()));
        }
    }
    out
}

/// Returns the unique set of `(varname, decoded_int)` pairs surfacing
/// in the merged step event (deduplicated, order-preserving).  Useful
/// for asserting "this Int value, attached to this name, was observed
/// at least once during the function".
///
/// Note: as the converter learned to emit typed compound values
/// (`Sequence` / `Struct` / `Tuple` for Move vectors / structs / tuple
/// returns) we expanded the allowed-kinds list passed to
/// [`collect_step_vars`] so nested compound payloads do not trip the
/// "unexpected ValueRecord kind" hard error.  This helper still only
/// returns scalar `Int` leaves — compound walks belong to the test
/// using them.
fn unique_int_pairs(doc: &serde_json::Value) -> Vec<(String, i64)> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for (name, value) in collect_step_vars(
        doc,
        &[
            "BigInt",
            "Bool",
            "Int",
            "Raw",
            "Reference",
            "String",
            "Sequence",
            "Struct",
            "Tuple",
            "Variant",
        ],
    ) {
        if value["kind"] == "Int" {
            let i = value["i"].as_i64().expect("Int.i");
            if seen.insert((name.clone(), i)) {
                out.push((name, i));
            }
        }
    }
    out
}

/// Returns the unique set of (varname, printed_repr) pairs for `Raw`-,
/// `String`-, or `Bool`-kind values in the merged step event.
///
/// The Move recorder emits printed-form values in three shapes depending
/// on the underlying VM value:
///   * struct / address / vector text → `ValueRecord::String` (kind="String", text=...)
///   * boolean predicates             → `ValueRecord::Bool`   (kind="Bool",   text="true"|"false")
///   * pre-fix everything else        → `ValueRecord::Raw`    (kind="Raw",    r=...)
///
/// Prior to the `codetracer_trace_writer_nim::register_variable_with_full_value`
/// fix the FFI wrapper flattened all three into `ValueRecord::Raw`, so
/// pre-existing tests used `r` for everything. The recorder's *intent*
/// (a printed scalar / boolean text) is unchanged, so this helper
/// coalesces the variants — picking `text` from String/Bool and `r`
/// from Raw. Per-test assertions now exercise the typed shape directly
/// where possible (e.g. asserting `kind=="Bool"` and `b==true`).
fn unique_raw_pairs(doc: &serde_json::Value) -> Vec<(String, String)> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for (name, value) in collect_step_vars(
        doc,
        &[
            "BigInt",
            "Bool",
            "Int",
            "Raw",
            "Reference",
            "String",
            "Sequence",
            "Struct",
            "Tuple",
            "Variant",
        ],
    ) {
        let payload = match value["kind"].as_str() {
            Some("Raw") => value["r"].as_str().map(|s| s.to_string()),
            Some("String") => value["text"].as_str().map(|s| s.to_string()),
            Some("Bool") => value["text"].as_str().map(|s| s.to_string()),
            _ => None,
        };
        if let Some(r) = payload {
            if seen.insert((name.clone(), r.clone())) {
                out.push((name, r));
            }
        }
    }
    out
}

/// Returns the unique set of (varname, bool_value) pairs for `Bool`-kind
/// values in the merged step event. Allows asserting on the strongest
/// typed shape — kind="Bool", b=true/false, text="true"/"false" — rather
/// than the historical Raw-coerced stringification.
/// Collect every `kind:"Sequence"` value's element-int list (only
/// pulling sequences whose elements are all `Int` leaves) from the
/// merged step's vars.  Used by `test_vectors_*` to assert that each
/// successive `vector::push_back(_, N)` shape surfaces as a typed
/// Sequence carrying the expected children, rather than as a printed
/// `Raw` / `String` payload.
fn collect_sequence_int_lists(doc: &serde_json::Value) -> Vec<Vec<i64>> {
    let mut out = Vec::new();
    for (_, value) in collect_step_vars(
        doc,
        &[
            "BigInt",
            "Bool",
            "Int",
            "Raw",
            "Reference",
            "String",
            "Sequence",
            "Struct",
            "Tuple",
            "Variant",
        ],
    ) {
        if value["kind"] != "Sequence" {
            continue;
        }
        let Some(elems) = value["elements"].as_array() else {
            continue;
        };
        let mut ints = Vec::with_capacity(elems.len());
        let mut all_ints = true;
        for e in elems {
            if e["kind"] == "Int" {
                ints.push(e["i"].as_i64().expect("Int.i"));
            } else {
                all_ints = false;
                break;
            }
        }
        if all_ints {
            out.push(ints);
        }
    }
    out
}

/// Collect every `kind:"Struct"` value's field-Int list (only pulling
/// structs whose fields are all `Int` leaves) from the merged step's
/// vars.  Used by `test_structs_*` to assert that each successive
/// `Point { x, y }` / `Wallet { balance, id }` shape surfaces as a
/// typed Struct carrying the expected children, rather than as a
/// printed `Raw` / `String` payload.  Nested structs (Rectangle whose
/// first field is a Point) are skipped — the test asserts on the
/// inner Point and Wallet shapes directly.
fn collect_struct_int_lists(doc: &serde_json::Value) -> Vec<Vec<i64>> {
    let mut out = Vec::new();
    for (_, value) in collect_step_vars(
        doc,
        &[
            "BigInt",
            "Bool",
            "Int",
            "Raw",
            "Reference",
            "String",
            "Sequence",
            "Struct",
            "Tuple",
            "Variant",
        ],
    ) {
        if value["kind"] != "Struct" {
            continue;
        }
        let Some(fields) = value["field_values"].as_array() else {
            continue;
        };
        let mut ints = Vec::with_capacity(fields.len());
        let mut all_ints = true;
        for f in fields {
            if f["kind"] == "Int" {
                ints.push(f["i"].as_i64().expect("Int.i"));
            } else {
                all_ints = false;
                break;
            }
        }
        if all_ints {
            out.push(ints);
        }
    }
    out
}

fn unique_bool_pairs(doc: &serde_json::Value) -> Vec<(String, bool)> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for (name, value) in collect_step_vars(
        doc,
        &[
            "BigInt",
            "Bool",
            "Int",
            "Raw",
            "Reference",
            "String",
            "Sequence",
            "Struct",
            "Tuple",
            "Variant",
        ],
    ) {
        if value["kind"] == "Bool" {
            let b = value["b"].as_bool().expect("Bool.b");
            // Spec invariant from streaming_value_encoder.writeBool: the
            // text field is always the lower-case stringification.
            let text = value["text"].as_str().expect("Bool.text");
            assert_eq!(
                text,
                if b { "true" } else { "false" },
                "Bool ValueRecord.text must mirror b; got value={value}"
            );
            if seen.insert((name.clone(), b)) {
                out.push((name, b));
            }
        }
    }
    out
}

// ===========================================================================
// test_loops — control flow: while + loop/break + if/else
// ===========================================================================

/// Records `flow_test::test_loops` and asserts the recorder pinned the
/// canonical loop terminal values (`counter==10`, `accumulator==55`,
/// `power==128`, `iterations==7`, `grade==1`).
///
/// RECORDER BUG: the converter merges every Move VM stack push/pop
/// into a single `step` event with hundreds of `vars` entries, so we
/// can only assert "value V was observed for varname N at least once
/// during the function" rather than the spec-required "value V was
/// the binding for N at line L".  See `test_loops_one_step_per_source_line`
/// for the spec-correct pin (currently `#[ignore]`d).
#[test]
fn test_loops_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full("test_loops_via_ct_print_full", "test_loops") else {
        return;
    };

    assert_metadata_program_is(&doc, "flow_test");
    assert_paths_contains_flow_test(&doc);

    // ----- Function table --------------------------------------------------
    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(functions, vec!["test_loops"]);

    // ----- counts (recorder-internal: one merged step / call) -------------
    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1), "steps; counts={counts}");
    assert_eq!(counts["calls"].as_u64(), Some(1), "calls; counts={counts}");
    assert_eq!(
        counts["io_events"].as_u64(),
        Some(0),
        "io_events; counts={counts}",
    );

    // ----- events: 1 call_entry + 1 step + 1 call_exit = 3 ----------------
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 3, "events.len()");
    assert_step_indices_monotonic(&doc);

    assert_eq!(observed_call_sequence(&doc), vec!["test_loops".to_string()]);
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].0, "test_loops");
    assert_eq!(exits[0].1["kind"].as_str(), Some("Void"));

    // ----- The canonical loop terminal values must surface ----------------
    // While loop reaches counter=10, accumulator=55.  loop/break finds
    // power=128 after iterations=7.  if/else picks grade=1.
    //
    // All of these surface as `Int` ValueRecords on the merged step's
    // `vars` array (under the synthetic `stack_top` / `popped` /
    // `local_*` names the Move recorder uses for stack slots and
    // function-frame locals).
    let ints = unique_int_pairs(&doc);
    let int_set: std::collections::BTreeSet<(String, i64)> = ints.iter().cloned().collect();

    // The Move converter assigns synthetic names `local_<idx>` to
    // every function-frame local in slot order.  For test_loops the
    // mapping is (pinned by inspection of `locals_types` in the raw
    // NDJSON):
    //   local_0 -> grade           (assigned last; reaches 1)
    //   local_1 -> accumulator     (reaches 55)
    //   local_2 -> counter         (reaches 10)
    //   local_3 -> iterations      (reaches 7)
    //   local_4 -> power           (reaches 128)
    //
    // RECORDER BUG: those synthetic `local_<N>` names are opaque to a
    // human reader of the trace.  Spec-compliant output should resolve
    // them to source-level identifiers (`counter`, `accumulator`,
    // `power`, `iterations`, `grade`) using the Move debug info.
    let must_observe: &[(&str, i64)] = &[
        // grade = 1
        ("local_0", 1),
        // accumulator = 55
        ("local_1", 55),
        // counter = 10
        ("local_2", 10),
        // iterations = 7
        ("local_3", 7),
        // power = 128
        ("local_4", 128),
    ];
    for (n, v) in must_observe {
        assert!(
            int_set.contains(&((*n).to_string(), *v)),
            "expected `{n}` = {v} in test_loops merged step; observed = {ints:?}"
        );
    }

    // ----- Boolean conditional branch outcomes -----------------------------
    // Every `if` / `while` predicate evaluation pushes a Move bool which
    // the recorder now serialises as a typed `ValueRecord::Bool`
    // (kind="Bool", b=true|false, text="true"|"false") — previously the
    // FFI wrapper flattened these into Raw `"true"`/`"false"` strings.
    // We keep a Raw/String/Bool-coalescing helper (`unique_raw_pairs`)
    // for backward-compatible printed-form assertions and ALSO assert
    // on the typed Bool shape directly so any future regression toward
    // Raw is loud.
    let raws = unique_raw_pairs(&doc);
    assert!(
        raws.iter().any(|(_, r)| r == "true"),
        "expected at least one `true` printed-form value (loop predicates); got {raws:?}",
    );
    assert!(
        raws.iter().any(|(_, r)| r == "false"),
        "expected at least one `false` printed-form value (loop terminator); got {raws:?}",
    );
    let bools = unique_bool_pairs(&doc);
    assert!(
        bools.iter().any(|(_, b)| *b),
        "expected at least one typed `Bool {{b:true,text:\"true\"}}` value (loop predicates); got {bools:?}",
    );
    assert!(
        bools.iter().any(|(_, b)| !*b),
        "expected at least one typed `Bool {{b:false,text:\"false\"}}` value (loop terminator); got {bools:?}",
    );
}

/// Spec-correct expectation: each iteration of a loop body emits one
/// step event at the corresponding source line.  See the spec at
/// `metacraft-specs/policies/recorder-test-requirements.md`:
///
/// > A `for i in 0..10` loop must produce exactly 10 step events at
/// > the loop body.
///
/// This test feeds the converter a synthetic NDJSON trace that models
/// a 10-iteration loop body in bytecode (each iteration is one
/// "loop-body" Instruction at pc=2 followed by a backward branch to
/// pc=2 for the next iteration).  The accompanying source map maps
/// every body pc to source-line 6, so a recorder that merely deduped
/// "consecutive same-line" instructions would collapse all ten
/// iterations into a single step.  The spec-compliant recorder
/// detects the backward `pc` deltas (`prev_pc > current_pc`) and
/// force-emits a step at each iteration boundary even when the line
/// is unchanged — yielding exactly 10 body-line steps.
///
/// The integration `test_loops_via_ct_print_full` test cannot exercise
/// this directly because the .mvsm-driven `SourceMapResolver` is a
/// follow-up and the recorded fixture has no live source map; this
/// synthetic test is the canonical pin for the per-source-line
/// invariant until then.
#[test]
fn test_loops_one_step_per_source_line() {
    use codetracer_trace_types::TraceLowLevelEvent;
    use codetracer_trace_writer_nim::non_streaming_trace_writer::NonStreamingTraceWriter;

    // Source map: pc=0 (preamble) -> line 5, pc=1 (loop guard) -> line 5,
    //             pc=2 (body)     -> line 6, pc=3 (postamble) -> line 7.
    let source_map = SourceMapResolver::from_entries(vec![
        ("loops".to_string(), 0, "loops.move".to_string(), 5),
        ("loops".to_string(), 1, "loops.move".to_string(), 5),
        ("loops".to_string(), 2, "loops.move".to_string(), 6),
        ("loops".to_string(), 3, "loops.move".to_string(), 7),
    ]);

    // Build a 10-iter loop:
    //   pc=0 (preamble), pc=1 (guard), { pc=2 (body), pc=1 (guard) }*10, pc=3 (postamble)
    // The pc=1 guard re-entries are backward jumps relative to the
    // immediately-preceding pc=2 body instruction, and likewise pc=2
    // body re-entries are forward but follow a pc=1 guard which sits
    // on the same source line as pc=0 — so without backward-jump
    // detection the body line would dedup to a single step.
    let mut lines: Vec<String> = vec![
        r#"{"version":3}"#.to_string(),
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"ten_iter_loop","module":{"address":"0x0","name":"loops"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[],"is_native":false},"gas_left":1000000}}"#.to_string(),
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":999999,"instruction":"Nop"}}"#.to_string(),
    ];
    for i in 0..10 {
        let g = 999_998 - 2 * i;
        let b = g - 1;
        lines.push(format!(
            r#"{{"Instruction":{{"type_parameters":[],"pc":1,"gas_left":{g},"instruction":"Lt"}}}}"#
        ));
        lines.push(format!(
            r#"{{"Instruction":{{"type_parameters":[],"pc":2,"gas_left":{b},"instruction":"Nop"}}}}"#
        ));
    }
    lines.push(
        r#"{"Instruction":{"type_parameters":[],"pc":3,"gas_left":999000,"instruction":"Ret"}}"#
            .to_string(),
    );
    lines.push(r#"{"CloseFrame":{"frame_id":1,"return_":[],"gas_left":998999}}"#.to_string());
    let ndjson = lines.join("\n");

    let source_path = std::path::Path::new("loops.move");
    let mut writer = NonStreamingTraceWriter::new("loops.move", &[]);
    converter::convert_trace_into_writer(ndjson.as_bytes(), &source_map, source_path, &mut writer)
        .expect("convert_trace_into_writer should succeed");

    let body_steps = writer
        .events
        .iter()
        .filter(|e| matches!(e, TraceLowLevelEvent::Step(s) if s.line.0 == 6))
        .count();
    assert_eq!(
        body_steps, 10,
        "expected exactly 10 step events on the loop body line (line 6); \
         backward-jump detection must force-emit a step at every loop \
         iteration boundary even when the resolved source line matches \
         the previous step.  observed body_steps={body_steps}; events={:?}",
        writer.events
    );
}

// ===========================================================================
// test_nested_calls — function calls ≥3 deep + tuple return
// ===========================================================================

/// Records `flow_test::test_nested_calls`.  The Move source calls
/// `compute_triple(12, 8)` which internally calls `max_u64(12, 8)`,
/// then `min_u64` twice and `max_u64` once more on the result.  Five
/// helper-function invocations plus the test entry = 6 frames.
///
/// RECORDER BUG: `call_entry` events appear in *close-frame* order
/// (innermost / first-to-close first), not call order.  This pins the
/// present-day order so any future re-ordering is caught.  A spec-
/// correct recorder would emit `call_entry` at OpenFrame time so the
/// outermost call appears first.
#[test]
fn test_nested_calls_via_ct_print_full() {
    let Some((doc, _)) =
        record_and_dump_full("test_nested_calls_via_ct_print_full", "test_nested_calls")
    else {
        return;
    };

    assert_metadata_program_is(&doc, "flow_test");
    assert_paths_contains_flow_test(&doc);

    // ----- Function table — order is writer-assignment order --------------
    // The recorder registers function names in close-frame order.
    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        functions,
        vec!["test_nested_calls", "compute_triple", "max_u64", "min_u64"],
        "function table order should be writer-assignment order; \
         change here means the converter changed function-registration timing"
    );

    // ----- counts -----------------------------------------------------------
    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1), "steps; counts={counts}");
    assert_eq!(counts["calls"].as_u64(), Some(6), "calls; counts={counts}");
    assert_eq!(
        counts["io_events"].as_u64(),
        Some(0),
        "io_events; counts={counts}",
    );

    // 1 step + 6 call_entry + 6 call_exit = 13 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 13, "events.len()");
    assert_step_indices_monotonic(&doc);

    // ----- Call-entry sequence (close-frame order) ------------------------
    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            // max_u64(12, 8) inside compute_triple
            "max_u64".to_string(),
            // compute_triple(12, 8) closes after its inner max_u64
            "compute_triple".to_string(),
            // min_u64(x, y) = min_u64(12, 8)
            "min_u64".to_string(),
            // min_u64(15, 20)
            "min_u64".to_string(),
            // outer max_u64 over the two mins
            "max_u64".to_string(),
            // test_nested_calls itself
            "test_nested_calls".to_string(),
        ],
        "call_entry sequence pins the recorder's close-frame ordering"
    );

    // ----- Return values (Int + Tuple + Void) -----------------------------
    // `compute_triple` returns the tuple `(20, 96, 12)` — three u64
    // values.  The recorder now surfaces the *full* tuple as a typed
    // `ValueRecord::Tuple` carrying three `Int` elements, rather than
    // silently truncating to the first element.  See
    // `test_nested_calls_tuple_return_decodes_full_tuple` for the
    // dedicated typed-shape pin.
    let exits = observed_exit_sequence(&doc);
    let exit_pairs: Vec<(String, Option<i64>)> = exits
        .iter()
        .map(|(f, rv)| {
            let kind = rv["kind"].as_str().unwrap_or("");
            let i = if kind == "Int" {
                Some(rv["i"].as_i64().expect("Int.i"))
            } else {
                None
            };
            (f.clone(), i)
        })
        .collect();
    assert_eq!(
        exit_pairs,
        vec![
            ("max_u64".to_string(), Some(12)),
            // compute_triple's return is a Tuple (not an Int), so the
            // shorthand `Option<i64>` projector reports `None` here —
            // the full Tuple shape is asserted explicitly below.
            ("compute_triple".to_string(), None),
            ("min_u64".to_string(), Some(8)),
            ("min_u64".to_string(), Some(15)),
            ("max_u64".to_string(), Some(15)),
            ("test_nested_calls".to_string(), None), // Void
        ]
    );

    // Strict tuple-shape assertion: kind=Tuple, three Int elements
    // [20, 96, 12].  Pinned exactly so any future regression toward
    // truncation / re-shaping shows up here.
    let compute_triple_rv = &exits[1].1;
    assert_eq!(compute_triple_rv["kind"].as_str(), Some("Tuple"));
    let tuple_elems = compute_triple_rv["elements"]
        .as_array()
        .expect("Tuple.elements");
    assert_eq!(tuple_elems.len(), 3);
    assert_eq!(tuple_elems[0]["kind"].as_str(), Some("Int"));
    assert_eq!(tuple_elems[0]["i"].as_i64(), Some(20));
    assert_eq!(tuple_elems[1]["kind"].as_str(), Some("Int"));
    assert_eq!(tuple_elems[1]["i"].as_i64(), Some(96));
    assert_eq!(tuple_elems[2]["kind"].as_str(), Some("Int"));
    assert_eq!(tuple_elems[2]["i"].as_i64(), Some(12));

    // ----- Argument decoding on call_entry --------------------------------
    // The recorder is supposed to decode each call's args.  Pin the
    // arg values for every helper invocation in order.
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let arg_ints = |idx: usize| -> Vec<i64> {
        entries[idx]["args"]
            .as_array()
            .expect("args array")
            .iter()
            .map(|a| a["value"]["i"].as_i64().expect("arg Int.i"))
            .collect()
    };
    assert_eq!(
        arg_ints(0),
        vec![12, 8],
        "max_u64(12,8) inside compute_triple"
    );
    assert_eq!(arg_ints(1), vec![12, 8], "compute_triple(12,8)");
    assert_eq!(arg_ints(2), vec![12, 8], "first min_u64(12,8)");
    assert_eq!(arg_ints(3), vec![15, 20], "second min_u64(15,20)");
    assert_eq!(
        arg_ints(4),
        vec![8, 15],
        "outer max_u64(min_u64(12,8), min_u64(15,20))"
    );
    assert!(
        entries[5]["args"].as_array().unwrap().is_empty(),
        "test_nested_calls itself takes no args"
    );

    // ----- The scaled product == 9600 must surface ------------------------
    let ints = unique_int_pairs(&doc);
    let int_set: std::collections::BTreeSet<(String, i64)> = ints.into_iter().collect();
    assert!(
        int_set.iter().any(|(_, v)| *v == 9600),
        "expected scaled (product * SCALE_FACTOR) = 9600 in vars; got values {:?}",
        int_set.iter().map(|(_, v)| v).collect::<Vec<_>>(),
    );
}

#[test]
fn test_nested_calls_tuple_return_decodes_full_tuple() {
    let Some((doc, _)) = record_and_dump_full(
        "test_nested_calls_tuple_return_decodes_full_tuple",
        "test_nested_calls",
    ) else {
        return;
    };
    let exits = observed_exit_sequence(&doc);
    let compute_triple = exits
        .iter()
        .find(|(f, _)| f == "compute_triple")
        .expect("compute_triple should appear in exit sequence");
    // Spec-correct: the tuple decodes as a Tuple ValueRecord with three
    // Int elements [20, 96, 12].  Sequence is also accepted because a
    // future converter could reasonably model variadic returns as a
    // typed sequence — both shapes preserve all three elements.
    let kind = compute_triple.1["kind"].as_str().unwrap_or("");
    assert!(
        kind == "Tuple" || kind == "Sequence",
        "expected Tuple/Sequence ValueRecord for compute_triple's tuple \
         return; got kind={kind} (full = {})",
        compute_triple.1
    );
    let elems = compute_triple.1["elements"]
        .as_array()
        .expect("Tuple/Sequence.elements");
    let ints: Vec<i64> = elems
        .iter()
        .map(|e| e["i"].as_i64().expect("element Int.i"))
        .collect();
    assert_eq!(
        ints,
        vec![20, 96, 12],
        "compute_triple's full tuple return must surface as [20, 96, 12]"
    );
}

// ===========================================================================
// test_vectors — vector<u64> push/pop/borrow/length/sum
// ===========================================================================

/// Records `flow_test::test_vectors`.  The Move source pushes
/// `[10, 20, 30, 40, 50]`, computes `vector::length` (5),
/// `vector::borrow` at indices 0 and 4 (10, 50), calls
/// `vector_sum` (150), pops 50, and verifies new length is 4.
///
/// Vector values surface as typed `ValueRecord::Sequence` payloads
/// carrying recursively-converted children — the test pins each
/// expected element list shape so any future regression toward
/// printed-form `Raw`/`String` (the historical fallback) is caught.
/// See also `test_vectors_uses_sequence_value_record` for the dedicated
/// kind-presence pin.
#[test]
fn test_vectors_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full("test_vectors_via_ct_print_full", "test_vectors")
    else {
        return;
    };

    assert_metadata_program_is(&doc, "flow_test");
    assert_paths_contains_flow_test(&doc);

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(functions, vec!["test_vectors", "vector_sum"]);

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(2));
    assert_eq!(counts["io_events"].as_u64(), Some(0));

    // 1 step + 2 call_entry + 2 call_exit = 5 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 5);
    assert_step_indices_monotonic(&doc);

    assert_eq!(
        observed_call_sequence(&doc),
        vec!["vector_sum".to_string(), "test_vectors".to_string()]
    );

    // ----- Return values --------------------------------------------------
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 2);
    assert_eq!(exits[0].0, "vector_sum");
    assert_eq!(exits[0].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[0].1["i"].as_i64(), Some(150), "vector_sum(v) == 150");
    assert_eq!(exits[1].0, "test_vectors");
    assert_eq!(exits[1].1["kind"].as_str(), Some("Void"));

    // ----- Vector contents must surface as typed Sequence values ----------
    // Walk the merged step's vars and collect each Sequence's element-int
    // list.  We expect every vector growth shape to appear as a typed
    // `kind:"Sequence"` payload with `Int`-leaf elements.
    let seq_int_lists = collect_sequence_int_lists(&doc);
    for want in [
        Vec::<i64>::new(),
        vec![10],
        vec![10, 20],
        vec![10, 20, 30],
        vec![10, 20, 30, 40],
        vec![10, 20, 30, 40, 50],
    ] {
        assert!(
            seq_int_lists.contains(&want),
            "expected vector contents `{want:?}` as a typed Sequence ValueRecord; \
             got Sequence shapes = {seq_int_lists:?}"
        );
    }

    // ----- Scalar Move semantics: len==5, len-after-pop==4, sum==150 ------
    let int_set: std::collections::BTreeSet<(String, i64)> =
        unique_int_pairs(&doc).into_iter().collect();
    let just_values: std::collections::BTreeSet<i64> = int_set.iter().map(|(_, v)| *v).collect();
    for want in [4, 5, 10, 50, 150] {
        assert!(
            just_values.contains(&want),
            "expected int value {want} (len/borrow/sum); got {just_values:?}"
        );
    }
}

#[test]
fn test_vectors_uses_sequence_value_record() {
    let Some((doc, _)) =
        record_and_dump_full("test_vectors_uses_sequence_value_record", "test_vectors")
    else {
        return;
    };
    let mut kinds = std::collections::BTreeSet::new();
    for ev in doc["events"].as_array().unwrap() {
        if ev["kind"] != "step" {
            continue;
        }
        for v in ev["vars"].as_array().cloned().unwrap_or_default() {
            if let Some(k) = v["value"]["kind"].as_str() {
                kinds.insert(k.to_string());
            }
        }
    }
    assert!(
        kinds.contains("Sequence"),
        "expected Sequence ValueRecord for vector contents; got {kinds:?}"
    );
    // Strict shape check: at least one Sequence whose elements are the
    // canonical [10, 20, 30, 40, 50] list pushed by `test_vectors`.
    let seq_lists = collect_sequence_int_lists(&doc);
    assert!(
        seq_lists.contains(&vec![10, 20, 30, 40, 50]),
        "expected the [10, 20, 30, 40, 50] vector contents as a typed \
         Sequence; got Sequence shapes = {seq_lists:?}"
    );
}

// ===========================================================================
// test_structs — struct creation + field access + destructuring
// ===========================================================================

/// Records `flow_test::test_structs`.  Constructs `Point { x: 3, y: 4 }`,
/// `Point { x: 7, y: 6 }`, sums them via `add_points` (returning
/// `Point { x: 10, y: 10 }`), builds a `Rectangle`, computes its area
/// (40), destructures the sum into `(px, py)`, builds a `Wallet`.
///
/// Struct values surface as typed `ValueRecord::Struct` payloads
/// carrying recursively-converted field children — the test pins the
/// observed Point shapes so any future regression toward the
/// historical `Raw`/`String` printed-form fallback is caught.  See
/// also `test_structs_uses_struct_value_record` for the dedicated
/// kind-presence pin.
#[test]
fn test_structs_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full("test_structs_via_ct_print_full", "test_structs")
    else {
        return;
    };

    assert_metadata_program_is(&doc, "flow_test");
    assert_paths_contains_flow_test(&doc);

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        functions,
        vec!["test_structs", "add_points", "rectangle_area"]
    );

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(3));
    assert_eq!(counts["io_events"].as_u64(), Some(0));

    // 1 step + 3 call_entry + 3 call_exit = 7 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 7);
    assert_step_indices_monotonic(&doc);

    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "add_points".to_string(),
            "rectangle_area".to_string(),
            "test_structs".to_string(),
        ]
    );

    // ----- Return values: add_points -> Struct(Point), area=40 -----------
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits[0].0, "add_points");
    // The Point struct return now surfaces as a typed
    // `ValueRecord::Struct` carrying two `Int` fields [x=10, y=10].
    assert_eq!(exits[0].1["kind"].as_str(), Some("Struct"));
    let p_fields = exits[0].1["field_values"]
        .as_array()
        .expect("Struct.field_values");
    assert_eq!(p_fields.len(), 2, "Point has two fields (x, y)");
    assert_eq!(p_fields[0]["kind"].as_str(), Some("Int"));
    assert_eq!(p_fields[0]["i"].as_i64(), Some(10), "Point.x == 10");
    assert_eq!(p_fields[1]["kind"].as_str(), Some("Int"));
    assert_eq!(p_fields[1]["i"].as_i64(), Some(10), "Point.y == 10");
    assert_eq!(exits[1].0, "rectangle_area");
    assert_eq!(exits[1].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[1].1["i"].as_i64(), Some(40));
    assert_eq!(exits[2].0, "test_structs");
    assert_eq!(exits[2].1["kind"].as_str(), Some("Void"));

    // ----- Every observed Point/Wallet shape surfaces as Struct -----------
    // Walk the merged step's vars and collect each Struct's flattened
    // Int-field list.  The test pins the canonical (x, y) Point shapes
    // built by the source program plus the Wallet (balance, id) shape.
    let struct_int_lists = collect_struct_int_lists(&doc);
    for want in [
        vec![3_i64, 4],
        vec![7, 6],
        vec![10, 10],
        // Wallet { balance: 1000, id: 1 }
        vec![1000, 1],
    ] {
        assert!(
            struct_int_lists.contains(&want),
            "expected Struct field-Int shape `{want:?}` in vars; got Struct shapes = {struct_int_lists:?}"
        );
    }

    // ----- Scalar field-access values (px=10, py=10, sum_coords=20, area=40)
    let ints: std::collections::BTreeSet<i64> =
        unique_int_pairs(&doc).into_iter().map(|(_, v)| v).collect();
    for want in [10, 20, 40, 1000] {
        assert!(
            ints.contains(&want),
            "expected scalar value {want} (px/py/sum_coords/area/balance) in vars; got {ints:?}"
        );
    }
}

#[test]
fn test_structs_uses_struct_value_record() {
    let Some((doc, _)) =
        record_and_dump_full("test_structs_uses_struct_value_record", "test_structs")
    else {
        return;
    };
    let mut kinds = std::collections::BTreeSet::new();
    for ev in doc["events"].as_array().unwrap() {
        if ev["kind"] != "step" {
            continue;
        }
        for v in ev["vars"].as_array().cloned().unwrap_or_default() {
            if let Some(k) = v["value"]["kind"].as_str() {
                kinds.insert(k.to_string());
            }
        }
    }
    assert!(
        kinds.contains("Struct"),
        "expected Struct ValueRecord for Point/Rectangle/Wallet construction; got {kinds:?}"
    );
    // Strict shape check: at least one Struct whose fields are the
    // canonical Point { x: 3, y: 4 } shape.
    let struct_lists = collect_struct_int_lists(&doc);
    assert!(
        struct_lists.contains(&vec![3, 4]),
        "expected Point {{ x: 3, y: 4 }} as a typed Struct with Int field \
         values; got Struct shapes = {struct_lists:?}"
    );
}

// ===========================================================================
// test_references — & and &mut value reads + mutation through &mut
// ===========================================================================

/// Records `flow_test::test_references`.  Builds
/// `Point { x: 2, y: 3 }`, calls `scale_point(&mut p, 5)` to mutate
/// it to `Point { x: 10, y: 15 }`, reads `(read_x, read_y)` through an
/// `&p` ref, then calls `scale_point(&mut p, 3)` to reach `Point { x: 30, y: 45 }`.
#[test]
fn test_references_via_ct_print_full() {
    let Some((doc, _)) =
        record_and_dump_full("test_references_via_ct_print_full", "test_references")
    else {
        return;
    };

    assert_metadata_program_is(&doc, "flow_test");
    assert_paths_contains_flow_test(&doc);

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(functions, vec!["test_references", "scale_point"]);

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(3));
    assert_eq!(counts["io_events"].as_u64(), Some(0));

    // 1 step + 3 call_entry + 3 call_exit = 7 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 7);
    assert_step_indices_monotonic(&doc);

    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "scale_point".to_string(),
            "scale_point".to_string(),
            "test_references".to_string(),
        ]
    );

    let exits = observed_exit_sequence(&doc);
    // Both scale_point calls return Void (mutate-through-ref).
    assert_eq!(exits[0].0, "scale_point");
    assert_eq!(exits[0].1["kind"].as_str(), Some("Void"));
    assert_eq!(exits[1].0, "scale_point");
    assert_eq!(exits[1].1["kind"].as_str(), Some("Void"));
    assert_eq!(exits[2].0, "test_references");
    assert_eq!(exits[2].1["kind"].as_str(), Some("Void"));

    // ----- &mut Point arg: surfaces as a typed Reference wrapping a Struct
    // The Move recorder now preserves the borrow wrapper around `&mut T`
    // / `&T` parameters, surfacing them as `ValueRecord::Reference` with
    // a `mutable` flag and a `dereferenced` Struct payload carrying the
    // pointee shape.  Both calls to `scale_point(&mut p, _)` borrow the
    // same `p` (frame-local index 0 in `test_references`'s frame), so
    // `address` is stable across the two calls and `mutable == true`.
    // See `test_references_use_typed_reference_value_record` for the
    // dedicated kind-presence pin.
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let scale_args = |idx: usize| -> (String, bool, Option<Vec<i64>>, Option<i64>) {
        let args = entries[idx]["args"].as_array().expect("args array");
        assert_eq!(args.len(), 2, "scale_point takes (&mut Point, u64)");
        let arg0 = &args[0]["value"];
        let arg1 = &args[1]["value"];
        let kind0 = arg0["kind"].as_str().expect("kind").to_string();
        let mutable0 = arg0["mutable"].as_bool().unwrap_or(false);
        let deref0 = &arg0["dereferenced"];
        let xy0 = deref0["field_values"].as_array().map(|fields| {
            fields
                .iter()
                .map(|f| f["i"].as_i64().expect("Int.i"))
                .collect::<Vec<_>>()
        });
        let i1 = arg1["i"].as_i64();
        (kind0, mutable0, xy0, i1)
    };
    let (k0, m0, xy0, i0) = scale_args(0);
    assert_eq!(
        k0, "Reference",
        "scale_point's &mut Point arg surfaces as a typed Reference wrapper"
    );
    assert!(m0, "scale_point's first arg is `&mut Point`, not `&Point`");
    assert_eq!(
        xy0.as_deref(),
        Some(&[2_i64, 3][..]),
        "Point {{ x: 2, y: 3 }}"
    );
    assert_eq!(i0, Some(5), "scale_point's factor arg = 5");
    let (k1, m1, xy1, i1) = scale_args(1);
    assert_eq!(k1, "Reference");
    assert!(m1);
    assert_eq!(
        xy1.as_deref(),
        Some(&[10_i64, 15][..]),
        "Point {{ x: 10, y: 15 }} after first scale_point"
    );
    assert_eq!(i1, Some(3), "scale_point's second factor arg = 3");

    // Both `scale_point(&mut mut_point, _)` calls borrow the same Move
    // local, so the synthesised reference address must be stable across
    // call_entry events — verify so a future reshape that loses
    // borrow-identity (e.g. zeroing the address) is caught here.
    let address0 = entries[0]["args"][0]["value"]["address"].as_u64();
    let address1 = entries[1]["args"][0]["value"]["address"].as_u64();
    assert!(
        address0.is_some(),
        "Reference must carry a synthetic address"
    );
    assert_eq!(
        address0, address1,
        "both scale_point calls borrow the same `mut_point`; addresses should match"
    );

    // ----- All Point shapes surface as typed Structs (incl. mutated copies)
    // The Move source threads a single Point through `scale_point(&mut, _)`
    // so we should observe `(2, 3)`, `(10, 15)`, and `(30, 45)` Struct
    // shapes among the merged step's vars.
    let struct_lists = collect_struct_int_lists(&doc);
    for want in [vec![2_i64, 3], vec![10, 15], vec![30, 45]] {
        assert!(
            struct_lists.contains(&want),
            "expected Point shape `{want:?}` as a typed Struct after mutation; \
             got Struct shapes = {struct_lists:?}"
        );
    }

    // ----- Scalar reads through `&p` (read_x=10, read_y=15, final=30/45) --
    let ints: std::collections::BTreeSet<i64> =
        unique_int_pairs(&doc).into_iter().map(|(_, v)| v).collect();
    for want in [10, 15, 30, 45] {
        assert!(
            ints.contains(&want),
            "expected scalar value {want} (read through ref / final mutation); got {ints:?}"
        );
    }
}

#[test]
fn test_references_use_typed_reference_value_record() {
    let Some((doc, _)) = record_and_dump_full(
        "test_references_use_typed_reference_value_record",
        "test_references",
    ) else {
        return;
    };
    let mut kinds = std::collections::BTreeSet::new();
    for ev in doc["events"].as_array().unwrap() {
        if ev["kind"] != "call_entry" {
            continue;
        }
        for a in ev["args"].as_array().cloned().unwrap_or_default() {
            if let Some(k) = a["value"]["kind"].as_str() {
                kinds.insert(k.to_string());
            }
        }
    }
    assert!(
        kinds.contains("Reference") || kinds.contains("Pointer"),
        "expected Reference/Pointer ValueRecord for Move &mut/& args; got {kinds:?}"
    );
}

// ===========================================================================
// test_abort — error path: `abort E_TEST_ABORT` (code 42)
// ===========================================================================

/// Records `flow_test::test_abort`.  The Move source aborts with code
/// `E_TEST_ABORT = 42` after binding `x = 10` and `y = 0`.  This is
/// the `expected_failure` Move test pattern — the canonical Move
/// equivalent of "raise without handler (program-terminating)" from
/// the recorder spec's universal-checklist row.
#[test]
fn test_abort_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full("test_abort_via_ct_print_full", "test_abort") else {
        return;
    };

    assert_metadata_program_is(&doc, "flow_test");
    assert_paths_contains_flow_test(&doc);

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(functions, vec!["test_abort"]);

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(1));
    assert_eq!(
        counts["io_events"].as_u64(),
        Some(1),
        "abort must surface exactly one io_event of kind ioError; counts={counts}",
    );

    // 1 step + 1 call_entry + 1 io + 1 call_exit = 4 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 4, "events.len()");
    assert_step_indices_monotonic(&doc);

    assert_eq!(observed_call_sequence(&doc), vec!["test_abort".to_string()]);
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].0, "test_abort");
    assert_eq!(exits[0].1["kind"].as_str(), Some("Void"));

    // ----- The `io` event: kind ioError, text "ABORTED: code 42" ----------
    // The Move v3 trace format models `abort 42` as the sequence
    //   Instruction{ABORT} -> Effect::Pop(U64 42) -> Effect::ExecutionError("ABORTED")
    // and the `ExecutionError` payload itself is just the bare marker
    // `"ABORTED"`.  The recorder stitches the popped abort code back into
    // the io_event content so distinct abort sites surface distinct
    // payloads (see `test_abort_io_event_carries_abort_code`).
    let io = events
        .iter()
        .find(|e| e["kind"] == "io")
        .expect("expected one `io` event for the abort");
    assert_eq!(io["io_kind"].as_str(), Some("ioError"));
    assert_eq!(io["text"].as_str(), Some("ABORTED: code 42"));
    assert_eq!(io["bytes_len"].as_u64(), Some(16));
    assert_eq!(io["io_index"].as_u64(), Some(0));
    assert_eq!(io["step_id"].as_u64(), Some(0));

    // ----- The abort code 42 must surface in the merged step's vars -------
    let int_set: std::collections::BTreeSet<i64> =
        unique_int_pairs(&doc).into_iter().map(|(_, v)| v).collect();
    assert!(
        int_set.contains(&42),
        "expected abort code 42 (E_TEST_ABORT) in vars; got {int_set:?}"
    );
    // y = 0 surfaces (it's the operand of the `y == 0` predicate that
    // triggers the abort).
    assert!(
        int_set.contains(&0),
        "expected y=0 in vars; got {int_set:?}"
    );
    // RECORDER BUG: the source-level binding `let x: u64 = 10;` does
    // NOT surface in the trace.  The Sui Move VM apparently elides
    // `x` because it is dead in the code path that executes (the
    // abort branch never reads `x`).  A spec-compliant trace would
    // record every let-binding regardless of dead-code analysis;
    // pin the present-day shape here so any change is caught.
    assert!(
        !int_set.contains(&10),
        "RECORDER BUG pinned: x=10 unexpectedly surfaced in test_abort vars; \
         if the recorder now captures dead let-bindings, extend the \
         assertion above to require it.  Got {int_set:?}"
    );
}

#[test]
fn test_abort_io_event_carries_abort_code() {
    let Some((doc, _)) =
        record_and_dump_full("test_abort_io_event_carries_abort_code", "test_abort")
    else {
        return;
    };
    let io = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "io")
        .expect("io event");
    let text = io["text"].as_str().unwrap_or("");
    assert!(
        text.contains("42"),
        "expected abort code 42 to be embedded in the io event text; got `{text}`"
    );
}

// ===========================================================================
// test_fibonacci — repeated calls with varied arguments
// ===========================================================================

/// Records `flow_test::test_fibonacci`.  Calls `fibonacci(n)` for
/// `n in [0, 1, 5, 10, 15]`, expecting `[0, 1, 5, 55, 610]`.  Pins
/// every (arg, return) pair on the call_entry / call_exit events so a
/// regression in argument decoding or return decoding is caught.
#[test]
fn test_fibonacci_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full("test_fibonacci_via_ct_print_full", "test_fibonacci")
    else {
        return;
    };

    assert_metadata_program_is(&doc, "flow_test");
    assert_paths_contains_flow_test(&doc);

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(functions, vec!["test_fibonacci", "fibonacci"]);

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(6));
    assert_eq!(counts["io_events"].as_u64(), Some(0));

    // 1 step + 6 call_entry + 6 call_exit = 13 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 13);
    assert_step_indices_monotonic(&doc);

    // ----- Call sequence: five fibonacci calls + the test entry ----------
    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "fibonacci".to_string(),
            "fibonacci".to_string(),
            "fibonacci".to_string(),
            "fibonacci".to_string(),
            "fibonacci".to_string(),
            "test_fibonacci".to_string(),
        ]
    );

    // ----- Argument decoding: fibonacci(0,1,5,10,15) ----------------------
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let fib_arg = |idx: usize| -> i64 {
        let args = entries[idx]["args"].as_array().expect("args array");
        assert_eq!(args.len(), 1, "fibonacci takes one u64 arg");
        args[0]["value"]["i"].as_i64().expect("arg Int.i")
    };
    assert_eq!(fib_arg(0), 0);
    assert_eq!(fib_arg(1), 1);
    assert_eq!(fib_arg(2), 5);
    assert_eq!(fib_arg(3), 10);
    assert_eq!(fib_arg(4), 15);
    assert!(
        entries[5]["args"].as_array().unwrap().is_empty(),
        "test_fibonacci itself takes no args"
    );

    // ----- Return values: F(n) for n in [0,1,5,10,15] = [0,1,5,55,610] ---
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits[0].0, "fibonacci");
    assert_eq!(exits[0].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[0].1["i"].as_i64(), Some(0));
    assert_eq!(exits[1].1["i"].as_i64(), Some(1));
    assert_eq!(exits[2].1["i"].as_i64(), Some(5));
    assert_eq!(exits[3].1["i"].as_i64(), Some(55));
    assert_eq!(exits[4].1["i"].as_i64(), Some(610));
    assert_eq!(exits[5].0, "test_fibonacci");
    assert_eq!(exits[5].1["kind"].as_str(), Some("Void"));
}

// ===========================================================================
// test_generics — generic Container<T> for T in {u64, bool, Point}
// ===========================================================================

/// Records `flow_test::test_generics`.  Wraps and unwraps three
/// different concrete types through the generic `Container<T>` and
/// `wrap_value<T>` / `unwrap_value<T>`.  Pins the printed form of the
/// `Container { value: ..., label: N }` per concrete `T`.
///
/// RECORDER BUG: the `bool` case surfaces `arg0` for `wrap_value<bool>(true, 2)`
/// as a JSON `null` in the call_entry args — Move's `bool` type encodes
/// to the `String` ValueRecord variant and the converter currently
/// drops it when consumed as a generic argument.  The first arg of
/// the second `wrap_value` invocation has `value.text == null` instead
/// of `"true"`.  Pinned below as the present-day shape.
#[test]
fn test_generics_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full("test_generics_via_ct_print_full", "test_generics")
    else {
        return;
    };

    assert_metadata_program_is(&doc, "flow_test");
    assert_paths_contains_flow_test(&doc);

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        functions,
        vec!["test_generics", "wrap_value", "unwrap_value"]
    );

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(7));
    assert_eq!(counts["io_events"].as_u64(), Some(0));

    // 1 step + 7 call_entry + 7 call_exit = 15 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 15);
    assert_step_indices_monotonic(&doc);

    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "wrap_value".to_string(),
            "unwrap_value".to_string(),
            "wrap_value".to_string(),
            "unwrap_value".to_string(),
            "wrap_value".to_string(),
            "unwrap_value".to_string(),
            "test_generics".to_string(),
        ]
    );

    // ----- Per-type return values ----------------------------------------
    // The Move recorder now emits typed `ValueRecord::Struct` payloads
    // for Move struct returns (Container<T>) — `kind:"Struct"` with a
    // `field_values` array carrying the recursively-converted children.
    // Previously these surfaced as printed-form `String` values; the
    // typed shape lets the frontend object inspector walk fields
    // instead of re-parsing the rendered text.
    let exits = observed_exit_sequence(&doc);
    // wrap_value<u64>(42, 1) -> Container { value: 42, label: 1 }
    //   field_values = [Int(42), Int(1)]
    assert_eq!(exits[0].0, "wrap_value");
    assert_eq!(exits[0].1["kind"].as_str(), Some("Struct"));
    let c1_fields = exits[0].1["field_values"]
        .as_array()
        .expect("Struct.field_values");
    assert_eq!(c1_fields.len(), 2, "Container has two fields");
    assert_eq!(c1_fields[0]["kind"].as_str(), Some("Int"));
    assert_eq!(c1_fields[0]["i"].as_i64(), Some(42));
    assert_eq!(c1_fields[1]["kind"].as_str(), Some("Int"));
    assert_eq!(c1_fields[1]["i"].as_i64(), Some(1));
    // unwrap_value<u64>(c1) -> 42
    assert_eq!(exits[1].0, "unwrap_value");
    assert_eq!(exits[1].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[1].1["i"].as_i64(), Some(42));
    // wrap_value<bool>(true, 2) -> Container { value: true, label: 2 }
    //   field_values = [Bool(true), Int(2)]
    assert_eq!(exits[2].0, "wrap_value");
    assert_eq!(exits[2].1["kind"].as_str(), Some("Struct"));
    let c2_fields = exits[2].1["field_values"]
        .as_array()
        .expect("Struct.field_values");
    assert_eq!(c2_fields.len(), 2);
    assert_eq!(c2_fields[0]["kind"].as_str(), Some("Bool"));
    assert_eq!(c2_fields[0]["b"].as_bool(), Some(true));
    assert_eq!(c2_fields[1]["kind"].as_str(), Some("Int"));
    assert_eq!(c2_fields[1]["i"].as_i64(), Some(2));
    // unwrap_value<bool>(c2) -> true.  The recorder builds
    // `ValueRecord::Bool` here, so this exit now surfaces with the
    // typed Bool variant (kind=Bool, b=true, text="true") rather than
    // the previous flattened Raw "true" string.
    assert_eq!(exits[3].0, "unwrap_value");
    assert_eq!(exits[3].1["kind"].as_str(), Some("Bool"));
    assert_eq!(exits[3].1["b"].as_bool(), Some(true));
    assert_eq!(exits[3].1["text"].as_str(), Some("true"));
    // wrap_value<Point>(pt, 3) -> Container { value: Point {...}, label: 3 }
    //   field_values = [Struct(Point{Int(5), Int(10)}), Int(3)]
    assert_eq!(exits[4].0, "wrap_value");
    assert_eq!(exits[4].1["kind"].as_str(), Some("Struct"));
    let c3_fields = exits[4].1["field_values"]
        .as_array()
        .expect("Struct.field_values");
    assert_eq!(c3_fields.len(), 2);
    assert_eq!(c3_fields[0]["kind"].as_str(), Some("Struct"));
    let pt_fields = c3_fields[0]["field_values"]
        .as_array()
        .expect("nested Point Struct.field_values");
    assert_eq!(pt_fields.len(), 2);
    assert_eq!(pt_fields[0]["kind"].as_str(), Some("Int"));
    assert_eq!(pt_fields[0]["i"].as_i64(), Some(5));
    assert_eq!(pt_fields[1]["kind"].as_str(), Some("Int"));
    assert_eq!(pt_fields[1]["i"].as_i64(), Some(10));
    assert_eq!(c3_fields[1]["kind"].as_str(), Some("Int"));
    assert_eq!(c3_fields[1]["i"].as_i64(), Some(3));
    // unwrap_value<Point>(c3) -> Point { x: 5, y: 10 } (typed Struct)
    assert_eq!(exits[5].0, "unwrap_value");
    assert_eq!(exits[5].1["kind"].as_str(), Some("Struct"));
    let pt5_fields = exits[5].1["field_values"]
        .as_array()
        .expect("Point Struct.field_values");
    assert_eq!(pt5_fields.len(), 2);
    assert_eq!(pt5_fields[0]["i"].as_i64(), Some(5));
    assert_eq!(pt5_fields[1]["i"].as_i64(), Some(10));
    // test_generics -> Void
    assert_eq!(exits[6].0, "test_generics");
    assert_eq!(exits[6].1["kind"].as_str(), Some("Void"));

    // ----- Generic argument decoding -------------------------------------
    // After the bool-text decoding fix, the `bool` argument to
    // wrap_value<bool>(true, 2) surfaces as a `Bool`-kind ValueRecord
    // with the printed boolean in `text` (the streaming CBOR encoder
    // for booleans now writes a 4-key map including `text: "true"|"false"`
    // alongside `kind`, `b`, and `type_id`, mirroring how Int/Float
    // populate `text` on the call-arg path).
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    // wrap_value<u64>(42, 1)
    let wv_u64_args = entries[0]["args"].as_array().unwrap();
    assert_eq!(wv_u64_args[0]["value"]["kind"].as_str(), Some("Int"));
    assert_eq!(wv_u64_args[0]["value"]["i"].as_i64(), Some(42));
    assert_eq!(wv_u64_args[1]["value"]["i"].as_i64(), Some(1));
    // wrap_value<bool>(true, 2)
    let wv_bool_args = entries[2]["args"].as_array().unwrap();
    assert_eq!(wv_bool_args[1]["value"]["i"].as_i64(), Some(2));
    // The bool arg surfaces as a Bool with `text="true"`.
    assert_eq!(wv_bool_args[0]["value"]["kind"].as_str(), Some("Bool"));
    assert_eq!(
        wv_bool_args[0]["value"]["text"].as_str(),
        Some("true"),
        "bool generic arg should carry text=`true`; got value={}",
        wv_bool_args[0]["value"],
    );
}

#[test]
fn test_generics_bool_arg_decodes_text() {
    let Some((doc, _)) =
        record_and_dump_full("test_generics_bool_arg_decodes_text", "test_generics")
    else {
        return;
    };
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let wv_bool_args = entries[2]["args"].as_array().unwrap();
    assert_eq!(
        wv_bool_args[0]["value"]["text"].as_str(),
        Some("true"),
        "expected bool generic arg to decode as text=`true`"
    );
}

// ===========================================================================
// test_boolean_and_integers — bool ops, u8 + u128 arithmetic
// ===========================================================================

/// Records `flow_test::test_boolean_and_integers`.  Exercises `&&`,
/// `||`, `!`, u8 arithmetic (`200 + 55 = 255`), u128 arithmetic
/// (`1_000_000_000_000 + 2_000_000_000_000 = 3_000_000_000_000`), and
/// a boolean conditional yielding `status = 1`.
///
/// RECORDER BUG: the u128 sum `3_000_000_000_000` *does* fit in i64
/// (max i64 ≈ 9.2e18), so it surfaces as a plain `Int { i: ... }`.  A
/// truly out-of-i64-range u128 would force the recorder into a
/// `BigInt` ValueRecord variant; the current fixture cannot exercise
/// that path.  See `test_boolean_and_integers_u128_overflow_uses_bigint`
/// for the dedicated u128 spec pin (which feeds the converter a
/// synthetic NDJSON trace so it does not depend on a re-recorded
/// fixture).
#[test]
fn test_boolean_and_integers_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full(
        "test_boolean_and_integers_via_ct_print_full",
        "test_boolean_and_integers",
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "flow_test");
    assert_paths_contains_flow_test(&doc);

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(functions, vec!["test_boolean_and_integers"]);

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(1));
    assert_eq!(counts["io_events"].as_u64(), Some(0));

    // 1 step + 1 call_entry + 1 call_exit = 3 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 3);
    assert_step_indices_monotonic(&doc);

    assert_eq!(
        observed_call_sequence(&doc),
        vec!["test_boolean_and_integers".to_string()]
    );
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits[0].0, "test_boolean_and_integers");
    assert_eq!(exits[0].1["kind"].as_str(), Some("Void"));

    // ----- All canonical integer values must surface in vars -------------
    // RECORDER BUG: the Sui Move VM trace for `test_boolean_and_integers`
    // (`flow_test__flow_test__test_boolean_and_integers.json.zst`) does
    // NOT contain any `U8` or `U128` values — the compiler appears to
    // have constant-folded the small_a/small_b/small_sum and big_a/
    // big_b/big_sum let-bindings, since their results are only used by
    // dead `assert!` calls.  The only Int that surfaces is the final
    // `status: u64 = 1`.
    //
    // A spec-compliant trace would preserve every let-binding so a
    // user-visible value at line N can be inspected; the test pins
    // the present-day "only status survives" shape so any future
    // capture-of-dead-bindings shows up as a failure here and the
    // assertion below grows accordingly.
    let int_set: std::collections::BTreeSet<i64> =
        unique_int_pairs(&doc).into_iter().map(|(_, v)| v).collect();
    assert_eq!(
        int_set,
        std::collections::BTreeSet::from([1_i64]),
        "RECORDER BUG pinned: today only status=1 survives the Sui VM \
         constant-folding; if more Int values now appear, extend this \
         assertion to require them"
    );

    // ----- Boolean typed-Bool values --------------------------------------
    // After the trace-writer-nim wrapper fix, Move bools surface as
    // typed `ValueRecord::Bool` (kind="Bool", b=true|false, text=...)
    // rather than the historical flattened-to-Raw `"true"`/`"false"`
    // strings. We keep the printed-form coalescer (`unique_raw_pairs`)
    // for the `t && f` derived bool textual checks AND assert on the
    // typed Bool shape so any regression toward Raw is loud.
    let raw_set: std::collections::BTreeSet<String> =
        unique_raw_pairs(&doc).into_iter().map(|(_, r)| r).collect();
    assert!(
        raw_set.contains("true"),
        "expected at least one `true` printed-form value; got {raw_set:?}"
    );
    assert!(
        raw_set.contains("false"),
        "expected at least one `false` printed-form value (from t && f); got {raw_set:?}"
    );
    let bool_set: std::collections::BTreeSet<bool> = unique_bool_pairs(&doc)
        .into_iter()
        .map(|(_, b)| b)
        .collect();
    assert!(
        bool_set.contains(&true),
        "expected at least one typed `Bool {{b:true,text:\"true\"}}` value; got {bool_set:?}"
    );
    assert!(
        bool_set.contains(&false),
        "expected at least one typed `Bool {{b:false,text:\"false\"}}` value (from t && f); got {bool_set:?}"
    );
}

/// Spec pin for u128 values that exceed `i64::MAX`: the recorder must
/// emit a `ValueRecord::BigInt` rather than truncating into an
/// `i64`-typed `ValueRecord::Int` (which would silently flip the sign
/// and lose the high bits).
///
/// The pre-recorded `flow_test::test_boolean_and_integers` fixture
/// shipped under `test-programs/move/flow_test/traces/` has the Sui
/// Move VM constant-folding the source's `let big_a/big_b/big_sum`
/// bindings before they reach the trace, and regenerating the
/// .json.zst requires the un-Nix-packaged `sui` CLI.  Until that
/// re-recording happens, this test feeds the converter a synthetic v3
/// NDJSON trace with a `U128` value of `18_000_000_000_000_000_000`
/// (≈ 2 × i64::MAX) and asserts the resulting CTFS bundle carries a
/// `BigInt`-kind ValueRecord with the full 128-bit big-endian
/// magnitude.
#[test]
fn test_boolean_and_integers_u128_overflow_uses_bigint() {
    let Some(ct_print) = ct_print_or_skip("test_boolean_and_integers_u128_overflow_uses_bigint")
    else {
        return;
    };

    // Synthetic v3 NDJSON: open a `test_u128` frame, push a U128 value
    // exceeding i64::MAX, write it into a local, then close the frame.
    // This is the exact shape Sui's `--trace-execution` would emit if
    // the constant-folder did not elide the `let big_sum: u128 = ...`
    // binding.
    //
    //   2^63 - 1  =  9_223_372_036_854_775_807   (= i64::MAX)
    //   18 * 1e18 = 18_000_000_000_000_000_000   (overflow, fits in u128)
    let big: u128 = 18_000_000_000_000_000_000u128;
    // Build the U128 Write effect by string-concatenation so the JSON
    // braces don't have to be escaped through `format!`'s grammar.
    let write_event = format!(
        r#"{{"Effect":{{"Write":{{"location":{{"Local":[1,0]}},"root_value_after_write":{{"RuntimeValue":{{"value":{{"type":"U128","value":{}}}}}}}}}}}}}"#,
        big
    );
    let ndjson = [
        r#"{"version":3}"#,
        r#"{"OpenFrame":{"frame":{"frame_id":1,"function_name":"test_u128","module":{"address":"0x0","name":"flow_test"},"type_instantiation":[],"parameters":[],"return_types":[],"locals_types":[{"type_":"u128"}],"is_native":false},"gas_left":1000000}}"#,
        r#"{"Instruction":{"type_parameters":[],"pc":0,"gas_left":999990,"instruction":"LdU128"}}"#,
        write_event.as_str(),
        r#"{"CloseFrame":{"frame_id":1,"return_":[],"gas_left":999980}}"#,
    ]
    .join("\n");

    let source_path = flow_test_source();
    let tmp_dir = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp_dir.path().join("ct-out");

    converter::convert_trace(
        ndjson.as_bytes(),
        &SourceMapResolver::empty(),
        &source_path,
        &out_dir,
    )
    .expect("convert_trace should succeed for synthetic u128 NDJSON");

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

    let output = Command::new(&ct_print)
        .args(["--full", "--strip-paths"])
        .arg(&ct_files[0])
        .output()
        .expect("failed to spawn ct-print");
    assert!(
        output.status.success(),
        "ct-print --full should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let doc: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("ct-print --full should emit valid JSON");

    // Walk every step event's `vars` array and look for a BigInt-kind
    // value whose decoded magnitude matches `big`.  ct-print's `--full`
    // pretty-printer emits `BigInt` payloads as
    //   { "kind": "BigInt", "b": "<base64 BE>", "negative": <bool>, "type_id": <u32> }
    let mut found = false;
    for ev in doc["events"].as_array().expect("events array") {
        if ev["kind"] != "step" {
            continue;
        }
        for v in ev["vars"].as_array().cloned().unwrap_or_default() {
            let value = &v["value"];
            if value["kind"].as_str() != Some("BigInt") {
                continue;
            }
            assert_eq!(
                value["negative"].as_bool(),
                Some(false),
                "u128 BigInt must be non-negative; got {value}"
            );
            let b64 = value["b"]
                .as_str()
                .expect("BigInt.b must be a base64 string");
            let bytes = base64_decode(b64).expect("BigInt.b must decode as base64");
            assert!(
                !bytes.is_empty(),
                "BigInt.b must carry at least one byte for non-zero magnitudes"
            );
            // Reconstruct the magnitude as u128 from big-endian bytes.
            let mut magnitude: u128 = 0;
            for byte in &bytes {
                magnitude = (magnitude << 8) | (*byte as u128);
            }
            assert_eq!(
                magnitude, big,
                "BigInt.b must encode the full u128 magnitude {big} (got {magnitude} from \
                 bytes={bytes:?})"
            );
            found = true;
            break;
        }
        if found {
            break;
        }
    }
    assert!(
        found,
        "expected a `ValueRecord::BigInt` in the step vars carrying {big}; \
         got events={}",
        serde_json::to_string_pretty(&doc["events"]).unwrap_or_default()
    );

    drop(tmp_dir);
}

// ===========================================================================
// M9 fixtures — variant constructors, wide integers, resources, Sui object
// lifecycle, and ability matrix.  Each uses a synthetic NDJSON trace shipped
// under `test-programs/move/flow_test/traces/` because the `sui` CLI is not
// yet packaged in the dev shell.  The strict shape pin is the same as the
// re-recorded fixtures above: function table, call sequence, exit shapes,
// and decoded variable values are asserted with `assert_eq!`.
// ===========================================================================

/// Records `flow_test::test_variant_constructors` (synthetic NDJSON).
///
/// Closes the M8 known-limitation `ValueRecord::Variant` falls through
/// to String via value_record_to_display.  Asserts the recorder now
/// emits a typed `kind:"Variant"` ValueRecord with a structured
/// discriminator and a `contents:Struct` payload — for the standard
/// library `Option<u64>::Some(42)` (tag=1), `Option<u64>::None`
/// (tag=0), and a Sui Move 2024 enum `Shape::Rect(3, 5)` (tag=1).
#[test]
fn test_variant_constructors_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_variant_constructors_via_ct_print_full",
        "test_variant_constructors",
        flow_test_named_source("variant_constructors_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "variant_constructors_test");
    let paths: Vec<&str> = doc["paths"]
        .as_array()
        .expect("paths array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        paths
            .iter()
            .any(|p| p.ends_with("variant_constructors_test.move")),
        "expected variant_constructors_test.move in paths; got {paths:?}",
    );

    // ----- Function table: outer test + 3 helpers -------------------------
    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        functions,
        vec![
            "test_variant_constructors",
            "make_some",
            "make_none",
            "make_rect"
        ]
    );

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1), "counts={counts}");
    assert_eq!(counts["calls"].as_u64(), Some(4), "counts={counts}");
    assert_eq!(counts["io_events"].as_u64(), Some(0), "counts={counts}");

    // 1 step + 4 call_entry + 4 call_exit = 9 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 9, "events.len()");
    assert_step_indices_monotonic(&doc);

    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "make_some".to_string(),
            "make_none".to_string(),
            "make_rect".to_string(),
            "test_variant_constructors".to_string(),
        ],
    );

    // ----- Return values: each helper returns a typed Variant -------------
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 4);

    // Some(42) -> Variant { discriminator: "0x1::option::Option::Variant#1",
    //                       contents: Struct { field_values: [Int(42)] } }
    assert_eq!(exits[0].0, "make_some");
    let some_rv = &exits[0].1;
    assert_eq!(some_rv["kind"].as_str(), Some("Variant"));
    assert_eq!(
        some_rv["discriminator"].as_str(),
        Some("0x1::option::Option::Variant#1"),
    );
    let some_contents = &some_rv["contents"];
    assert_eq!(some_contents["kind"].as_str(), Some("Struct"));
    let some_fields = some_contents["field_values"]
        .as_array()
        .expect("Variant.contents.field_values");
    assert_eq!(some_fields.len(), 1);
    assert_eq!(some_fields[0]["kind"].as_str(), Some("Int"));
    assert_eq!(some_fields[0]["i"].as_i64(), Some(42));

    // None -> Variant { discriminator: "...Variant#0", contents: Struct{} }
    assert_eq!(exits[1].0, "make_none");
    let none_rv = &exits[1].1;
    assert_eq!(none_rv["kind"].as_str(), Some("Variant"));
    assert_eq!(
        none_rv["discriminator"].as_str(),
        Some("0x1::option::Option::Variant#0"),
    );
    assert_eq!(none_rv["contents"]["kind"].as_str(), Some("Struct"));
    assert!(
        none_rv["contents"]["field_values"]
            .as_array()
            .expect("Variant.contents.field_values")
            .is_empty(),
        "None variant should carry an empty field_values array",
    );

    // Shape::Rect(3, 5) -> Variant { contents: Struct { fields: [Int(3), Int(5)] } }
    assert_eq!(exits[2].0, "make_rect");
    let rect_rv = &exits[2].1;
    assert_eq!(rect_rv["kind"].as_str(), Some("Variant"));
    assert_eq!(
        rect_rv["discriminator"].as_str(),
        Some("flow_test::variant_constructors_test::Shape::Variant#1"),
    );
    let rect_fields = rect_rv["contents"]["field_values"]
        .as_array()
        .expect("Variant.contents.field_values");
    assert_eq!(rect_fields.len(), 2);
    assert_eq!(rect_fields[0]["i"].as_i64(), Some(3));
    assert_eq!(rect_fields[1]["i"].as_i64(), Some(5));

    // test_variant_constructors itself returns Void.
    assert_eq!(exits[3].0, "test_variant_constructors");
    assert_eq!(exits[3].1["kind"].as_str(), Some("Void"));

    // ----- The Variant ValueRecord must NOT fall back to String --------
    // Pre-fix the Variant arm of `convert_move_value` emitted a printed
    // `ValueRecord::String { text: "Variant#N(...)" }`.  Walk every
    // step's vars and assert no such fallback appears anywhere.
    let mut variant_count = 0usize;
    for ev in events {
        if ev["kind"] != "step" {
            continue;
        }
        for v in ev["vars"].as_array().cloned().unwrap_or_default() {
            let val = &v["value"];
            if val["kind"] == "Variant" {
                variant_count += 1;
            }
            if let Some(text) = val["text"].as_str()
                && text.starts_with("Variant#")
            {
                panic!(
                    "regression: Variant arm fell back to printed-form String `{text}`; \
                     expected typed `ValueRecord::Variant`.  Full value: {val}",
                );
            }
        }
    }
    assert!(
        variant_count >= 3,
        "expected at least 3 Variant ValueRecords (Some, None, Rect) in step vars; \
         got {variant_count}",
    );

    // ----- The computed area = 3 * 5 = 15 must surface ------------------
    let int_set: std::collections::BTreeSet<i64> =
        unique_int_pairs(&doc).into_iter().map(|(_, v)| v).collect();
    assert!(
        int_set.contains(&15),
        "expected `area = 15` (= 3 * 5 from Rect match) in vars; got {int_set:?}",
    );
}

/// Records `flow_test::test_wide_integer` (synthetic NDJSON).
///
/// Pins the recorder's typed `Int` payloads for u8/u16/u32/u64 and the
/// `BigInt` payload for the u128 `18_000_000_000_000_000_000` (≈ 2 × i64::MAX)
/// — closing the M8 deferred `u128_overflow_uses_bigint` pin by USING
/// the values through `wide_product(a, b, c, d, e) -> u128` so the Sui
/// VM cannot constant-fold them away.  The product
/// `7 * 11 * 13 * 17 * 18e18 = 306_306_000_000_000_000_000_000` fits in
/// u128 (≈ 3.07e23 < u128::MAX ≈ 3.4e38) but vastly exceeds i64::MAX,
/// so it must surface as a `BigInt` ValueRecord.
#[test]
fn test_wide_integer_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_wide_integer_via_ct_print_full",
        "test_wide_integer",
        flow_test_named_source("wide_integer_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "wide_integer_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(functions, vec!["test_wide_integer", "wide_product"]);

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(2));
    assert_eq!(counts["io_events"].as_u64(), Some(0));

    // 1 step + 2 call_entry + 2 call_exit = 5 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 5);
    assert_step_indices_monotonic(&doc);

    assert_eq!(
        observed_call_sequence(&doc),
        vec!["wide_product".to_string(), "test_wide_integer".to_string()],
    );

    // ----- wide_product's args carry every integer width -----------------
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let wide_args = entries[0]["args"].as_array().expect("args array");
    assert_eq!(
        wide_args.len(),
        5,
        "wide_product takes (u8, u16, u32, u64, u128)"
    );
    // u8 = 7, u16 = 11, u32 = 13, u64 = 17 — all fit in Int.
    for (idx, want) in [7_i64, 11, 13, 17].iter().enumerate() {
        assert_eq!(
            wide_args[idx]["value"]["kind"].as_str(),
            Some("Int"),
            "arg{idx}",
        );
        assert_eq!(wide_args[idx]["value"]["i"].as_i64(), Some(*want));
    }
    // u128 = 18_000_000_000_000_000_000 — exceeds i64::MAX, must be BigInt.
    let u128_arg = &wide_args[4]["value"];
    assert_eq!(
        u128_arg["kind"].as_str(),
        Some("BigInt"),
        "u128 arg should surface as BigInt; got {u128_arg}",
    );
    assert_eq!(u128_arg["negative"].as_bool(), Some(false));
    let u128_bytes = base64_decode(u128_arg["b"].as_str().expect("BigInt.b base64 string"))
        .expect("base64 decode");
    let mut u128_mag: u128 = 0;
    for byte in &u128_bytes {
        u128_mag = (u128_mag << 8) | (*byte as u128);
    }
    assert_eq!(
        u128_mag, 18_000_000_000_000_000_000_u128,
        "u128 BigInt magnitude should round-trip 18e18",
    );

    // ----- wide_product's return is a BigInt of the full product ---------
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits[0].0, "wide_product");
    let prod_rv = &exits[0].1;
    assert_eq!(prod_rv["kind"].as_str(), Some("BigInt"));
    let prod_bytes =
        base64_decode(prod_rv["b"].as_str().expect("BigInt.b")).expect("base64 decode");
    let mut prod_mag: u128 = 0;
    for byte in &prod_bytes {
        prod_mag = (prod_mag << 8) | (*byte as u128);
    }
    assert_eq!(
        prod_mag, 306_306_000_000_000_000_000_000_u128,
        "wide_product return must encode 306306e18 = 7 * 11 * 13 * 17 * 18e18",
    );

    // test_wide_integer itself returns Void.
    assert_eq!(exits[1].0, "test_wide_integer");
    assert_eq!(exits[1].1["kind"].as_str(), Some("Void"));

    // ----- Every small width also appears as Int in the merged step ------
    let int_set: std::collections::BTreeSet<i64> =
        unique_int_pairs(&doc).into_iter().map(|(_, v)| v).collect();
    for want in [7_i64, 11, 13, 17] {
        assert!(
            int_set.contains(&want),
            "expected u8/u16/u32/u64 value {want} as a typed Int in step vars; got {int_set:?}",
        );
    }
}

/// Records `flow_test::test_resources` (synthetic NDJSON).
///
/// Resources (structs with the `key` ability) are Move's defining
/// feature.  Pins the recorder's typed `Struct` payloads for a `Coin`
/// resource through its full lifecycle: mint -> &Coin borrow ->
/// destructure-via-burn.  The `Coin` struct's owned type-id is stable
/// across all three call_exit events because the converter ensures
/// per-struct-name TypeIds are registered lazily once.
#[test]
fn test_resources_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_resources_via_ct_print_full",
        "test_resources",
        flow_test_named_source("resources_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "resources_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(functions, vec!["test_resources", "mint", "balance", "burn"]);

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(4));
    assert_eq!(counts["io_events"].as_u64(), Some(0));

    // 1 step + 4 call_entry + 4 call_exit = 9 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 9);
    assert_step_indices_monotonic(&doc);

    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "mint".to_string(),
            "balance".to_string(),
            "burn".to_string(),
            "test_resources".to_string(),
        ],
    );

    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 4);

    // mint(1, 100) -> Coin { id: 1, balance: 100 } (typed Struct)
    assert_eq!(exits[0].0, "mint");
    let mint_rv = &exits[0].1;
    assert_eq!(mint_rv["kind"].as_str(), Some("Struct"));
    let mint_fields = mint_rv["field_values"]
        .as_array()
        .expect("Struct.field_values");
    assert_eq!(mint_fields.len(), 2, "Coin has two fields (id, balance)");
    assert_eq!(mint_fields[0]["kind"].as_str(), Some("Int"));
    assert_eq!(mint_fields[0]["i"].as_i64(), Some(1));
    assert_eq!(mint_fields[1]["kind"].as_str(), Some("Int"));
    assert_eq!(mint_fields[1]["i"].as_i64(), Some(100));
    let coin_type_id = mint_rv["type_id"].as_u64().expect("Struct.type_id");

    // balance(&coin) -> Int(100) (read through ref)
    assert_eq!(exits[1].0, "balance");
    assert_eq!(exits[1].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[1].1["i"].as_i64(), Some(100));

    // burn(coin) -> Int(100) (consumed via destructure)
    assert_eq!(exits[2].0, "burn");
    assert_eq!(exits[2].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[2].1["i"].as_i64(), Some(100));

    // test_resources itself returns Void.
    assert_eq!(exits[3].0, "test_resources");
    assert_eq!(exits[3].1["kind"].as_str(), Some("Void"));

    // ----- The `&Coin` arg to balance() is a Reference wrapping a Struct ---
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let bal_arg0 = &entries[1]["args"][0]["value"];
    assert_eq!(bal_arg0["kind"].as_str(), Some("Reference"));
    assert_eq!(
        bal_arg0["mutable"].as_bool(),
        Some(false),
        "balance takes &Coin (immutable)",
    );
    let bal_deref = &bal_arg0["dereferenced"];
    assert_eq!(bal_deref["kind"].as_str(), Some("Struct"));
    assert_eq!(
        bal_deref["type_id"].as_u64(),
        Some(coin_type_id),
        "the &Coin pointee must share the Coin struct type id from mint's return",
    );
    let bal_fields = bal_deref["field_values"].as_array().expect("fields");
    assert_eq!(bal_fields[0]["i"].as_i64(), Some(1));
    assert_eq!(bal_fields[1]["i"].as_i64(), Some(100));

    // ----- The `Coin` typed-Struct shape must surface in the merged step --
    let struct_lists = collect_struct_int_lists(&doc);
    assert!(
        struct_lists.contains(&vec![1_i64, 100]),
        "expected Coin {{ id: 1, balance: 100 }} as typed Struct fields; got {struct_lists:?}",
    );
}

/// Records `flow_test::test_object_lifecycle` (synthetic NDJSON).
///
/// Pins the canonical Sui shape: a `Counter` struct whose `id: UID`
/// nests `UID -> ID -> Address` as Structs (matching Sui's
/// `sui::object::UID { id: ID { bytes: address } }` schema), mutated
/// through a `&mut Counter` reference, then read through a `&Counter`
/// reference.  An `External::Transfer` side effect surfaces as a
/// `TraceLogEvent` io entry.
#[test]
fn test_object_lifecycle_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_object_lifecycle_via_ct_print_full",
        "test_object_lifecycle",
        flow_test_named_source("object_lifecycle_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "object_lifecycle_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        functions,
        vec!["test_object_lifecycle", "increment", "value"]
    );

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(3));
    assert_eq!(
        counts["io_events"].as_u64(),
        Some(1),
        "External::Transfer must surface as exactly one io_event",
    );

    // 1 step + 3 call_entry + 1 io + 3 call_exit = 8 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 8);
    assert_step_indices_monotonic(&doc);

    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "increment".to_string(),
            "value".to_string(),
            "test_object_lifecycle".to_string(),
        ],
    );

    // ----- The io event for Transfer ------------------------------------
    let io = events
        .iter()
        .find(|e| e["kind"] == "io")
        .expect("expected an `io` event for the External::Transfer side effect");
    assert_eq!(io["text"].as_str(), Some("Transfer"));

    // ----- &mut Counter and &Counter args carry nested Struct payload ---
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    // increment(&mut Counter, by) -> Void
    let inc_args = entries[0]["args"].as_array().expect("args array");
    assert_eq!(inc_args.len(), 2);
    let inc_arg0 = &inc_args[0]["value"];
    assert_eq!(inc_arg0["kind"].as_str(), Some("Reference"));
    assert_eq!(inc_arg0["mutable"].as_bool(), Some(true));
    let counter = &inc_arg0["dereferenced"];
    assert_eq!(counter["kind"].as_str(), Some("Struct"));
    let counter_fields = counter["field_values"]
        .as_array()
        .expect("Counter.field_values");
    assert_eq!(counter_fields.len(), 2, "Counter {{ id: UID, value: u64 }}");
    // counter_fields[0] is the UID nested struct
    assert_eq!(counter_fields[0]["kind"].as_str(), Some("Struct"));
    let uid_fields = counter_fields[0]["field_values"]
        .as_array()
        .expect("UID.field_values");
    assert_eq!(uid_fields.len(), 1, "UID {{ id: ID }}");
    assert_eq!(uid_fields[0]["kind"].as_str(), Some("Struct"));
    let id_fields = uid_fields[0]["field_values"]
        .as_array()
        .expect("ID.field_values");
    assert_eq!(id_fields.len(), 1, "ID {{ bytes: address }}");
    assert_eq!(
        id_fields[0]["kind"].as_str(),
        Some("String"),
        "ID.bytes (address) surfaces as a String ValueRecord",
    );
    assert_eq!(id_fields[0]["text"].as_str(), Some("0xDEADBEEF"));
    // counter_fields[1] is the value: u64
    assert_eq!(counter_fields[1]["kind"].as_str(), Some("Int"));
    assert_eq!(counter_fields[1]["i"].as_i64(), Some(0));
    // increment's `by: u64` arg
    assert_eq!(inc_args[1]["value"]["kind"].as_str(), Some("Int"));
    assert_eq!(inc_args[1]["value"]["i"].as_i64(), Some(7));

    // value(&Counter) — same nested shape but `value: 7` after mutation
    let val_args = entries[1]["args"].as_array().expect("args array");
    let val_arg0 = &val_args[0]["value"];
    assert_eq!(val_arg0["kind"].as_str(), Some("Reference"));
    assert_eq!(
        val_arg0["mutable"].as_bool(),
        Some(false),
        "value() takes &Counter (immutable)",
    );
    let counter_after = &val_arg0["dereferenced"];
    let counter_after_fields = counter_after["field_values"]
        .as_array()
        .expect("Counter.field_values");
    assert_eq!(
        counter_after_fields[1]["i"].as_i64(),
        Some(7),
        "Counter.value is 7 after increment(7)",
    );

    // ----- Return values --------------------------------------------------
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 3);
    assert_eq!(exits[0].0, "increment");
    assert_eq!(exits[0].1["kind"].as_str(), Some("Void"));
    assert_eq!(exits[1].0, "value");
    assert_eq!(exits[1].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[1].1["i"].as_i64(), Some(7));
    assert_eq!(exits[2].0, "test_object_lifecycle");
    assert_eq!(exits[2].1["kind"].as_str(), Some("Void"));
}

/// Records `flow_test::test_abilities` (synthetic NDJSON).
///
/// Pins the recorder's coverage of the Move 4-ability matrix:
///   * Hot potato (`AccessToken`, no abilities) — minted then consumed
///     exactly once via destructure; surfaces as a typed Struct on
///     mint and an Int on consume.
///   * Copy + drop (`Datum`) — a single binding produces multiple Move
///     VM Struct copies of `Datum { x: 42 }`.
///   * Store-only (`StorageItem`) — explicit destructure required;
///     surfaces as a typed Struct then an Int return.
///
/// Each named struct type registers a distinct typed `TypeId` so the
/// converter does not collapse them into the generic `struct` fallback.
#[test]
fn test_abilities_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_abilities_via_ct_print_full",
        "test_abilities",
        flow_test_named_source("abilities_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "abilities_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        functions,
        vec![
            "test_abilities",
            "mint_token",
            "consume_token",
            "destroy_storage_item",
        ],
    );

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(4));
    assert_eq!(counts["io_events"].as_u64(), Some(0));

    // 1 step + 4 call_entry + 4 call_exit = 9 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 9);
    assert_step_indices_monotonic(&doc);

    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "mint_token".to_string(),
            "consume_token".to_string(),
            "destroy_storage_item".to_string(),
            "test_abilities".to_string(),
        ],
    );

    // ----- mint_token(7) -> AccessToken { operation_id: 7 } ---------------
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 4);
    assert_eq!(exits[0].0, "mint_token");
    let mint_rv = &exits[0].1;
    assert_eq!(mint_rv["kind"].as_str(), Some("Struct"));
    let mint_fields = mint_rv["field_values"]
        .as_array()
        .expect("Struct.field_values");
    assert_eq!(mint_fields.len(), 1);
    assert_eq!(mint_fields[0]["i"].as_i64(), Some(7));
    let token_type_id = mint_rv["type_id"].as_u64().expect("AccessToken type_id");

    // ----- consume_token(token) -> Int(7) (linear destructure) -----------
    assert_eq!(exits[1].0, "consume_token");
    assert_eq!(exits[1].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[1].1["i"].as_i64(), Some(7));

    // The hot potato AccessToken arg to consume_token must carry the
    // SAME type_id as the one returned by mint_token — this is the
    // "linearity" invariant from the recorder's POV: the same value
    // identity flows through.
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let consume_arg = &entries[1]["args"][0]["value"];
    assert_eq!(consume_arg["kind"].as_str(), Some("Struct"));
    assert_eq!(
        consume_arg["type_id"].as_u64(),
        Some(token_type_id),
        "the hot potato AccessToken passed to consume_token must share the \
         type_id minted by mint_token (linearity through the trace)",
    );

    // ----- destroy_storage_item(s) -> Int(99) ----------------------------
    assert_eq!(exits[2].0, "destroy_storage_item");
    assert_eq!(exits[2].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[2].1["i"].as_i64(), Some(99));

    // test_abilities itself returns Void.
    assert_eq!(exits[3].0, "test_abilities");
    assert_eq!(exits[3].1["kind"].as_str(), Some("Void"));

    // ----- Multiple Datum {x:42} copies surface in the merged step -------
    // `let d = Datum {x:42}; let d2 = d;` materialises the copy at the
    // Move VM level — both bindings appear as typed Struct values.
    let struct_lists = collect_struct_int_lists(&doc);
    let datum_copies = struct_lists
        .iter()
        .filter(|fields| fields == &&vec![42_i64])
        .count();
    assert!(
        datum_copies >= 2,
        "expected at least 2 Datum {{ x: 42 }} struct copies (d, d2); \
         got struct shapes = {struct_lists:?}",
    );
    // StorageItem { payload: 99 } also surfaces as a typed Struct.
    assert!(
        struct_lists.contains(&vec![99_i64]),
        "expected StorageItem {{ payload: 99 }} as a typed Struct; got {struct_lists:?}",
    );
    // AccessToken { operation_id: 7 } also surfaces.
    assert!(
        struct_lists.contains(&vec![7_i64]),
        "expected AccessToken {{ operation_id: 7 }} as a typed Struct; got {struct_lists:?}",
    );
}

/// Records `flow_test::test_option` (synthetic NDJSON).
///
/// Pins the recorder's `std::option::Option<T>` shape: `option::some(42)`
/// surfaces as a typed `ValueRecord::Variant` with discriminator
/// `0x1::option::Option::Variant#1` (Some) carrying an inner `Int(42)`,
/// `option::none<u64>()` surfaces as `Variant#0` (None) with an empty
/// payload, and `option::borrow(&Some(42))` returns a typed
/// `ValueRecord::Reference` whose pointee is the borrowed `u64`.
#[test]
fn test_option_test_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_option_test_via_ct_print_full",
        "test_option",
        flow_test_named_source("option_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "option_test");
    let paths: Vec<&str> = doc["paths"]
        .as_array()
        .expect("paths array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        paths.iter().any(|p| p.ends_with("option_test.move")),
        "expected option_test.move in paths; got {paths:?}",
    );

    // ----- Function table: outer test + 7 helpers ------------------------
    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        functions,
        vec![
            "test_option",
            "some",
            "none",
            "is_some",
            "is_none",
            "borrow_inner",
            "borrow",
            "extract",
        ],
    );

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1), "counts={counts}");
    assert_eq!(counts["calls"].as_u64(), Some(8), "counts={counts}");
    assert_eq!(counts["io_events"].as_u64(), Some(0), "counts={counts}");

    // 1 step + 8 call_entry + 8 call_exit = 17 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 17, "events.len()");
    assert_step_indices_monotonic(&doc);

    // CloseFrame ordering (LIFO — see observed_call_sequence comment).
    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "some".to_string(),
            "none".to_string(),
            "is_some".to_string(),
            "is_none".to_string(),
            "borrow".to_string(),
            "borrow_inner".to_string(),
            "extract".to_string(),
            "test_option".to_string(),
        ],
    );

    // ----- Return values pinned exactly ----------------------------------
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 8);

    // Some(42) -> Variant { discriminator: "0x1::option::Option::Variant#1",
    //                       contents: Struct { field_values: [Int(42)] } }
    assert_eq!(exits[0].0, "some");
    let some_rv = &exits[0].1;
    assert_eq!(some_rv["kind"].as_str(), Some("Variant"));
    assert_eq!(
        some_rv["discriminator"].as_str(),
        Some("0x1::option::Option::Variant#1"),
    );
    assert_eq!(some_rv["contents"]["kind"].as_str(), Some("Struct"));
    let some_fields = some_rv["contents"]["field_values"]
        .as_array()
        .expect("Variant.contents.field_values");
    assert_eq!(some_fields.len(), 1);
    assert_eq!(some_fields[0]["kind"].as_str(), Some("Int"));
    assert_eq!(some_fields[0]["i"].as_i64(), Some(42));
    let option_type_id = some_rv["type_id"].as_u64().expect("Variant.type_id");

    // None -> Variant#0 with empty payload.
    assert_eq!(exits[1].0, "none");
    let none_rv = &exits[1].1;
    assert_eq!(none_rv["kind"].as_str(), Some("Variant"));
    assert_eq!(
        none_rv["discriminator"].as_str(),
        Some("0x1::option::Option::Variant#0"),
    );
    assert_eq!(none_rv["contents"]["kind"].as_str(), Some("Struct"));
    assert!(
        none_rv["contents"]["field_values"]
            .as_array()
            .expect("Variant.contents.field_values")
            .is_empty(),
        "None must carry an empty field_values array",
    );
    assert_eq!(
        none_rv["type_id"].as_u64(),
        Some(option_type_id),
        "Some and None must share the Option<T> type_id",
    );

    // is_some / is_none -> Bool(true)
    assert_eq!(exits[2].0, "is_some");
    assert_eq!(exits[2].1["kind"].as_str(), Some("Bool"));
    assert_eq!(exits[2].1["b"].as_bool(), Some(true));
    assert_eq!(exits[2].1["text"].as_str(), Some("true"));

    assert_eq!(exits[3].0, "is_none");
    assert_eq!(exits[3].1["kind"].as_str(), Some("Bool"));
    assert_eq!(exits[3].1["b"].as_bool(), Some(true));
    assert_eq!(exits[3].1["text"].as_str(), Some("true"));

    // option::borrow(&Some(42)) -> &u64 — typed ValueRecord::Reference
    assert_eq!(exits[4].0, "borrow");
    let borrow_rv = &exits[4].1;
    assert_eq!(borrow_rv["kind"].as_str(), Some("Reference"));
    assert_eq!(borrow_rv["mutable"].as_bool(), Some(false));
    assert_eq!(borrow_rv["dereferenced"]["kind"].as_str(), Some("Int"));
    assert_eq!(borrow_rv["dereferenced"]["i"].as_i64(), Some(42));

    // borrow_inner / extract / test_option scalar returns.
    assert_eq!(exits[5].0, "borrow_inner");
    assert_eq!(exits[5].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[5].1["i"].as_i64(), Some(42));
    assert_eq!(exits[6].0, "extract");
    assert_eq!(exits[6].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[6].1["i"].as_i64(), Some(42));
    assert_eq!(exits[7].0, "test_option");
    assert_eq!(exits[7].1["kind"].as_str(), Some("Void"));

    // ----- Reference-typed call args carry the typed Variant pointee -----
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    // is_some takes &Option<u64> wrapping the Some(42) variant.
    let is_some_arg = &entries[2]["args"][0]["value"];
    assert_eq!(is_some_arg["kind"].as_str(), Some("Reference"));
    assert_eq!(is_some_arg["mutable"].as_bool(), Some(false));
    let is_some_pointee = &is_some_arg["dereferenced"];
    assert_eq!(is_some_pointee["kind"].as_str(), Some("Variant"));
    assert_eq!(
        is_some_pointee["discriminator"].as_str(),
        Some("0x1::option::Option::Variant#1"),
    );
    // is_none takes &Option<u64> wrapping the None variant.
    let is_none_arg = &entries[3]["args"][0]["value"];
    assert_eq!(is_none_arg["kind"].as_str(), Some("Reference"));
    assert_eq!(is_none_arg["mutable"].as_bool(), Some(false));
    let is_none_pointee = &is_none_arg["dereferenced"];
    assert_eq!(is_none_pointee["kind"].as_str(), Some("Variant"));
    assert_eq!(
        is_none_pointee["discriminator"].as_str(),
        Some("0x1::option::Option::Variant#0"),
    );
    // extract takes &mut Option<u64>.
    let extract_arg = &entries[6]["args"][0]["value"];
    assert_eq!(extract_arg["kind"].as_str(), Some("Reference"));
    assert_eq!(extract_arg["mutable"].as_bool(), Some(true));

    // ----- The post-extract write surfaces None -------------------------
    // After option::extract(&mut some_val) consumes the Some payload, the
    // converter emits a Write of the now-empty Option (Variant#0) to
    // local_0.  Walk the merged step's vars and assert the None shape
    // appears bound to local_0.
    let mut saw_none_at_local_0 = false;
    for (name, value) in collect_step_vars(
        &doc,
        &[
            "BigInt",
            "Bool",
            "Int",
            "Raw",
            "Reference",
            "String",
            "Sequence",
            "Struct",
            "Tuple",
            "Variant",
        ],
    ) {
        if name == "local_0"
            && value["kind"] == "Variant"
            && value["discriminator"] == "0x1::option::Option::Variant#0"
        {
            saw_none_at_local_0 = true;
        }
    }
    assert!(
        saw_none_at_local_0,
        "expected local_0 to surface as None (Variant#0) after extract consumes Some(42)",
    );
}

/// Records `flow_test::test_event_emit` (synthetic NDJSON).
///
/// Pins that a `sui::event::emit(MyEvent { sender, amount })` call
/// surfaces as exactly one structured `io` event tagged `MoveEvent`
/// whose `text` carries the typed event payload as a JSON object
/// (`{"fields": {...}, "struct": "MyEvent"}`) — alongside (not
/// replacing) the call_entry/call_exit pair for the `event::emit`
/// native frame.
#[test]
fn test_event_emit_test_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_event_emit_test_via_ct_print_full",
        "test_event_emit",
        flow_test_named_source("event_emit_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "event_emit_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(functions, vec!["test_event_emit", "fire", "emit"]);

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(3));
    assert_eq!(
        counts["io_events"].as_u64(),
        Some(1),
        "exactly one MoveEvent io_event must surface for sui::event::emit",
    );

    // 1 step + 3 call_entry + 1 io + 3 call_exit = 8 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 8, "events.len()");
    assert_step_indices_monotonic(&doc);

    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "emit".to_string(),
            "fire".to_string(),
            "test_event_emit".to_string(),
        ],
    );

    // ----- The structured MoveEvent io_event ----------------------------
    let io = events
        .iter()
        .find(|e| e["kind"] == "io")
        .expect("expected exactly one io event for sui::event::emit");
    let io_text = io["text"]
        .as_str()
        .expect("io.text str — MoveEvent payload as JSON");
    let payload: serde_json::Value = serde_json::from_str(io_text)
        .unwrap_or_else(|e| panic!("MoveEvent payload must be valid JSON ({e}); got {io_text}"));
    assert_eq!(
        payload,
        serde_json::json!({
            "struct": "MyEvent",
            "fields": {
                "sender": "0xCAFE",
                "amount": 1000_u64,
            },
        }),
        "MoveEvent payload mismatch",
    );

    // ----- The event::emit native call carries the typed Struct arg -----
    let entries: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let emit_args = entries[0]["args"].as_array().expect("emit args");
    assert_eq!(emit_args.len(), 1, "emit takes one event payload");
    let emit_arg0 = &emit_args[0]["value"];
    assert_eq!(emit_arg0["kind"].as_str(), Some("Struct"));
    let emit_fields = emit_arg0["field_values"]
        .as_array()
        .expect("Struct.field_values");
    assert_eq!(emit_fields.len(), 2, "MyEvent has two fields");
    assert_eq!(emit_fields[0]["kind"].as_str(), Some("String"));
    assert_eq!(emit_fields[0]["text"].as_str(), Some("0xCAFE"));
    assert_eq!(emit_fields[1]["kind"].as_str(), Some("Int"));
    assert_eq!(emit_fields[1]["i"].as_i64(), Some(1000));

    // ----- Return values --------------------------------------------------
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 3);
    assert_eq!(exits[0].0, "emit");
    assert_eq!(exits[0].1["kind"].as_str(), Some("Void"));
    assert_eq!(exits[1].0, "fire");
    assert_eq!(exits[1].1["kind"].as_str(), Some("Void"));
    assert_eq!(exits[2].0, "test_event_emit");
    assert_eq!(exits[2].1["kind"].as_str(), Some("Void"));
}

/// Records `flow_test::test_hash_builtins` (synthetic NDJSON).
///
/// Pins that `bcs::to_bytes(&Point { x: 3, y: 4 })`, `hash::sha2_256`,
/// and `hash::sha3_256` each round-trip their full byte-vector argument
/// and 32-byte digest as typed `ValueRecord::Sequence<u8>` payloads —
/// no truncation, no printed-form fallback.  The exact digest bytes are
/// pinned so any future native-call short-circuit (truncation, printed
/// form, base64 wrap, etc.) regresses the test.
#[test]
fn test_hash_builtins_test_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_hash_builtins_test_via_ct_print_full",
        "test_hash_builtins",
        flow_test_named_source("hash_builtins_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "hash_builtins_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        functions,
        vec![
            "test_hash_builtins",
            "to_bytes",
            "sha2_256",
            "sha3_256",
            "length",
        ],
    );

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(7));
    assert_eq!(counts["io_events"].as_u64(), Some(0));

    // 1 step + 7 call_entry + 7 call_exit = 15 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 15);
    assert_step_indices_monotonic(&doc);

    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "to_bytes".to_string(),
            "sha2_256".to_string(),
            "to_bytes".to_string(),
            "sha3_256".to_string(),
            "length".to_string(),
            "length".to_string(),
            "test_hash_builtins".to_string(),
        ],
    );

    // ----- Each native return surfaces as Sequence<u8> with exact bytes --
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 7);

    // bcs::to_bytes(&Point { x: 3, y: 4 }) -> [3,0,0,0,0,0,0,0, 4,0,0,0,0,0,0,0]
    let bcs_bytes_want: Vec<i64> = vec![3, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0];
    for idx in [0_usize, 2] {
        assert_eq!(exits[idx].0, "to_bytes");
        let rv = &exits[idx].1;
        assert_eq!(rv["kind"].as_str(), Some("Sequence"));
        assert_eq!(rv["is_slice"].as_bool(), Some(false));
        let elements = rv["elements"]
            .as_array()
            .expect("to_bytes return Sequence.elements");
        let got: Vec<i64> = elements
            .iter()
            .map(|e| {
                assert_eq!(e["kind"].as_str(), Some("Int"));
                e["i"].as_i64().expect("Int.i")
            })
            .collect();
        assert_eq!(
            got, bcs_bytes_want,
            "bcs::to_bytes byte-vector mismatch at exit {idx}",
        );
    }

    // sha2_256 / sha3_256 outputs (precomputed against [3,0,..,4,0,..]).
    let sha2_want: Vec<i64> = vec![
        253, 34, 59, 133, 244, 220, 32, 24, 63, 213, 149, 249, 15, 196, 132, 214, 122, 34, 66, 169,
        222, 88, 9, 205, 62, 132, 79, 169, 89, 84, 252, 74,
    ];
    let sha3_want: Vec<i64> = vec![
        112, 102, 84, 234, 114, 231, 158, 13, 163, 210, 222, 207, 68, 89, 105, 163, 135, 247, 104,
        70, 133, 32, 76, 213, 41, 140, 245, 208, 218, 15, 231, 23,
    ];
    let check_digest = |which: usize, name: &str, want: &[i64]| {
        assert_eq!(exits[which].0, name);
        let rv = &exits[which].1;
        assert_eq!(rv["kind"].as_str(), Some("Sequence"));
        let elements = rv["elements"]
            .as_array()
            .unwrap_or_else(|| panic!("{name} return Sequence.elements"));
        assert_eq!(elements.len(), 32, "{name} digest must be 32 bytes");
        let got: Vec<i64> = elements
            .iter()
            .map(|e| {
                assert_eq!(e["kind"].as_str(), Some("Int"));
                e["i"].as_i64().expect("Int.i")
            })
            .collect();
        assert_eq!(got, want.to_vec(), "{name} digest bytes mismatch");
    };
    check_digest(1, "sha2_256", &sha2_want);
    check_digest(3, "sha3_256", &sha3_want);

    // length(&digest) -> 32 (twice)
    for idx in [4_usize, 5] {
        assert_eq!(exits[idx].0, "length");
        assert_eq!(exits[idx].1["kind"].as_str(), Some("Int"));
        assert_eq!(exits[idx].1["i"].as_i64(), Some(32));
    }
    assert_eq!(exits[6].0, "test_hash_builtins");
    assert_eq!(exits[6].1["kind"].as_str(), Some("Void"));

    // ----- The hash arg also surfaces as the same Sequence<u8> ----------
    // CloseFrame ordering: entries[0]=to_bytes(1), [1]=sha2_256, [2]=to_bytes(2),
    // [3]=sha3_256, [4]=length, [5]=length, [6]=test_hash_builtins.
    let entries: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    // sha2_256(bytes) — the bytes arg is the bcs output (entries[1]).
    let sha2_arg0 = &entries[1]["args"][0]["value"];
    assert_eq!(sha2_arg0["kind"].as_str(), Some("Sequence"));
    let sha2_arg_elems = sha2_arg0["elements"]
        .as_array()
        .expect("sha2_256 arg Sequence.elements");
    let got_in: Vec<i64> = sha2_arg_elems
        .iter()
        .map(|e| e["i"].as_i64().expect("Int.i"))
        .collect();
    assert_eq!(
        got_in, bcs_bytes_want,
        "sha2_256's input byte-vector must match bcs::to_bytes output exactly",
    );
    // sha3_256(bytes2) — entries[3], same payload.
    let sha3_arg0 = &entries[3]["args"][0]["value"];
    assert_eq!(sha3_arg0["kind"].as_str(), Some("Sequence"));
    let sha3_arg_elems = sha3_arg0["elements"]
        .as_array()
        .expect("sha3_256 arg Sequence.elements");
    let got_in3: Vec<i64> = sha3_arg_elems
        .iter()
        .map(|e| e["i"].as_i64().expect("Int.i"))
        .collect();
    assert_eq!(
        got_in3, bcs_bytes_want,
        "sha3_256's input byte-vector must match bcs::to_bytes output exactly",
    );

    // ----- bcs::to_bytes(&p) takes a Reference<Point> ---------------------
    let bcs_arg0 = &entries[0]["args"][0]["value"];
    assert_eq!(bcs_arg0["kind"].as_str(), Some("Reference"));
    assert_eq!(bcs_arg0["mutable"].as_bool(), Some(false));
    let point = &bcs_arg0["dereferenced"];
    assert_eq!(point["kind"].as_str(), Some("Struct"));
    let point_fields = point["field_values"]
        .as_array()
        .expect("Point.field_values");
    assert_eq!(point_fields.len(), 2);
    assert_eq!(point_fields[0]["kind"].as_str(), Some("Int"));
    assert_eq!(point_fields[0]["i"].as_i64(), Some(3));
    assert_eq!(point_fields[1]["kind"].as_str(), Some("Int"));
    assert_eq!(point_fields[1]["i"].as_i64(), Some(4));
}

/// Records `flow_test::test_string` (synthetic NDJSON).
///
/// Pins that `std::string::String` flowing through `string::utf8`,
/// `string::append`, `string::sub_string`, `string::length` surfaces as
/// a typed `ValueRecord::Struct` whose single `bytes: vector<u8>` field
/// is a typed `ValueRecord::Sequence<u8>` from which the printable text
/// is recoverable byte-for-byte.
#[test]
fn test_string_test_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_string_test_via_ct_print_full",
        "test_string",
        flow_test_named_source("string_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "string_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        functions,
        vec!["test_string", "utf8", "append", "sub_string", "length"],
    );

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(7));
    assert_eq!(counts["io_events"].as_u64(), Some(0));

    // 1 step + 7 call_entry + 7 call_exit = 15 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 15);
    assert_step_indices_monotonic(&doc);

    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "utf8".to_string(),
            "utf8".to_string(),
            "append".to_string(),
            "sub_string".to_string(),
            "length".to_string(),
            "length".to_string(),
            "test_string".to_string(),
        ],
    );

    // Helper: extract the printable text from a String ValueRecord.
    fn string_struct_text(v: &serde_json::Value) -> String {
        assert_eq!(
            v["kind"].as_str(),
            Some("Struct"),
            "expected Struct; got {v}"
        );
        let fields = v["field_values"].as_array().expect("String.field_values");
        assert_eq!(fields.len(), 1, "String has a single bytes field");
        let bytes_field = &fields[0];
        assert_eq!(
            bytes_field["kind"].as_str(),
            Some("Sequence"),
            "String.bytes must be a typed Sequence; got {bytes_field}",
        );
        let elements = bytes_field["elements"]
            .as_array()
            .expect("String.bytes Sequence.elements");
        let raw: Vec<u8> = elements
            .iter()
            .map(|e| {
                assert_eq!(e["kind"].as_str(), Some("Int"));
                let i = e["i"].as_i64().expect("Int.i");
                assert!((0..=255).contains(&i), "byte out of range: {i}");
                i as u8
            })
            .collect();
        String::from_utf8(raw).expect("String bytes must round-trip as UTF-8")
    }

    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 7);

    // utf8(b"hello") -> "hello", utf8(b" world") -> " world"
    assert_eq!(exits[0].0, "utf8");
    assert_eq!(string_struct_text(&exits[0].1), "hello");
    assert_eq!(exits[1].0, "utf8");
    assert_eq!(string_struct_text(&exits[1].1), " world");

    // append(&mut s, suffix) -> Void
    assert_eq!(exits[2].0, "append");
    assert_eq!(exits[2].1["kind"].as_str(), Some("Void"));

    // sub_string(&s, 0, 5) -> "hello"
    assert_eq!(exits[3].0, "sub_string");
    assert_eq!(string_struct_text(&exits[3].1), "hello");

    // length(&s) -> 11, length(&head_bytes) -> 5
    assert_eq!(exits[4].0, "length");
    assert_eq!(exits[4].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[4].1["i"].as_i64(), Some(11));
    assert_eq!(exits[5].0, "length");
    assert_eq!(exits[5].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[5].1["i"].as_i64(), Some(5));

    assert_eq!(exits[6].0, "test_string");
    assert_eq!(exits[6].1["kind"].as_str(), Some("Void"));

    // ----- After append, the &mut s arg snapshot is "hello world" -------
    let entries: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    // sub_string(&s, 0, 5) — its first arg is a Reference whose pointee
    // String must spell out "hello world" after the in-place append.
    let sub_arg0 = &entries[3]["args"][0]["value"];
    assert_eq!(sub_arg0["kind"].as_str(), Some("Reference"));
    assert_eq!(sub_arg0["mutable"].as_bool(), Some(false));
    assert_eq!(string_struct_text(&sub_arg0["dereferenced"]), "hello world");
    // length(&s) — same shape.
    let len_arg0 = &entries[4]["args"][0]["value"];
    assert_eq!(len_arg0["kind"].as_str(), Some("Reference"));
    assert_eq!(string_struct_text(&len_arg0["dereferenced"]), "hello world");
}

/// Records `flow_test::test_vector_operations` (synthetic NDJSON).
///
/// Pins that each mutating + observational vector op surfaces with its
/// runtime side-effect on the contents.  The contents snapshot before
/// and after each `swap_remove`, `pop_back`, `reverse`, `append`, and
/// `borrow_mut`-then-write step is a typed `ValueRecord::Sequence` with
/// exact element values; `index_of` returns its `(found, idx)` shape as
/// a typed `ValueRecord::Tuple`; `borrow` and `borrow_mut` return typed
/// `ValueRecord::Reference`s.
#[test]
fn test_vector_operations_test_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_vector_operations_test_via_ct_print_full",
        "test_vector_operations",
        flow_test_named_source("vector_operations_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "vector_operations_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        functions,
        vec![
            "test_vector_operations",
            "swap_remove",
            "pop_back",
            "contains",
            "reverse",
            "append",
            "index_of",
            "borrow_mut",
            "borrow",
        ],
    );

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(10));
    assert_eq!(counts["io_events"].as_u64(), Some(0));

    // 1 step + 10 call_entry + 10 call_exit = 21 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 21);
    assert_step_indices_monotonic(&doc);

    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "swap_remove".to_string(),
            "pop_back".to_string(),
            "contains".to_string(),
            "contains".to_string(),
            "reverse".to_string(),
            "append".to_string(),
            "index_of".to_string(),
            "borrow_mut".to_string(),
            "borrow".to_string(),
            "test_vector_operations".to_string(),
        ],
    );

    // ----- Return values pinned exactly ----------------------------------
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 10);

    // swap_remove(v, 1) -> 20
    assert_eq!(exits[0].0, "swap_remove");
    assert_eq!(exits[0].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[0].1["i"].as_i64(), Some(20));
    // pop_back(v) -> 30
    assert_eq!(exits[1].0, "pop_back");
    assert_eq!(exits[1].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[1].1["i"].as_i64(), Some(30));
    // contains(v, 40) -> true
    assert_eq!(exits[2].0, "contains");
    assert_eq!(exits[2].1["kind"].as_str(), Some("Bool"));
    assert_eq!(exits[2].1["b"].as_bool(), Some(true));
    // contains(v, 99) -> false
    assert_eq!(exits[3].0, "contains");
    assert_eq!(exits[3].1["kind"].as_str(), Some("Bool"));
    assert_eq!(exits[3].1["b"].as_bool(), Some(false));
    // reverse(v) -> Void
    assert_eq!(exits[4].0, "reverse");
    assert_eq!(exits[4].1["kind"].as_str(), Some("Void"));
    // append(v, other) -> Void
    assert_eq!(exits[5].0, "append");
    assert_eq!(exits[5].1["kind"].as_str(), Some("Void"));
    // index_of(v, &7) -> (true, 2) — Tuple
    assert_eq!(exits[6].0, "index_of");
    let idx_rv = &exits[6].1;
    assert_eq!(idx_rv["kind"].as_str(), Some("Tuple"));
    let idx_elems = idx_rv["elements"].as_array().expect("Tuple.elements");
    assert_eq!(idx_elems.len(), 2);
    assert_eq!(idx_elems[0]["kind"].as_str(), Some("Bool"));
    assert_eq!(idx_elems[0]["b"].as_bool(), Some(true));
    assert_eq!(idx_elems[1]["kind"].as_str(), Some("Int"));
    assert_eq!(idx_elems[1]["i"].as_i64(), Some(2));
    // borrow_mut(v, 0) -> &mut u64 (Reference, mutable=true, pointee=40)
    assert_eq!(exits[7].0, "borrow_mut");
    let bm_rv = &exits[7].1;
    assert_eq!(bm_rv["kind"].as_str(), Some("Reference"));
    assert_eq!(bm_rv["mutable"].as_bool(), Some(true));
    assert_eq!(bm_rv["dereferenced"]["kind"].as_str(), Some("Int"));
    assert_eq!(bm_rv["dereferenced"]["i"].as_i64(), Some(40));
    // borrow(v, 0) -> &u64 (Reference, mutable=false, pointee=100 after *r=100)
    assert_eq!(exits[8].0, "borrow");
    let b_rv = &exits[8].1;
    assert_eq!(b_rv["kind"].as_str(), Some("Reference"));
    assert_eq!(b_rv["mutable"].as_bool(), Some(false));
    assert_eq!(b_rv["dereferenced"]["kind"].as_str(), Some("Int"));
    assert_eq!(b_rv["dereferenced"]["i"].as_i64(), Some(100));
    assert_eq!(exits[9].0, "test_vector_operations");
    assert_eq!(exits[9].1["kind"].as_str(), Some("Void"));

    // ----- Reference args carry the contents snapshot at call time ------
    let entries: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let extract_seq = |arg: &serde_json::Value| -> Vec<i64> {
        let v = if arg["kind"] == "Reference" {
            &arg["dereferenced"]
        } else {
            arg
        };
        assert_eq!(
            v["kind"].as_str(),
            Some("Sequence"),
            "expected Sequence; got {v}"
        );
        v["elements"]
            .as_array()
            .expect("Sequence.elements")
            .iter()
            .map(|e| {
                assert_eq!(e["kind"].as_str(), Some("Int"));
                e["i"].as_i64().expect("Int.i")
            })
            .collect()
    };
    // swap_remove sees v = [10, 20, 30, 40] (initial)
    assert_eq!(
        extract_seq(&entries[0]["args"][0]["value"]),
        vec![10_i64, 20, 30, 40],
    );
    // pop_back sees v = [10, 40, 30] (after swap_remove)
    assert_eq!(
        extract_seq(&entries[1]["args"][0]["value"]),
        vec![10_i64, 40, 30],
    );
    // contains(_, 40) sees v = [10, 40] (after pop_back)
    assert_eq!(
        extract_seq(&entries[2]["args"][0]["value"]),
        vec![10_i64, 40],
    );
    // contains(_, 99) sees the same v = [10, 40]
    assert_eq!(
        extract_seq(&entries[3]["args"][0]["value"]),
        vec![10_i64, 40],
    );
    // reverse sees v = [10, 40]
    assert_eq!(
        extract_seq(&entries[4]["args"][0]["value"]),
        vec![10_i64, 40],
    );
    // append sees v = [40, 10] (after reverse) and other = [7, 8]
    assert_eq!(
        extract_seq(&entries[5]["args"][0]["value"]),
        vec![40_i64, 10],
    );
    assert_eq!(extract_seq(&entries[5]["args"][1]["value"]), vec![7_i64, 8],);
    // index_of sees v = [40, 10, 7, 8] (after append)
    assert_eq!(
        extract_seq(&entries[6]["args"][0]["value"]),
        vec![40_i64, 10, 7, 8],
    );
    // borrow_mut sees the same v
    assert_eq!(
        extract_seq(&entries[7]["args"][0]["value"]),
        vec![40_i64, 10, 7, 8],
    );
    // borrow (final readback) sees v = [100, 10, 7, 8] (after *r = 100)
    assert_eq!(
        extract_seq(&entries[8]["args"][0]["value"]),
        vec![100_i64, 10, 7, 8],
    );

    // ----- The contents snapshots also surface as typed Sequences in the
    //       merged step's vars (one Sequence per Effect::Write to local_0).
    let seq_lists = collect_sequence_int_lists(&doc);
    for snapshot in [
        vec![10_i64, 20, 30, 40],
        vec![10_i64, 40, 30],
        vec![10_i64, 40],
        vec![40_i64, 10],
        vec![40_i64, 10, 7, 8],
        vec![100_i64, 10, 7, 8],
    ] {
        assert!(
            seq_lists.contains(&snapshot),
            "expected v snapshot {snapshot:?} as a typed Sequence in step vars; got {seq_lists:?}",
        );
    }
}

/// Decode the standard base64 alphabet (no URL-safe variant) into raw
/// bytes.  We open-code this rather than pulling in the `base64`
/// crate because the test only needs to round-trip a single field
/// emitted by ct-print's `--full` pretty-printer.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for &c in bytes {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = val(c)?;
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}
