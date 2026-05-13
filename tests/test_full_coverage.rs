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
    let ct_print = ct_print_or_skip(test_name)?;

    let trace_zst = flow_test_trace_fixture(move_test);
    let trace_bytes = read_decompressed_trace(&trace_zst);

    let source_path = flow_test_source();
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
        &["Bool", "Int", "Raw", "String", "Sequence", "Struct", "Tuple"],
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
        &["Bool", "Int", "Raw", "String", "Sequence", "Struct", "Tuple"],
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
        &["Bool", "Int", "Raw", "String", "Sequence", "Struct", "Tuple"],
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
        &["Bool", "Int", "Raw", "String", "Sequence", "Struct", "Tuple"],
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
        &["Bool", "Int", "Raw", "String", "Sequence", "Struct", "Tuple"],
    ) {
        if value["kind"] == "Bool" {
            let b = value["b"].as_bool().expect("Bool.b");
            // Spec invariant from streaming_value_encoder.writeBool: the
            // text field is always the lower-case stringification.
            let text = value["text"].as_str().expect("Bool.text");
            assert_eq!(text, if b { "true" } else { "false" },
                "Bool ValueRecord.text must mirror b; got value={value}");
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

/// Spec-correct expectation for `test_loops`: each iteration of the
/// while/loop bodies should emit one step event at the corresponding
/// source line.  Today the recorder collapses everything into one
/// merged step.
#[test]
#[ignore = "RECORDER BUG: the converter emits exactly one `step` event \
            per function frame, with every Move VM stack push/pop \
            stuffed into a single `vars` array.  Spec-compliant output \
            should emit one step event per source line, so a 10-iter \
            while loop produces ≥10 step events at the loop body line."]
fn test_loops_one_step_per_source_line() {
    let Some((doc, _)) = record_and_dump_full("test_loops_one_step_per_source_line", "test_loops")
    else {
        return;
    };
    let counts = &doc["counts"];
    assert!(
        counts["steps"].as_u64().unwrap_or(0) >= 10,
        "expected ≥10 step events for a 10-iter while loop; counts={counts}"
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

    // ----- &mut Point arg: surfaces as a typed Struct snapshot ------------
    // RECORDER BUG: a Move `&mut Point` reference comes through the
    // converter as the *snapshot* of the underlying Point — currently a
    // typed `ValueRecord::Struct { field_values: [Int x, Int y] }` —
    // rather than a typed `ValueRecord::Reference` carrying the pointee
    // type and a back-pointer.  Pin the present-day Struct snapshot
    // shape so any future reshape (towards a real Reference variant)
    // shows up here.  See `test_references_use_typed_reference_value_record`
    // (currently `#[ignore]`d) for the spec-correct expectation.
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let scale_args = |idx: usize| -> (String, Option<Vec<i64>>, Option<i64>) {
        let args = entries[idx]["args"].as_array().expect("args array");
        assert_eq!(args.len(), 2, "scale_point takes (&mut Point, u64)");
        let arg0 = &args[0]["value"];
        let arg1 = &args[1]["value"];
        let kind0 = arg0["kind"].as_str().expect("kind").to_string();
        let xy0 = arg0["field_values"].as_array().map(|fields| {
            fields
                .iter()
                .map(|f| f["i"].as_i64().expect("Int.i"))
                .collect::<Vec<_>>()
        });
        let i1 = arg1["i"].as_i64();
        (kind0, xy0, i1)
    };
    let (k0, xy0, i0) = scale_args(0);
    assert_eq!(
        k0, "Struct",
        "scale_point's &mut Point arg surfaces as a typed Struct snapshot"
    );
    assert_eq!(xy0.as_deref(), Some(&[2_i64, 3][..]), "Point {{ x: 2, y: 3 }}");
    assert_eq!(i0, Some(5), "scale_point's factor arg = 5");
    let (k1, xy1, i1) = scale_args(1);
    assert_eq!(k1, "Struct");
    assert_eq!(
        xy1.as_deref(),
        Some(&[10_i64, 15][..]),
        "Point {{ x: 10, y: 15 }} after first scale_point"
    );
    assert_eq!(i1, Some(3), "scale_point's second factor arg = 3");

    // ----- All Point shapes surface as typed Structs (incl. mutated copies)
    // The Move source threads a single Point through `scale_point(&mut, _)`
    // so we should observe `(2, 3)`, `(10, 15)`, and `(30, 45)` Struct
    // shapes among the merged step's vars.
    let struct_lists = collect_struct_int_lists(&doc);
    for want in [
        vec![2_i64, 3],
        vec![10, 15],
        vec![30, 45],
    ] {
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
#[ignore = "RECORDER BUG: Move `&mut T` / `&T` references surface as \
            `String {text: <printed>}` instead of a typed \
            `ValueRecord::Reference` (or similar) carrying the pointee \
            type and a back-pointer.  Spec-compliant output should \
            distinguish a borrowed reference from an owned printed-form \
            string."]
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
/// (currently `#[ignore]`d).
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
    let bool_set: std::collections::BTreeSet<bool> =
        unique_bool_pairs(&doc).into_iter().map(|(_, b)| b).collect();
    assert!(
        bool_set.contains(&true),
        "expected at least one typed `Bool {{b:true,text:\"true\"}}` value; got {bool_set:?}"
    );
    assert!(
        bool_set.contains(&false),
        "expected at least one typed `Bool {{b:false,text:\"false\"}}` value (from t && f); got {bool_set:?}"
    );
}

#[test]
#[ignore = "RECORDER BUG / fixture limitation: the Sui Move VM \
            constant-folds dead let-bindings before producing the \
            trace, so the small_a/small_b/small_sum (u8) and \
            big_a/big_b/big_sum (u128) values from the source program \
            never reach the converter.  A spec-compliant pipeline \
            would either preserve dead bindings in the VM trace or \
            have the converter synthesise step events from the source \
            map; without one of those, no u8 / u128 value can surface. \
            Once dead-binding preservation lands (or a u128>i64::MAX \
            fixture is added), assert here that the recorder emits a \
            `BigInt`-kind ValueRecord with the full 128-bit payload."]
fn test_boolean_and_integers_u128_overflow_uses_bigint() {
    // No live fixture exposes a u128 path today; the assertion in
    // `test_boolean_and_integers_via_ct_print_full` already pins the
    // current shape (`int_set == {1}`) so a regression that *adds*
    // u128 values is detected as a failure to extend the matrix.
    panic!("no fixture: see ignore reason above");
}
