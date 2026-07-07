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
//! Historical note: where the recorder's behaviour previously deviated
//! from what the Move semantics dictate (single merged step event for
//! the whole function body; struct/vector values surfacing as `Raw`
//! strings rather than typed `Struct`/`Sequence` `ValueRecord` variants;
//! `call_entry` events emitted in close-frame order; etc.), the
//! deviation used to be documented inline as `RECORDER BUG: ...` and
//! captured with a parallel pin.  The recorder + trace writer have
//! since been brought into spec alignment for the call-entry ordering,
//! the typed compound `ValueRecord` variants, the BigInt-for-u128
//! path, AND the source-level local naming (the recorder now reads
//! the Sui Move compiler's debug-info JSON sidecar at
//! `<package_root>/build/<PackageName>/debug_info/<Module>.json` and
//! substitutes source-level identifiers for the historical synthetic
//! `local_<N>` slot names — see `crate::move_debug_info`).  The one
//! remaining limitation — Sui Move compiler constant-folding +
//! dead-code-elimination dropping let-bindings whose values aren't
//! observed downstream (see `test_abort_via_ct_print_full` and
//! `test_boolean_and_integers_via_ct_print_full` for the per-fixture
//! pins documenting which source bindings survive) — is an upstream
//! compiler constraint, not a recorder bug.  Re-recording with a
//! Sui CLI built with DCE / constant-folding disabled would unblock
//! it; that CLI is not Nix-packaged in this workspace.
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
/// The Move recorder emits `register_call` at `OpenFrame` time and the
/// CTFS multi-stream writer materialises records in `call_key` (entry)
/// order, so the resulting `call_entry` sequence is the spec-correct
/// entry order — outermost (first opened) first, matching every other
/// recorder in the workspace.  Tests pin this entry order exactly.
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
/// Note: the recorded fixture has no source map, so all of the
/// function body collapses into a single merged `step` event with the
/// full `vars` snapshot — the spec-correct per-source-line stepping
/// invariant (one `step` per loop iteration) is exercised by the
/// synthetic-source-map sibling `test_loops_one_step_per_source_line`
/// (which now passes against the recorder's backward-jump detection).
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

    // The Move converter resolves each function-frame local slot to a
    // source-level name by reading the package's compiler-emitted
    // debug-info JSON sidecar at
    //   <package_root>/build/<PackageName>/debug_info/<Module>.json
    // (see `crate::move_debug_info`).  The sidecar's
    // `function_map[binary_member_index].locals` array is in
    // slot-allocation order and the loader strips the compiler-internal
    // `#scope#unique` suffix (e.g. `accumulator#1#0` -> `accumulator`)
    // so the resulting names match the source.  Compiler-generated
    // temps that lack a source-level counterpart (the `let grade = if
    // (...) { 1 } else { 0 }` rhs gets folded into a `%#1` temp because
    // `grade` itself is dead beyond the immediately-following
    // `assert!`) keep their `%#N` form so they're visually distinct
    // from real user bindings.
    //
    // Slot-by-slot mapping for test_loops (pinned by the Sui compiler
    // emitted at `build/flow_test/debug_info/flow_test.json`,
    // `function_map["13"].locals`):
    //   slot 0 -> %#1            (the if-result feeding `grade`; folded)
    //   slot 1 -> accumulator    (reaches 55)
    //   slot 2 -> counter        (reaches 10)
    //   slot 3 -> iterations     (reaches 7)
    //   slot 4 -> power          (reaches 128)
    let must_observe: &[(&str, i64)] = &[
        // grade = 1, surfaced via the compiler-generated `%#1` temp
        ("%#1", 1),
        // accumulator = 55
        ("accumulator", 55),
        // counter = 10
        ("counter", 10),
        // iterations = 7
        ("iterations", 7),
        // power = 128
        ("power", 128),
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
/// `call_entry` events now appear in spec-correct entry order
/// (outermost / first-opened first) — the CTFS multi-stream writer
/// allocates `call_key` at OpenFrame and serialises records in
/// `call_key` order via `flushCompletedCalls`, mirroring every other
/// recorder in the workspace.  Tests below pin that entry order
/// exactly so any future regression toward close-frame LIFO is caught.
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
    // The recorder registers function names at OpenFrame time, so the
    // table order is entry order: outermost test entry first, then each
    // distinct callee in first-call order (compute_triple, max_u64
    // inside compute_triple, then min_u64 from the outer max_u64
    // expression).
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

    // ----- Call-entry sequence (entry order) ------------------------------
    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            // test_nested_calls itself (the outer test entry)
            "test_nested_calls".to_string(),
            // compute_triple(12, 8)
            "compute_triple".to_string(),
            // max_u64(12, 8) inside compute_triple
            "max_u64".to_string(),
            // min_u64(x, y) = min_u64(12, 8)
            "min_u64".to_string(),
            // min_u64(15, 20)
            "min_u64".to_string(),
            // outer max_u64 over the two mins
            "max_u64".to_string(),
        ],
        "call_entry sequence pins the spec-correct entry order"
    );

    // ----- Return values (Int + Tuple + Void) -----------------------------
    // `compute_triple` returns the tuple `(20, 96, 12)` — three u64
    // values.  The recorder now surfaces the *full* tuple as a typed
    // `ValueRecord::Tuple` carrying three `Int` elements, rather than
    // silently truncating to the first element.  See
    // `test_nested_calls_tuple_return_decodes_full_tuple` for the
    // dedicated typed-shape pin.  Exits are emitted in LIFO close order
    // (innermost frame closes first; the outer `test_nested_calls` entry
    // is the last frame still open and closes last — toplevel Return,
    // N+1 model): the inner max_u64(12,8) closes first, then
    // compute_triple, then the two sibling min_u64 calls, then the outer
    // max_u64, then the test entry.
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
            ("test_nested_calls".to_string(), None), // Void, toplevel Return
        ]
    );

    // Strict tuple-shape assertion: kind=Tuple, three Int elements
    // [20, 96, 12].  Pinned exactly so any future regression toward
    // truncation / re-shaping shows up here.  compute_triple closes
    // second (LIFO), so it lands at exits[1].
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
    // arg values for every helper invocation in entry order.  Indexing
    // mirrors `observed_call_sequence` above: 0=test_nested_calls,
    // 1=compute_triple, 2=inner max_u64, 3=first min_u64, 4=second
    // min_u64, 5=outer max_u64.
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
    assert!(
        entries[0]["args"].as_array().unwrap().is_empty(),
        "test_nested_calls itself takes no args"
    );
    assert_eq!(arg_ints(1), vec![12, 8], "compute_triple(12,8)");
    assert_eq!(
        arg_ints(2),
        vec![12, 8],
        "max_u64(12,8) inside compute_triple"
    );
    assert_eq!(arg_ints(3), vec![12, 8], "first min_u64(12,8)");
    assert_eq!(arg_ints(4), vec![15, 20], "second min_u64(15,20)");
    assert_eq!(
        arg_ints(5),
        vec![8, 15],
        "outer max_u64(min_u64(12,8), min_u64(15,20))"
    );

    // ----- The compute_triple decomposition surfaces in the logical step -
    // `compute_triple(12, 8)` yields `(sum=20, product=96, max=12)`.  All
    // three land as typed Int variable snapshots on the line-level logical
    // step that `ct print --full` surfaces (sum -> local_4=20,
    // product -> local_3=96, max -> local_2=12).  The later
    // `scaled = product * SCALE_FACTOR = 9600` write lands on a subsequent
    // column-nudge step (a distinct source statement) that the
    // logical-step view — aligned with logicalStepCount — does not carry.
    let ints = unique_int_pairs(&doc);
    let int_vals: std::collections::BTreeSet<i64> =
        ints.into_iter().map(|(_, v)| v).collect();
    for want in [20_i64, 96, 12] {
        assert!(
            int_vals.contains(&want),
            "expected compute_triple result value {want} in vars; got {int_vals:?}",
        );
    }
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
        vec!["test_vectors".to_string(), "vector_sum".to_string()]
    );

    // ----- Return values --------------------------------------------------
    // LIFO close order: the inner vector_sum closes first; the outer
    // test_vectors entry is the last frame open (toplevel Return) and
    // closes last.
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
            "test_structs".to_string(),
            "add_points".to_string(),
            "rectangle_area".to_string(),
        ]
    );

    // ----- Return values: add_points -> Struct(Point), area=40 -----------
    // Exits emitted in LIFO close order: add_points and rectangle_area
    // (both direct children of the test entry, called in sequence) close
    // in call order as each completes before the next; the outer
    // test_structs entry is the last frame open (toplevel Return) and
    // closes last.
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

    // ----- Every observed Point shape surfaces as Struct ------------------
    // Walk the logical step's vars and collect each Struct's flattened
    // Int-field list.  The (x, y) Point shapes built on the line-level
    // logical step that `ct print --full` surfaces are `[3,4]`, `[7,6]`,
    // and `[10,10]` (the add_points inputs and their sum).  The
    // `Wallet { balance: 1000, id: 1 }` binding is constructed on a
    // subsequent column-nudge step (a distinct source statement) that
    // the logical-step view — aligned with logicalStepCount — does not
    // carry.
    let struct_int_lists = collect_struct_int_lists(&doc);
    for want in [vec![3_i64, 4], vec![7, 6], vec![10, 10]] {
        assert!(
            struct_int_lists.contains(&want),
            "expected Struct field-Int shape `{want:?}` in vars; got Struct shapes = {struct_int_lists:?}"
        );
    }

    // ----- Scalar field values surfacing on the logical step ------------
    // The add_points inputs and their summed Point coordinates surface as
    // typed Int variable snapshots on the line-level logical step that
    // `ct print --full` presents: the two Point operands (3, 4) and
    // (7, 6) and the summed x-coordinate (10).  The later derived scalars
    // (sum_coords=20, area=40, Wallet.balance=1000) are computed on
    // subsequent column-nudge steps (distinct source statements) that the
    // logical-step view — aligned with logicalStepCount — does not carry;
    // the rectangle_area==40 result is already pinned exactly on the
    // rectangle_area call_exit above.
    let ints: std::collections::BTreeSet<i64> =
        unique_int_pairs(&doc).into_iter().map(|(_, v)| v).collect();
    for want in [3, 4, 6, 7, 10] {
        assert!(
            ints.contains(&want),
            "expected scalar value {want} (Point field / summed coord) in vars; got {ints:?}"
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
            "test_references".to_string(),
            "scale_point".to_string(),
            "scale_point".to_string(),
        ]
    );

    let exits = observed_exit_sequence(&doc);
    // Exits in LIFO close order: both scale_point calls (each returning
    // Void since they mutate-through-ref) close first in call order; the
    // outer test_references entry is the last frame open (toplevel
    // Return) and closes last.
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
    // entries[0] is the outer `test_references` entry (no args); the two
    // `scale_point(&mut mut_point, _)` calls land at entries[1] and
    // entries[2] in entry order.
    assert!(
        entries[0]["args"].as_array().unwrap().is_empty(),
        "test_references itself takes no args"
    );
    let (k0, m0, xy0, i0) = scale_args(1);
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
    let (k1, m1, xy1, i1) = scale_args(2);
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
    let address0 = entries[1]["args"][0]["value"]["address"].as_u64();
    let address1 = entries[2]["args"][0]["value"]["address"].as_u64();
    assert!(
        address0.is_some(),
        "Reference must carry a synthetic address"
    );
    assert_eq!(
        address0, address1,
        "both scale_point calls borrow the same `mut_point`; addresses should match"
    );

    // ----- Point shapes surface as typed Structs (incl. mutated copies) --
    // The Move source threads a single Point through two
    // `scale_point(&mut p, _)` calls: `[2,3]` scaled by 5 -> `[10,15]`
    // (via the intermediate `[10,3]` after the x-field write), then
    // scaled by 3 -> `[30,45]`.  Under column-aware step encoding the
    // recorder attaches each register/local write to the column-nudge
    // step current at that instruction; `ct print --full` surfaces the
    // line-level logical step whose variable snapshots span the first
    // scaled result.  The observed typed-Struct Point shapes in that
    // logical step are exactly `[2,3]`, `[10,3]`, and `[10,15]` — the
    // later `[30,15]`/`[30,45]` snapshots live on subsequent
    // column-nudge steps that the logical-step view (aligned with
    // logicalStepCount) does not carry.
    let struct_lists = collect_struct_int_lists(&doc);
    for want in [vec![2_i64, 3], vec![10, 3], vec![10, 15]] {
        assert!(
            struct_lists.contains(&want),
            "expected Point shape `{want:?}` as a typed Struct after mutation; \
             got Struct shapes = {struct_lists:?}"
        );
    }

    // ----- Scalar reads through `&p` surfacing in the logical step -------
    // The scalars carried by the logical step's variable snapshots are
    // the initial fields (2, 3), the scale factor (5), and the
    // first-scaled results (10, 15).
    let ints: std::collections::BTreeSet<i64> =
        unique_int_pairs(&doc).into_iter().map(|(_, v)| v).collect();
    for want in [2, 3, 5, 10, 15] {
        assert!(
            ints.contains(&want),
            "expected scalar value {want} (read through ref / first scale); got {ints:?}"
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
    // Limitation (upstream Sui Move compiler, not the recorder): the
    // source-level binding `let x: u64 = 10;` does NOT surface in the
    // trace because the Sui Move compiler's dead-code-elimination pass
    // drops `x` entirely — the abort branch never reads `x`, so no
    // `LD_U64 10` instruction is emitted into the bytecode and `x`
    // does not occupy any slot in `locals_types`.  Concrete evidence:
    // the compiler-emitted debug info at
    //   `test-programs/move/flow_test/build/flow_test/debug_info/flow_test.json`
    // shows `function_map["19"].locals == [["y#1#0", ...]]` — only
    // `y` survives, `x` is gone.
    //
    // The recorder cannot synthesise values that aren't in the trace
    // it consumes.  Three theoretically possible fixes, all outside
    // the recorder's current contract:
    //   (a) Re-record with a sui CLI built from source with the
    //       constant-folding / DCE passes disabled.  The Sui CLI is
    //       not Nix-packaged in this workspace, so this is blocked
    //       on workspace tooling.
    //   (b) Augment the recorder with a Move source-AST parser
    //       (e.g. via the upstream `move-compiler` crate) and a
    //       trivial-constant evaluator that synthesises `let x: u64
    //       = 10;` as a Write event the trace does not contain.
    //       Adding `move-compiler` would import the entire Move VM
    //       into this crate's dependency tree and is far out of scope.
    //   (c) Document as an inherent VM limitation — which is what
    //       this comment does.
    //
    // Pin the present-day shape here so any unexpected appearance of
    // `10` is caught (would indicate the Sui compiler stopped DCE'ing
    // dead let-bindings — extend the assertion above to require it).
    assert!(
        !int_set.contains(&10),
        "x=10 unexpectedly surfaced in test_abort vars — the upstream \
         Sui Move compiler appears to no longer DCE the dead \
         `let x: u64 = 10;` binding; extend the assertion above to \
         require it.  Got {int_set:?}"
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

    // ----- Call sequence: outer test entry + five fibonacci calls --------
    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "test_fibonacci".to_string(),
            "fibonacci".to_string(),
            "fibonacci".to_string(),
            "fibonacci".to_string(),
            "fibonacci".to_string(),
            "fibonacci".to_string(),
        ]
    );

    // ----- Argument decoding: fibonacci(0,1,5,10,15) ----------------------
    // entries[0] is the outer `test_fibonacci` (no args); the five
    // fibonacci(n) calls land at entries[1..6] in entry order.
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    assert!(
        entries[0]["args"].as_array().unwrap().is_empty(),
        "test_fibonacci itself takes no args"
    );
    let fib_arg = |idx: usize| -> i64 {
        let args = entries[idx]["args"].as_array().expect("args array");
        assert_eq!(args.len(), 1, "fibonacci takes one u64 arg");
        args[0]["value"]["i"].as_i64().expect("arg Int.i")
    };
    assert_eq!(fib_arg(1), 0);
    assert_eq!(fib_arg(2), 1);
    assert_eq!(fib_arg(3), 5);
    assert_eq!(fib_arg(4), 10);
    assert_eq!(fib_arg(5), 15);

    // ----- Return values: F(n) for n in [0,1,5,10,15] = [0,1,5,55,610] ---
    // Exits emitted in LIFO close order: the five sibling fibonacci(n)
    // calls each open and close before the next, so they close in entry
    // order; the outer `test_fibonacci` frame is the last still open and
    // closes last (toplevel Return, N+1 model).
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
/// The `bool` arg of `wrap_value<bool>(true, 2)` now decodes through
/// the typed `ValueRecord::Bool` path (kind="Bool", b=true, text="true"),
/// the same shape every other Move bool flows through.  Pinned exactly
/// below; previously the streaming CBOR encoder for booleans omitted
/// the `text` field and the call-arg path surfaced `value.text == null`.
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
            "test_generics".to_string(),
            "wrap_value".to_string(),
            "unwrap_value".to_string(),
            "wrap_value".to_string(),
            "unwrap_value".to_string(),
            "wrap_value".to_string(),
            "unwrap_value".to_string(),
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
    // Exits in LIFO close order.  The six wrap_value/unwrap_value calls
    // are direct children of the test entry, invoked one after another,
    // so each closes before the next opens — they close in call order.
    // The outer test_generics entry is the last frame open (toplevel
    // Return) and closes last.
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
    // test_generics -> Void (toplevel Return, closes last)
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
    // entries[0] = test_generics (no args); entries[1..] = each helper
    // call in entry order.
    assert!(
        entries[0]["args"].as_array().unwrap().is_empty(),
        "test_generics itself takes no args"
    );
    // wrap_value<u64>(42, 1)
    let wv_u64_args = entries[1]["args"].as_array().unwrap();
    assert_eq!(wv_u64_args[0]["value"]["kind"].as_str(), Some("Int"));
    assert_eq!(wv_u64_args[0]["value"]["i"].as_i64(), Some(42));
    assert_eq!(wv_u64_args[1]["value"]["i"].as_i64(), Some(1));
    // wrap_value<bool>(true, 2)
    let wv_bool_args = entries[3]["args"].as_array().unwrap();
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
    // call_entry events appear in entry order: 0=test_generics,
    // 1=wrap_value<u64>, 2=unwrap_value<u64>, 3=wrap_value<bool>.  The
    // bool generic arg lives on the second wrap_value invocation.
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let wv_bool_args = entries[3]["args"].as_array().unwrap();
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
/// Note: the u128 sum `3_000_000_000_000` fits in i64 (max i64 ≈ 9.2e18),
/// so it would surface as a plain `Int { i: ... }` if the trace
/// preserved the binding.  In practice the Sui VM constant-folds the
/// u128 arithmetic away (see `int_set` assertion below).  The dedicated
/// `test_boolean_and_integers_u128_overflow_uses_bigint` sibling
/// exercises the spec-correct BigInt path on a synthetic NDJSON trace
/// where the u128 magnitude exceeds i64::MAX, confirming the recorder
/// emits `ValueRecord::BigInt` rather than truncating into `i64`.
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
    // Limitation (upstream Sui Move compiler, not the recorder): the
    // captured trace
    // (`flow_test__flow_test__test_boolean_and_integers.json.zst`) does
    // NOT contain any `U8` or `U128` values — the Sui Move compiler's
    // constant-folding + dead-code-elimination passes drop the
    // small_a/small_b/small_sum and big_a/big_b/big_sum let-bindings
    // because their results only feed `assert!` calls whose conditions
    // const-fold to `true`.  Concrete evidence: the compiler-emitted
    // debug info at
    //   `test-programs/move/flow_test/build/flow_test/debug_info/flow_test.json`
    // shows `function_map["18"].locals == ["%#1", "%#2", "%#3", "%#4",
    //   "and_result", "f", "not_result", "or_result", "t"]` —
    // none of the U8/U128 source bindings survive, and `status` itself
    // gets folded into one of the `%#N` compiler temps.  The only Int
    // that surfaces in the trace is the final `1` (the value bound to
    // the if-result temp for `let status: u64 = if (or_result &&
    // !and_result) { 1 } else { 0 }`).
    //
    // Three theoretically possible fixes, all outside the recorder's
    // current contract:
    //   (a) Re-record with a sui CLI built from source with the DCE +
    //       constant-folding passes disabled.  The Sui CLI is not
    //       Nix-packaged in this workspace, so this is blocked on
    //       workspace tooling.
    //   (b) Pull the `move-compiler` crate in and synthesise Write
    //       events for trivially-constant let-bindings via an AST-level
    //       evaluator.  This would import the entire Move VM into this
    //       crate's dependency tree and is far out of scope.
    //   (c) Document as an inherent VM limitation — which is what this
    //       comment does.
    //
    // The dedicated `test_boolean_and_integers_u128_overflow_uses_bigint`
    // sibling feeds a synthetic NDJSON to confirm the BigInt path
    // independently of this re-recording limitation.  Pin the shape so
    // any future capture-of-dead-bindings shows up here.
    let int_set: std::collections::BTreeSet<i64> =
        unique_int_pairs(&doc).into_iter().map(|(_, v)| v).collect();
    assert_eq!(
        int_set,
        std::collections::BTreeSet::from([1_i64]),
        "Only status=1 survives the Sui Move compiler's constant-folding \
         + DCE for this fixture; if more Int values now appear, extend \
         this assertion to require them"
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
            "test_variant_constructors".to_string(),
            "make_some".to_string(),
            "make_none".to_string(),
            "make_rect".to_string(),
        ],
    );

    // ----- Return values: each helper returns a typed Variant -------------
    // Exits in LIFO close order: the three constructors (direct children
    // of the test entry, invoked in sequence) close in call order; the
    // outer test_variant_constructors entry is the last frame open
    // (toplevel Return) and closes last.
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

    // test_variant_constructors -> Void (toplevel Return, closes last).
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
    // At least one typed Variant ValueRecord surfaces on the line-level
    // logical step that `ct print --full` presents (the `make_some(42)`
    // result binding).  The `make_none` and `make_rect` bindings, and the
    // `area = 3 * 5 = 15` computation, are materialised on subsequent
    // column-nudge steps (distinct source statements) that the
    // logical-step view — aligned with logicalStepCount — does not carry;
    // their typed shapes are already pinned exactly on the call_exit
    // return values above.
    assert!(
        variant_count >= 1,
        "expected at least 1 Variant ValueRecord in the logical step's vars; \
         got {variant_count}",
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
        vec!["test_wide_integer".to_string(), "wide_product".to_string()],
    );

    // ----- wide_product's args carry every integer width -----------------
    // entries[0] is the outer test entry (no args); wide_product is at
    // entries[1].
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let wide_args = entries[1]["args"].as_array().expect("args array");
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
    // Exits in LIFO close order: the inner wide_product closes first; the
    // outer test_wide_integer entry is the last frame open (toplevel
    // Return) and closes last.
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

    // test_wide_integer -> Void (toplevel Return, closes last).
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
            "test_resources".to_string(),
            "mint".to_string(),
            "balance".to_string(),
            "burn".to_string(),
        ],
    );

    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 4);

    // Exits in LIFO close order: the three helpers (direct children of
    // the test entry, invoked in sequence) close in call order; the
    // outer test_resources entry is the last frame open (toplevel
    // Return) and closes last.
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

    // test_resources -> Void (toplevel Return, closes last).
    assert_eq!(exits[3].0, "test_resources");
    assert_eq!(exits[3].1["kind"].as_str(), Some("Void"));

    // ----- The `&Coin` arg to balance() is a Reference wrapping a Struct ---
    // entries[0]=test_resources, entries[1]=mint, entries[2]=balance,
    // entries[3]=burn (entry order).
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let bal_arg0 = &entries[2]["args"][0]["value"];
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
            "test_object_lifecycle".to_string(),
            "increment".to_string(),
            "value".to_string(),
        ],
    );

    // ----- The io event for Transfer ------------------------------------
    let io = events
        .iter()
        .find(|e| e["kind"] == "io")
        .expect("expected an `io` event for the External::Transfer side effect");
    assert_eq!(io["text"].as_str(), Some("Transfer"));

    // ----- &mut Counter and &Counter args carry nested Struct payload ---
    // entries[0]=test_object_lifecycle (no args), entries[1]=increment,
    // entries[2]=value (entry order).
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    // increment(&mut Counter, by) -> Void
    let inc_args = entries[1]["args"].as_array().expect("args array");
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
    let val_args = entries[2]["args"].as_array().expect("args array");
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

    // ----- Return values (LIFO close order) ------------------------------
    // increment and value (direct children of the test entry, invoked in
    // sequence) close in call order; the outer test_object_lifecycle
    // entry is the last frame open (toplevel Return) and closes last.
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
            "test_abilities".to_string(),
            "mint_token".to_string(),
            "consume_token".to_string(),
            "destroy_storage_item".to_string(),
        ],
    );

    // ----- mint_token(7) -> AccessToken { operation_id: 7 } ---------------
    // Exits in LIFO close order: the three helpers (direct children of
    // the test entry, invoked in sequence) close in call order; the
    // outer test_abilities entry is the last frame open (toplevel
    // Return) and closes last.
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
    // identity flows through.  In entry order: entries[0]=test_abilities,
    // [1]=mint_token, [2]=consume_token, [3]=destroy_storage_item.
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let consume_arg = &entries[2]["args"][0]["value"];
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

    // ----- test_abilities -> Void (toplevel Return, closes last) ---------
    assert_eq!(exits[3].0, "test_abilities");
    assert_eq!(exits[3].1["kind"].as_str(), Some("Void"));

    // ----- The AccessToken { operation_id: 7 } shape surfaces as a typed
    //       Struct on the logical step -------------------------------------
    // The mint_token(7) result binding materialises on the line-level
    // logical step that `ct print --full` surfaces, as a typed
    // `AccessToken { operation_id: 7 }` Struct.  The `Datum { x: 42 }`
    // copies and the `StorageItem { payload: 99 }` binding are
    // constructed on subsequent column-nudge steps (distinct source
    // statements) that the logical-step view — aligned with
    // logicalStepCount — does not carry; their typed shapes are already
    // pinned exactly on the call_exit return values above.
    let struct_lists = collect_struct_int_lists(&doc);
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

    // Entry-order call sequence.  borrow_inner is called *before*
    // option::borrow inside the test source (the inner helper invokes
    // option::borrow internally, but its OpenFrame fires first).
    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "test_option".to_string(),
            "some".to_string(),
            "none".to_string(),
            "is_some".to_string(),
            "is_none".to_string(),
            "borrow_inner".to_string(),
            "borrow".to_string(),
            "extract".to_string(),
        ],
    );

    // ----- Return values pinned exactly (LIFO close order) ---------------
    // some/none/is_some/is_none are sibling helpers that close in call
    // order; borrow_inner *calls* option::borrow internally, so the inner
    // `borrow` frame closes before its `borrow_inner` parent (LIFO);
    // extract closes next; the outer test_option entry is the last frame
    // open (toplevel Return) and closes last.
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

    // option::borrow(&Some(42)) -> &u64 — typed ValueRecord::Reference.
    // borrow is called *from inside* borrow_inner, so it closes first.
    assert_eq!(exits[4].0, "borrow");
    let borrow_rv = &exits[4].1;
    assert_eq!(borrow_rv["kind"].as_str(), Some("Reference"));
    assert_eq!(borrow_rv["mutable"].as_bool(), Some(false));
    assert_eq!(borrow_rv["dereferenced"]["kind"].as_str(), Some("Int"));
    assert_eq!(borrow_rv["dereferenced"]["i"].as_i64(), Some(42));

    // borrow_inner is the outer helper wrapping option::borrow: Int(42).
    assert_eq!(exits[5].0, "borrow_inner");
    assert_eq!(exits[5].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[5].1["i"].as_i64(), Some(42));

    // extract -> Int(42)
    assert_eq!(exits[6].0, "extract");
    assert_eq!(exits[6].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[6].1["i"].as_i64(), Some(42));

    // test_option -> Void (toplevel Return, closes last).
    assert_eq!(exits[7].0, "test_option");
    assert_eq!(exits[7].1["kind"].as_str(), Some("Void"));

    // ----- Reference-typed call args carry the typed Variant pointee -----
    // entries[0]=test_option (no args); helpers at entries[1..] in entry
    // order: 1=some, 2=none, 3=is_some, 4=is_none, 5=borrow, 6=borrow_inner,
    // 7=extract.
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    // is_some takes &Option<u64> wrapping the Some(42) variant.
    let is_some_arg = &entries[3]["args"][0]["value"];
    assert_eq!(is_some_arg["kind"].as_str(), Some("Reference"));
    assert_eq!(is_some_arg["mutable"].as_bool(), Some(false));
    let is_some_pointee = &is_some_arg["dereferenced"];
    assert_eq!(is_some_pointee["kind"].as_str(), Some("Variant"));
    assert_eq!(
        is_some_pointee["discriminator"].as_str(),
        Some("0x1::option::Option::Variant#1"),
    );
    // is_none takes &Option<u64> wrapping the None variant.
    let is_none_arg = &entries[4]["args"][0]["value"];
    assert_eq!(is_none_arg["kind"].as_str(), Some("Reference"));
    assert_eq!(is_none_arg["mutable"].as_bool(), Some(false));
    let is_none_pointee = &is_none_arg["dereferenced"];
    assert_eq!(is_none_pointee["kind"].as_str(), Some("Variant"));
    assert_eq!(
        is_none_pointee["discriminator"].as_str(),
        Some("0x1::option::Option::Variant#0"),
    );
    // extract takes &mut Option<u64>.
    let extract_arg = &entries[7]["args"][0]["value"];
    assert_eq!(extract_arg["kind"].as_str(), Some("Reference"));
    assert_eq!(extract_arg["mutable"].as_bool(), Some(true));

    // ----- The Some(42) variant surfaces on the logical step ------------
    // The `option::some(42)` result (Variant#1) surfaces as a typed
    // `ValueRecord::Variant` `stack_top` snapshot on the line-level
    // logical step that `ct print --full` presents.  The post-extract
    // `None` (Variant#0) write to local_0 materialises on a subsequent
    // column-nudge step (a distinct source statement) that the
    // logical-step view — aligned with logicalStepCount — does not carry;
    // the None shape is already pinned exactly on the `none` call_exit
    // return value above.
    let mut saw_some_variant = false;
    for (_name, value) in collect_step_vars(
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
        if value["kind"] == "Variant"
            && value["discriminator"] == "0x1::option::Option::Variant#1"
        {
            saw_some_variant = true;
        }
    }
    assert!(
        saw_some_variant,
        "expected a Some (Variant#1) typed Variant ValueRecord on the logical step",
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
            "test_event_emit".to_string(),
            "fire".to_string(),
            "emit".to_string(),
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
    // entries[0]=test_event_emit, [1]=fire, [2]=emit (entry order).
    let entries: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let emit_args = entries[2]["args"].as_array().expect("emit args");
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

    // ----- Return values (LIFO close order) ------------------------------
    // emit is called from inside fire, so the inner emit frame closes
    // first, then fire; the outer test_event_emit entry is the last frame
    // open (toplevel Return) and closes last.
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
            "test_hash_builtins".to_string(),
            "to_bytes".to_string(),
            "sha2_256".to_string(),
            "to_bytes".to_string(),
            "sha3_256".to_string(),
            "length".to_string(),
            "length".to_string(),
        ],
    );

    // ----- Each native return surfaces as Sequence<u8> with exact bytes --
    // Exits in LIFO close order: the six helpers are direct children of
    // the test entry invoked in sequence, so they close in call order:
    // 0=to_bytes, 1=sha2_256, 2=to_bytes, 3=sha3_256, 4=length, 5=length.
    // The outer test_hash_builtins entry is the last frame open (toplevel
    // Return) and closes last (index 6).
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

    // test_hash_builtins -> Void (toplevel Return, closes last).
    assert_eq!(exits[6].0, "test_hash_builtins");
    assert_eq!(exits[6].1["kind"].as_str(), Some("Void"));

    // ----- The hash arg also surfaces as the same Sequence<u8> ----------
    // Entry order: entries[0]=test_hash_builtins, [1]=to_bytes(1),
    // [2]=sha2_256, [3]=to_bytes(2), [4]=sha3_256, [5]=length, [6]=length.
    let entries: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    // sha2_256(bytes) — the bytes arg is the bcs output (entries[2]).
    let sha2_arg0 = &entries[2]["args"][0]["value"];
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
    // sha3_256(bytes2) — entries[4], same payload.
    let sha3_arg0 = &entries[4]["args"][0]["value"];
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
    // The first to_bytes call is at entries[1] in entry order.
    let bcs_arg0 = &entries[1]["args"][0]["value"];
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
            "test_string".to_string(),
            "utf8".to_string(),
            "utf8".to_string(),
            "append".to_string(),
            "sub_string".to_string(),
            "length".to_string(),
            "length".to_string(),
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

    // Exits in LIFO close order: the six helpers are direct children of
    // the test entry invoked in sequence, so they close in call order:
    // 0=utf8, 1=utf8, 2=append, 3=sub_string, 4=length, 5=length. The
    // outer test_string entry is the last frame open (toplevel Return)
    // and closes last (index 6).
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

    // test_string -> Void (toplevel Return, closes last).
    assert_eq!(exits[6].0, "test_string");
    assert_eq!(exits[6].1["kind"].as_str(), Some("Void"));

    // ----- After append, the &mut s arg snapshot is "hello world" -------
    // entries[0]=test_string, [1]=utf8, [2]=utf8, [3]=append,
    // [4]=sub_string, [5]=length, [6]=length (entry order).
    let entries: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    // sub_string(&s, 0, 5) — its first arg is a Reference whose pointee
    // String must spell out "hello world" after the in-place append.
    let sub_arg0 = &entries[4]["args"][0]["value"];
    assert_eq!(sub_arg0["kind"].as_str(), Some("Reference"));
    assert_eq!(sub_arg0["mutable"].as_bool(), Some(false));
    assert_eq!(string_struct_text(&sub_arg0["dereferenced"]), "hello world");
    // length(&s) — same shape.
    let len_arg0 = &entries[5]["args"][0]["value"];
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
            "test_vector_operations".to_string(),
            "swap_remove".to_string(),
            "pop_back".to_string(),
            "contains".to_string(),
            "contains".to_string(),
            "reverse".to_string(),
            "append".to_string(),
            "index_of".to_string(),
            "borrow_mut".to_string(),
            "borrow".to_string(),
        ],
    );

    // ----- Return values pinned exactly (LIFO close order) --------------
    // The nine vector-op helpers are direct children of the test entry
    // invoked in sequence, so they close in call order at indices 0..9;
    // the outer test_vector_operations entry is the last frame open
    // (toplevel Return) and closes last (index 9).
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

    // test_vector_operations -> Void (toplevel Return, closes last).
    assert_eq!(exits[9].0, "test_vector_operations");
    assert_eq!(exits[9].1["kind"].as_str(), Some("Void"));

    // ----- Reference args carry the contents snapshot at call time ------
    // entries[0]=test_vector_operations (no args); helpers at 1..10 in
    // entry order, mirroring observed_call_sequence above.
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
        extract_seq(&entries[1]["args"][0]["value"]),
        vec![10_i64, 20, 30, 40],
    );
    // pop_back sees v = [10, 40, 30] (after swap_remove)
    assert_eq!(
        extract_seq(&entries[2]["args"][0]["value"]),
        vec![10_i64, 40, 30],
    );
    // contains(_, 40) sees v = [10, 40] (after pop_back)
    assert_eq!(
        extract_seq(&entries[3]["args"][0]["value"]),
        vec![10_i64, 40],
    );
    // contains(_, 99) sees the same v = [10, 40]
    assert_eq!(
        extract_seq(&entries[4]["args"][0]["value"]),
        vec![10_i64, 40],
    );
    // reverse sees v = [10, 40]
    assert_eq!(
        extract_seq(&entries[5]["args"][0]["value"]),
        vec![10_i64, 40],
    );
    // append sees v = [40, 10] (after reverse) and other = [7, 8]
    assert_eq!(
        extract_seq(&entries[6]["args"][0]["value"]),
        vec![40_i64, 10],
    );
    assert_eq!(extract_seq(&entries[6]["args"][1]["value"]), vec![7_i64, 8],);
    // index_of sees v = [40, 10, 7, 8] (after append)
    assert_eq!(
        extract_seq(&entries[7]["args"][0]["value"]),
        vec![40_i64, 10, 7, 8],
    );
    // borrow_mut sees the same v
    assert_eq!(
        extract_seq(&entries[8]["args"][0]["value"]),
        vec![40_i64, 10, 7, 8],
    );
    // borrow (final readback) sees v = [100, 10, 7, 8] (after *r = 100)
    assert_eq!(
        extract_seq(&entries[9]["args"][0]["value"]),
        vec![100_i64, 10, 7, 8],
    );

    // ----- The initial contents snapshot surfaces as a typed Sequence in
    //       the logical step's vars.
    // Each mutating op's post-write `v` snapshot is materialised on the
    // column-nudge step current at that instruction; `ct print --full`
    // surfaces the line-level logical step whose variable snapshots carry
    // the initial `v = [10, 20, 30, 40]` as a typed Sequence.  The later
    // per-op snapshots ([10,40,30], [40,10], [100,10,7,8], ...) live on
    // subsequent column-nudge steps that the logical-step view — aligned
    // with logicalStepCount — does not carry; each op's runtime effect is
    // already pinned exactly through the call-arg contents snapshots above.
    let seq_lists = collect_sequence_int_lists(&doc);
    assert!(
        seq_lists.contains(&vec![10_i64, 20, 30, 40]),
        "expected initial v snapshot [10, 20, 30, 40] as a typed Sequence in \
         step vars; got {seq_lists:?}",
    );
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

// ===========================================================================
// test_phantom_types — phantom type parameters surface as distinct TypeIds
// ===========================================================================

/// Records `flow_test::test_phantom_types` (synthetic NDJSON).
///
/// Pins that two `TypedCoin<phantom T>` instantiations with the *same*
/// runtime layout (`u64` payload of `100`) but distinct phantom tags
/// (`USD`, `EUR`) register two distinguishable `TypeKind::Struct`
/// `TypeId`s in the type table — the recorder must key the type-table
/// entry by `(struct_name, type_args)` rather than by the bare struct
/// name, otherwise the phantom currency tag is lost when the value
/// flows through `mint`/`value`/`burn`.  Also pins that the
/// `TypeId`s for the bare `USD` and `EUR` phantom-tag structs (no
/// fields, no arguments) are themselves distinct from each other and
/// from the parameterised coin types.
#[test]
fn test_phantom_types_test_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_phantom_types_test_via_ct_print_full",
        "test_phantom_types",
        flow_test_named_source("phantom_types_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "phantom_types_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        functions,
        vec!["test_phantom_types", "mint", "value", "burn"],
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
            "test_phantom_types".to_string(),
            "mint".to_string(),
            "mint".to_string(),
            "value".to_string(),
            "value".to_string(),
            "burn".to_string(),
            "burn".to_string(),
        ],
    );

    // Exits in LIFO close order: the six helpers are direct children of
    // the test entry invoked in sequence, so they close in call order at
    // indices 0..6; the outer test_phantom_types entry is the last frame
    // open (toplevel Return) and closes last (index 6).
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 7);

    // ----- mint<USD>(100) -> TypedCoin<USD> { value: 100 } ---------------
    assert_eq!(exits[0].0, "mint");
    let usd_coin = &exits[0].1;
    assert_eq!(usd_coin["kind"].as_str(), Some("Struct"));
    let usd_fields = usd_coin["field_values"]
        .as_array()
        .expect("Struct.field_values");
    assert_eq!(usd_fields.len(), 1);
    assert_eq!(usd_fields[0]["kind"].as_str(), Some("Int"));
    assert_eq!(usd_fields[0]["i"].as_i64(), Some(100));
    let usd_coin_type_id = usd_coin["type_id"].as_u64().expect("Struct.type_id");

    // ----- mint<EUR>(100) -> TypedCoin<EUR> { value: 100 } ---------------
    assert_eq!(exits[1].0, "mint");
    let eur_coin = &exits[1].1;
    assert_eq!(eur_coin["kind"].as_str(), Some("Struct"));
    let eur_fields = eur_coin["field_values"]
        .as_array()
        .expect("Struct.field_values");
    assert_eq!(eur_fields.len(), 1);
    assert_eq!(eur_fields[0]["kind"].as_str(), Some("Int"));
    assert_eq!(eur_fields[0]["i"].as_i64(), Some(100));
    let eur_coin_type_id = eur_coin["type_id"].as_u64().expect("Struct.type_id");

    // The phantom-tag-aware key must produce DISTINCT type ids for
    // TypedCoin<USD> and TypedCoin<EUR> even though their runtime
    // layouts are identical (both pack a single `u64` field).
    assert_ne!(
        usd_coin_type_id, eur_coin_type_id,
        "TypedCoin<USD> and TypedCoin<EUR> must register as distinct \
         TypeKind::Struct ids — phantom-tag distinctness is the whole \
         point of this fixture",
    );

    // ----- value<USD>(&usd_coin) and value<EUR>(&eur_coin) ---------------
    assert_eq!(exits[2].0, "value");
    assert_eq!(exits[2].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[2].1["i"].as_i64(), Some(100));
    assert_eq!(exits[3].0, "value");
    assert_eq!(exits[3].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[3].1["i"].as_i64(), Some(100));

    // ----- burn<USD>(usd_coin) -> 100, burn<EUR>(eur_coin) -> 100 -------
    assert_eq!(exits[4].0, "burn");
    assert_eq!(exits[4].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[4].1["i"].as_i64(), Some(100));
    assert_eq!(exits[5].0, "burn");
    assert_eq!(exits[5].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[5].1["i"].as_i64(), Some(100));

    // ----- test_phantom_types -> Void (toplevel Return, closes last) ----
    assert_eq!(exits[6].0, "test_phantom_types");
    assert_eq!(exits[6].1["kind"].as_str(), Some("Void"));

    // ----- value<USD>'s &TypedCoin<USD> arg keeps the phantom-tagged id -
    // entries[0]=test_phantom_types, [1]=mint<USD>, [2]=mint<EUR>,
    // [3]=value<USD>, [4]=value<EUR>, [5]=burn<USD>, [6]=burn<EUR>.
    let entries: Vec<&serde_json::Value> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let usd_value_arg = &entries[3]["args"][0]["value"];
    assert_eq!(usd_value_arg["kind"].as_str(), Some("Reference"));
    assert_eq!(usd_value_arg["mutable"].as_bool(), Some(false));
    let usd_pointee = &usd_value_arg["dereferenced"];
    assert_eq!(usd_pointee["kind"].as_str(), Some("Struct"));
    assert_eq!(
        usd_pointee["type_id"].as_u64(),
        Some(usd_coin_type_id),
        "the &TypedCoin<USD> pointee must carry the same TypedCoin<USD> \
         type id minted by mint<USD>",
    );

    let eur_value_arg = &entries[4]["args"][0]["value"];
    assert_eq!(eur_value_arg["kind"].as_str(), Some("Reference"));
    let eur_pointee = &eur_value_arg["dereferenced"];
    assert_eq!(
        eur_pointee["type_id"].as_u64(),
        Some(eur_coin_type_id),
        "the &TypedCoin<EUR> pointee must carry the same TypedCoin<EUR> \
         type id minted by mint<EUR>",
    );

    // ----- burn's owned TypedCoin<T> args also keep their phantom ids ---
    let burn_usd_arg = &entries[5]["args"][0]["value"];
    assert_eq!(burn_usd_arg["kind"].as_str(), Some("Struct"));
    assert_eq!(
        burn_usd_arg["type_id"].as_u64(),
        Some(usd_coin_type_id),
        "burn<USD>'s owned arg must carry the TypedCoin<USD> type id",
    );
    let burn_eur_arg = &entries[6]["args"][0]["value"];
    assert_eq!(burn_eur_arg["kind"].as_str(), Some("Struct"));
    assert_eq!(
        burn_eur_arg["type_id"].as_u64(),
        Some(eur_coin_type_id),
        "burn<EUR>'s owned arg must carry the TypedCoin<EUR> type id",
    );

    // ----- A TypedCoin<T> { value: 100 } shape surfaces on the logical
    //       step --------------------------------------------------------
    // The first mint(100) result binding materialises a typed
    // `TypedCoin<T> { value: 100 }` Struct on the line-level logical step
    // that `ct print --full` presents.  The remaining mint/value/burn
    // frames' TypedCoin snapshots materialise on subsequent column-nudge
    // steps (distinct source statements) that the logical-step view —
    // aligned with logicalStepCount — does not carry; the per-frame typed
    // shapes and their phantom-distinct `type_id`s are already pinned
    // exactly on the call_exit return values above.
    let struct_lists = collect_struct_int_lists(&doc);
    assert_eq!(struct_lists, vec![vec![100_i64]]);
}

// ===========================================================================
// test_signer — &signer permission-checking shape (Aptos)
// ===========================================================================

/// Records `flow_test::test_signer` (synthetic NDJSON).
///
/// Pins the recorder's surface for the canonical Aptos access-check
/// pattern: `authorize(admin: &signer, target: address): bool` calls
/// `signer::address_of(admin)` and compares the result against a
/// hard-coded admin constant.  The `&signer` parameter must surface as
/// a typed `ValueRecord::Reference { mutable: false, .. }` whose
/// dereferenced pointee is a typed `Signer` `ValueRecord::Struct`
/// carrying a single `address` field, the address-of native return
/// must surface as a typed `ValueRecord::String` (the recorder's
/// canonical address shape), and the boolean comparison verdict must
/// surface as a typed `ValueRecord::Bool`.
#[test]
fn test_signer_test_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_signer_test_via_ct_print_full",
        "test_signer",
        flow_test_named_source("signer_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "signer_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(functions, vec!["authorize", "address_of"]);

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
        vec!["authorize".to_string(), "address_of".to_string()],
    );

    // ----- authorize takes (admin: &signer, target: address) -------------
    let entries: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    // Entry order: entries[0]=authorize, entries[1]=address_of (called
    // by authorize internally).
    let authorize_args = entries[0]["args"].as_array().expect("authorize args");
    assert_eq!(
        authorize_args.len(),
        2,
        "authorize takes (&signer, address)"
    );

    // arg0: &signer -> Reference (immutable) wrapping a typed Signer struct.
    let signer_arg = &authorize_args[0]["value"];
    assert_eq!(signer_arg["kind"].as_str(), Some("Reference"));
    assert_eq!(signer_arg["mutable"].as_bool(), Some(false));
    let signer_pointee = &signer_arg["dereferenced"];
    assert_eq!(
        signer_pointee["kind"].as_str(),
        Some("Struct"),
        "&signer's dereferenced pointee must be a typed Signer Struct",
    );
    let signer_fields = signer_pointee["field_values"]
        .as_array()
        .expect("Signer.field_values");
    assert_eq!(
        signer_fields.len(),
        1,
        "Signer carries a single address field"
    );
    assert_eq!(signer_fields[0]["kind"].as_str(), Some("String"));
    assert_eq!(signer_fields[0]["text"].as_str(), Some("0xA11CE"));

    // arg1: address `target` -> typed String (the recorder's address shape).
    let target_arg = &authorize_args[1]["value"];
    assert_eq!(target_arg["kind"].as_str(), Some("String"));
    assert_eq!(target_arg["text"].as_str(), Some("0xBEEF"));

    // ----- address_of(admin) takes the same &signer pointee ------------
    let address_of_args = entries[1]["args"].as_array().expect("address_of args");
    assert_eq!(address_of_args.len(), 1);
    let inner_signer = &address_of_args[0]["value"];
    assert_eq!(inner_signer["kind"].as_str(), Some("Reference"));
    assert_eq!(inner_signer["mutable"].as_bool(), Some(false));
    assert_eq!(
        inner_signer["dereferenced"]["field_values"][0]["text"].as_str(),
        Some("0xA11CE"),
    );

    // ----- Return values (LIFO close order) ------------------------------
    // address_of is called from inside authorize (the entry function
    // here — this fixture has no synthetic outer test frame), so the
    // inner address_of frame closes first and authorize closes last.
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 2);

    // address_of returns the admin address as a typed String.
    assert_eq!(exits[0].0, "address_of");
    assert_eq!(exits[0].1["kind"].as_str(), Some("String"));
    assert_eq!(exits[0].1["text"].as_str(), Some("0xA11CE"));

    // authorize returns the boolean comparison verdict — true.
    assert_eq!(exits[1].0, "authorize");
    assert_eq!(exits[1].1["kind"].as_str(), Some("Bool"));
    assert_eq!(exits[1].1["b"].as_bool(), Some(true));
    assert_eq!(exits[1].1["text"].as_str(), Some("true"));

    // ----- The target address surfaces in the logical step -------------
    // The `target: address` argument (@0xBEEF) surfaces as the typed
    // String `arg1` on the line-level logical step that `ct print --full`
    // presents.  The boolean comparison verdict
    // (`signer::address_of(admin) == @0xA11CE`) is computed on subsequent
    // column-nudge steps (distinct source statements) whose `stack_top` /
    // `local_3` Bool snapshots the logical-step view — aligned with
    // logicalStepCount — does not carry; the verdict is already pinned
    // exactly on the authorize call_exit return value above.
    let raws = unique_raw_pairs(&doc);
    assert_eq!(raws, vec![("arg1".to_string(), "0xBEEF".to_string())]);
    let bools = unique_bool_pairs(&doc);
    assert!(
        bools.is_empty(),
        "the boolean verdict lands on column-nudge steps, so no Bool surfaces \
         on the logical step; got {bools:?}",
    );
}

// ===========================================================================
// test_tx_context — Sui's `&mut TxContext` shape
// ===========================================================================

/// Records `flow_test::test_tx_context` (synthetic NDJSON).
///
/// Pins the recorder's surface for the canonical Sui entry-function
/// shape: `mint(ctx: &mut TxContext): Token` calls
/// `tx_context::sender(ctx)` to recover the transaction sender and
/// `object::new(ctx)` to derive a fresh UID for the new resource.
/// The `&mut TxContext` parameter must surface as a typed
/// `ValueRecord::Reference { mutable: true, .. }`; the recovered
/// sender as a typed address-shaped `ValueRecord::String`; and the
/// minted `UID` as a typed `ValueRecord::Struct` whose nested
/// `ID { bytes: address }` payload preserves the byte-vector identity.
#[test]
fn test_tx_context_test_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_tx_context_test_via_ct_print_full",
        "test_tx_context",
        flow_test_named_source("tx_context_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "tx_context_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(functions, vec!["mint", "sender", "new"]);

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
        vec!["mint".to_string(), "sender".to_string(), "new".to_string(),],
    );

    let entries: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();

    // ----- mint(ctx: &mut TxContext) — the &mut TxContext arg shape -----
    // Entry order: entries[0]=mint, entries[1]=sender (called by mint),
    // entries[2]=new (also called by mint).
    let mint_args = entries[0]["args"].as_array().expect("mint args");
    assert_eq!(mint_args.len(), 1, "mint takes a single &mut TxContext arg");
    let ctx_arg = &mint_args[0]["value"];
    assert_eq!(ctx_arg["kind"].as_str(), Some("Reference"));
    assert_eq!(
        ctx_arg["mutable"].as_bool(),
        Some(true),
        "&mut TxContext must surface with mutable=true",
    );
    let ctx_pointee = &ctx_arg["dereferenced"];
    assert_eq!(ctx_pointee["kind"].as_str(), Some("Struct"));
    let ctx_fields = ctx_pointee["field_values"]
        .as_array()
        .expect("TxContext.field_values");
    assert_eq!(
        ctx_fields.len(),
        5,
        "TxContext fields: sender, tx_hash, epoch, epoch_timestamp_ms, ids_created",
    );
    // sender field is the typed address String.
    assert_eq!(ctx_fields[0]["kind"].as_str(), Some("String"));
    assert_eq!(ctx_fields[0]["text"].as_str(), Some("0xCAFE"));

    // tx_context::sender(ctx) takes the same &mut TxContext.
    let sender_args = entries[1]["args"].as_array().expect("sender args");
    assert_eq!(sender_args.len(), 1);
    assert_eq!(sender_args[0]["value"]["kind"].as_str(), Some("Reference"));
    assert_eq!(sender_args[0]["value"]["mutable"].as_bool(), Some(true));

    // object::new(ctx) takes the same &mut TxContext.
    let new_args = entries[2]["args"].as_array().expect("new args");
    assert_eq!(new_args.len(), 1);
    assert_eq!(new_args[0]["value"]["kind"].as_str(), Some("Reference"));
    assert_eq!(new_args[0]["value"]["mutable"].as_bool(), Some(true));

    // ----- Return values (LIFO close order) ------------------------------
    // mint is the entry function here (no synthetic outer test frame); it
    // calls tx_context::sender then object::new, each of which closes
    // before mint's own frame.  So the inner sender and new close first
    // (in call order), and mint closes last.
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 3);

    // tx_context::sender returns the sender address as a typed String.
    assert_eq!(exits[0].0, "sender");
    assert_eq!(exits[0].1["kind"].as_str(), Some("String"));
    assert_eq!(exits[0].1["text"].as_str(), Some("0xCAFE"));

    // object::new returns a fresh UID as a typed Struct whose inner
    // ID { bytes: address } preserves the byte-vector identity.
    assert_eq!(exits[1].0, "new");
    let uid_rv = &exits[1].1;
    assert_eq!(uid_rv["kind"].as_str(), Some("Struct"));
    let uid_fields = uid_rv["field_values"].as_array().expect("UID.field_values");
    assert_eq!(uid_fields.len(), 1, "UID {{ id: ID }}");
    assert_eq!(uid_fields[0]["kind"].as_str(), Some("Struct"));
    let id_fields = uid_fields[0]["field_values"]
        .as_array()
        .expect("ID.field_values");
    assert_eq!(id_fields.len(), 1, "ID {{ bytes: address }}");
    assert_eq!(id_fields[0]["kind"].as_str(), Some("String"));
    assert_eq!(id_fields[0]["text"].as_str(), Some("0xFEED"));
    let uid_type_id = uid_rv["type_id"].as_u64().expect("UID.type_id");

    // mint returns Token { id: UID } — nested struct shape; mint closes
    // last (entry function).
    assert_eq!(exits[2].0, "mint");
    let token_rv = &exits[2].1;
    assert_eq!(token_rv["kind"].as_str(), Some("Struct"));
    let token_fields = token_rv["field_values"]
        .as_array()
        .expect("Token.field_values");
    assert_eq!(token_fields.len(), 1);
    assert_eq!(token_fields[0]["kind"].as_str(), Some("Struct"));
    assert_eq!(
        token_fields[0]["type_id"].as_u64(),
        Some(uid_type_id),
        "Token.id must share the UID type id minted by object::new",
    );
    let inner_id_fields = token_fields[0]["field_values"]
        .as_array()
        .expect("nested UID.field_values");
    assert_eq!(
        inner_id_fields[0]["field_values"][0]["text"].as_str(),
        Some("0xFEED"),
    );
}

// ===========================================================================
// test_friend_visibility — `friend` declaration + `public(friend) fun`
// ===========================================================================

/// Records `flow_test::test_friend_visibility` (synthetic NDJSON).
///
/// Pins that a cross-module call from a friend caller (`auth::query`)
/// to a `public(friend)` callee (`secrets::reveal`) surfaces as a
/// normal Call/Return pair across the friend boundary, and that the
/// `reveal` function appears in the function table with its full
/// module-qualified name (`secrets::reveal`) — preserving the owning
/// module across a cross-user-code call.  Same-module helpers
/// (`auth::query` from the toplevel `auth::test_friend_visibility`)
/// retain their bare names so the existing M5–M8 function-table
/// conventions stay intact.
#[test]
fn test_friend_visibility_test_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_friend_visibility_test_via_ct_print_full",
        "test_friend_visibility",
        flow_test_named_source("friend_visibility_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "friend_visibility_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        functions,
        vec!["test_friend_visibility", "query", "secrets::reveal"],
        "secrets::reveal must surface with its module-qualified name \
         because it crosses a friend boundary into a different user-code \
         module than the toplevel `auth` frame",
    );

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(3));
    assert_eq!(counts["io_events"].as_u64(), Some(0));

    // 1 step + 3 call_entry + 3 call_exit = 7 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 7);
    assert_step_indices_monotonic(&doc);

    // Entry order: outermost first, then each callee in call order.
    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "test_friend_visibility".to_string(),
            "query".to_string(),
            "secrets::reveal".to_string(),
        ],
    );

    // ----- The Call/Return pair across the friend boundary --------------
    // Exits in LIFO close order: secrets::reveal is called from inside
    // query, which is called from inside the test entry, so reveal closes
    // first, then query, then the outer test_friend_visibility frame
    // (toplevel Return) closes last.
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 3);

    // secrets::reveal returns the canonical 42.
    assert_eq!(exits[0].0, "secrets::reveal");
    assert_eq!(exits[0].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[0].1["i"].as_i64(), Some(42));

    // query() forwards the value through the friend boundary.
    assert_eq!(exits[1].0, "query");
    assert_eq!(exits[1].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[1].1["i"].as_i64(), Some(42));

    // The outer test frame returns Void (toplevel Return, closes last).
    assert_eq!(exits[2].0, "test_friend_visibility");
    assert_eq!(exits[2].1["kind"].as_str(), Some("Void"));

    // ----- The reveal call_entry has zero positional args ---------------
    // entries[0]=test_friend_visibility, [1]=query, [2]=secrets::reveal.
    let entries: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let reveal_entry = entries[2];
    assert_eq!(
        reveal_entry["function"].as_str(),
        Some("secrets::reveal"),
        "the friend-callee call_entry must use the qualified function name",
    );
    let reveal_args = reveal_entry["args"].as_array().expect("reveal args");
    assert_eq!(reveal_args.len(), 0, "secrets::reveal takes no parameters");

    // The query call_entry also has zero positional args.
    let query_args = entries[1]["args"].as_array().expect("query args");
    assert_eq!(query_args.len(), 0);

    // ----- The 42 surfaces in the logical step's vars -------------------
    // The canonical secret value `42` returned by `secrets::reveal` flows
    // back through `auth::query` and surfaces as `stack_top` (the Move VM
    // stack snapshot) on the line-level logical step that `ct print
    // --full` presents.  The `local_0` return-value binding materialises
    // on a subsequent column-nudge step (a distinct source statement)
    // that the logical-step view — aligned with logicalStepCount — does
    // not carry; the cross-friend-call dataflow is already pinned exactly
    // through the query / secrets::reveal call_exit return values above.
    let ints = unique_int_pairs(&doc);
    assert_eq!(ints, vec![("stack_top".to_string(), 42)]);
}

// ===========================================================================
// test_native_fun — `native fun` declarations: Call/Return brackets zero steps
// ===========================================================================

/// Records `flow_test::test_native_fun` (synthetic NDJSON).
///
/// Pins that a Move stdlib `native fun` call (`vector::length`)
/// surfaces as a Call/Return pair that brackets *zero* `step` events
/// between its `call_entry` and `call_exit`.  This is the recorder's
/// canonical surface for "native fn body has no Move source" — there
/// is nothing to step through, so the recorder emits no `step` event
/// inside the native frame.  The function table also lists the native
/// by name (unqualified, matching the M5–M8 stdlib convention) so
/// downstream consumers can pair the call against the stdlib's known
/// native registry.  The native's return value (`u64` length of the
/// 5-byte input vector) surfaces as a typed `ValueRecord::Int`.
#[test]
fn test_native_fun_test_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_native_fun_test_via_ct_print_full",
        "test_native_fun",
        flow_test_named_source("native_fun_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "native_fun_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(functions, vec!["test_native_fun", "length"]);

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
        vec!["test_native_fun".to_string(), "length".to_string()],
    );

    // ----- The native call_entry/call_exit pair brackets ZERO steps -----
    // This is the recorder's canonical surface for "native fn body has
    // no Move source": the Move VM emits no `Instruction` events between
    // the native's OpenFrame and CloseFrame, so the recorder records no
    // `entry_step`/`exit_step` advancement across the native frame —
    // both step indices on the call_entry are equal because no step
    // fired between OpenFrame and CloseFrame for the native body.
    let length_entry = events
        .iter()
        .find(|e| e["kind"] == "call_entry" && e["function"] == "length")
        .expect("length call_entry");
    assert_eq!(
        length_entry["entry_step"].as_u64(),
        length_entry["exit_step"].as_u64(),
        "native fn `length` must register entry_step == exit_step \
         on its call_entry — no step event fired between its \
         OpenFrame and CloseFrame because its body has no Move source \
         to step through",
    );
    // The native call's depth is 1 (called from depth 0 toplevel).
    assert_eq!(length_entry["depth"].as_u64(), Some(1));

    // ----- The native call's argument is &vector<u8> --------------------
    // entries[0]=test_native_fun (no args), entries[1]=length (entry order).
    let entries: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let length_args = entries[1]["args"].as_array().expect("length args");
    assert_eq!(length_args.len(), 1, "vector::length takes one &vector arg");
    let v_arg = &length_args[0]["value"];
    assert_eq!(v_arg["kind"].as_str(), Some("Reference"));
    assert_eq!(v_arg["mutable"].as_bool(), Some(false));
    let v_pointee = &v_arg["dereferenced"];
    assert_eq!(v_pointee["kind"].as_str(), Some("Sequence"));
    let v_elements = v_pointee["elements"].as_array().expect("Sequence.elements");
    assert_eq!(v_elements.len(), 5, "b\"abcde\" is 5 bytes");
    let bytes: Vec<i64> = v_elements
        .iter()
        .map(|e| {
            assert_eq!(e["kind"].as_str(), Some("Int"));
            e["i"].as_i64().expect("Int.i")
        })
        .collect();
    assert_eq!(bytes, vec![97_i64, 98, 99, 100, 101]);

    // ----- Return values (LIFO close order) ------------------------------
    // The inner native `length` frame closes first; the outer
    // test_native_fun entry is the last frame open (toplevel Return) and
    // closes last.
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 2);

    // vector::length returns the byte count as a typed Int.
    assert_eq!(exits[0].0, "length");
    assert_eq!(exits[0].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[0].1["i"].as_i64(), Some(5));

    assert_eq!(exits[1].0, "test_native_fun");
    assert_eq!(exits[1].1["kind"].as_str(), Some("Void"));

    // ----- The 5-byte input vector surfaces in the logical step ----------
    // The `b"abcde"` byte vector passed to the native `vector::length`
    // surfaces as a typed Sequence<u8> ([97, 98, 99, 100, 101]) on the
    // line-level logical step that `ct print --full` presents.  The
    // native's return value `5` is bound to `local_1` on a subsequent
    // column-nudge step (a distinct source statement) that the
    // logical-step view — aligned with logicalStepCount — does not carry;
    // the length return is already pinned exactly on the call_exit above.
    let seq_lists = collect_sequence_int_lists(&doc);
    assert!(
        seq_lists.contains(&vec![97_i64, 98, 99, 100, 101]),
        "expected b\"abcde\" input as a typed Sequence<u8>; got {seq_lists:?}",
    );
    let ints = unique_int_pairs(&doc);
    assert!(
        ints.is_empty(),
        "the native length return `5` lands on a column-nudge step, so no \
         scalar Int surfaces on the logical step; got {ints:?}",
    );
}

// ===========================================================================
// test_dynamic_field — Sui dynamic-field add/borrow/remove
// ===========================================================================

/// Records `flow_test::test_dynamic_field` (synthetic NDJSON).
///
/// Pins the recorder's surface for the canonical Sui dynamic-field
/// trio: `dynamic_field::add(&mut parent.id, b"key1", 42u64)`,
/// `dynamic_field::borrow<vector<u8>, u64>(&parent.id, b"key1")`, and
/// `dynamic_field::remove(...)`.  Each call surfaces as a balanced
/// Call/Return pair; the byte-vector key surfaces as a typed
/// `ValueRecord::Sequence` whose elements are the raw `u8`-tagged
/// `Int` bytes; and the dynamic-field value surfaces with its
/// declared runtime type — a typed `Int(42)` on `remove`'s return and
/// inside `borrow`'s `Reference` pointee.
#[test]
fn test_dynamic_field_test_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_dynamic_field_test_via_ct_print_full",
        "test_dynamic_field",
        flow_test_named_source("dynamic_field_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "dynamic_field_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        functions,
        vec!["test_dynamic_field", "add", "borrow", "remove"],
    );

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(4));
    assert_eq!(counts["io_events"].as_u64(), Some(0));

    // 1 step + 4 call_entry + 4 call_exit = 9 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 9);
    assert_step_indices_monotonic(&doc);

    // Entry order: outer test entry first, then each helper.
    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "test_dynamic_field".to_string(),
            "add".to_string(),
            "borrow".to_string(),
            "remove".to_string(),
        ],
    );

    // ----- Each dynamic_field::* call's args -----------------------------
    // entries[0]=test_dynamic_field (no args), [1]=add, [2]=borrow,
    // [3]=remove (entry order).
    let entries: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();

    // The canonical b"key1" byte-vector shape.  Pre-encoded as a typed
    // `Sequence` whose four `u8`-tagged `Int` children spell "key1".
    let expected_key: Vec<i64> = vec![107, 101, 121, 49];

    // ----- add(&mut parent.id, key, 42u64) -------------------------------
    let add_args = entries[1]["args"].as_array().expect("add args");
    assert_eq!(add_args.len(), 3, "dynamic_field::add takes 3 args");
    // arg0: &mut parent.id — a Reference whose pointee is the UID struct.
    let add_arg0 = &add_args[0]["value"];
    assert_eq!(add_arg0["kind"].as_str(), Some("Reference"));
    assert_eq!(add_arg0["mutable"].as_bool(), Some(true));
    let uid_pointee = &add_arg0["dereferenced"];
    assert_eq!(uid_pointee["kind"].as_str(), Some("Struct"));
    let uid_fields = uid_pointee["field_values"]
        .as_array()
        .expect("UID.field_values");
    assert_eq!(uid_fields.len(), 1, "UID {{ id: ID }}");
    assert_eq!(uid_fields[0]["kind"].as_str(), Some("Struct"));
    let id_fields = uid_fields[0]["field_values"]
        .as_array()
        .expect("ID.field_values");
    assert_eq!(id_fields.len(), 1, "ID {{ bytes: address }}");
    assert_eq!(id_fields[0]["kind"].as_str(), Some("String"));
    assert_eq!(id_fields[0]["text"].as_str(), Some("0xC0FFEE"));
    let uid_type_id = uid_pointee["type_id"].as_u64().expect("UID type_id");

    // arg1: b"key1" — typed Sequence<u8>.
    let add_arg1 = &add_args[1]["value"];
    assert_eq!(add_arg1["kind"].as_str(), Some("Sequence"));
    assert_eq!(add_arg1["is_slice"].as_bool(), Some(false));
    let key_elements = add_arg1["elements"].as_array().expect("Sequence.elements");
    let key_bytes: Vec<i64> = key_elements
        .iter()
        .map(|e| {
            assert_eq!(e["kind"].as_str(), Some("Int"));
            e["i"].as_i64().expect("Int.i")
        })
        .collect();
    assert_eq!(key_bytes, expected_key);

    // arg2: 42u64 — typed Int.
    let add_arg2 = &add_args[2]["value"];
    assert_eq!(add_arg2["kind"].as_str(), Some("Int"));
    assert_eq!(add_arg2["i"].as_i64(), Some(42));

    // ----- borrow(&parent.id, key) ---------------------------------------
    let borrow_args = entries[2]["args"].as_array().expect("borrow args");
    assert_eq!(borrow_args.len(), 2, "dynamic_field::borrow takes 2 args");
    let borrow_arg0 = &borrow_args[0]["value"];
    assert_eq!(borrow_arg0["kind"].as_str(), Some("Reference"));
    assert_eq!(
        borrow_arg0["mutable"].as_bool(),
        Some(false),
        "borrow takes &UID (immutable)",
    );
    assert_eq!(
        borrow_arg0["dereferenced"]["type_id"].as_u64(),
        Some(uid_type_id),
        "the borrowed UID pointee must share the parent UID's registered type id",
    );
    let borrow_arg1 = &borrow_args[1]["value"];
    assert_eq!(borrow_arg1["kind"].as_str(), Some("Sequence"));
    let borrow_key_bytes: Vec<i64> = borrow_arg1["elements"]
        .as_array()
        .expect("Sequence.elements")
        .iter()
        .map(|e| {
            assert_eq!(e["kind"].as_str(), Some("Int"));
            e["i"].as_i64().expect("Int.i")
        })
        .collect();
    assert_eq!(borrow_key_bytes, expected_key);

    // ----- remove(&mut parent.id, key) -----------------------------------
    let remove_args = entries[3]["args"].as_array().expect("remove args");
    assert_eq!(remove_args.len(), 2, "dynamic_field::remove takes 2 args");
    let remove_arg0 = &remove_args[0]["value"];
    assert_eq!(remove_arg0["kind"].as_str(), Some("Reference"));
    assert_eq!(remove_arg0["mutable"].as_bool(), Some(true));
    assert_eq!(
        remove_arg0["dereferenced"]["type_id"].as_u64(),
        Some(uid_type_id),
        "the &mut UID arg to remove must share the parent's UID type id",
    );

    // ----- Return values (LIFO close order) ------------------------------
    // The three dynamic_field helpers are direct children of the test
    // entry invoked in sequence, so they close in call order at indices
    // 0..3; the outer test_dynamic_field entry is the last frame open
    // (toplevel Return) and closes last (index 3).
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 4);

    // dynamic_field::add returns Void.
    assert_eq!(exits[0].0, "add");
    assert_eq!(exits[0].1["kind"].as_str(), Some("Void"));

    // dynamic_field::borrow returns &u64 (Reference whose pointee is Int 42).
    assert_eq!(exits[1].0, "borrow");
    let borrow_rv = &exits[1].1;
    assert_eq!(borrow_rv["kind"].as_str(), Some("Reference"));
    assert_eq!(borrow_rv["mutable"].as_bool(), Some(false));
    let borrow_pointee = &borrow_rv["dereferenced"];
    assert_eq!(borrow_pointee["kind"].as_str(), Some("Int"));
    assert_eq!(borrow_pointee["i"].as_i64(), Some(42));

    // dynamic_field::remove returns the dynamic-field value as a typed Int.
    assert_eq!(exits[2].0, "remove");
    assert_eq!(exits[2].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[2].1["i"].as_i64(), Some(42));

    // test_dynamic_field -> Void (toplevel Return, closes last).
    assert_eq!(exits[3].0, "test_dynamic_field");
    assert_eq!(exits[3].1["kind"].as_str(), Some("Void"));

    // ----- The dynamic-field value 42 surfaces in the logical step ------
    // The `add(&mut id, key, 42u64)` value argument surfaces as the typed
    // Int `arg2` on the line-level logical step that `ct print --full`
    // presents.  The borrow-deref result (`local_1`) and the `remove`
    // return binding (`local_2`) materialise on subsequent column-nudge
    // steps (distinct source statements) that the logical-step view —
    // aligned with logicalStepCount — does not carry; both are already
    // pinned exactly on the borrow/remove call_exit return values above.
    let ints = unique_int_pairs(&doc);
    assert_eq!(ints, vec![("arg2".to_string(), 42)]);
}

// ===========================================================================
// test_table — Aptos `0x1::table::Table` operations
// ===========================================================================

/// Records `flow_test::test_table` (synthetic NDJSON).
///
/// Pins the recorder's surface for the canonical Aptos table trio:
/// `table::new()`, `table::add(&mut t, k, v)`, `table::borrow(&t, k)`,
/// `table::contains(&t, k)`.  Each call surfaces as a balanced
/// Call/Return pair; the `Table<address,u64>` struct surfaces as a
/// typed `ValueRecord::Struct` with its single `handle: address` field
/// captured as a `ValueRecord::String`; and the contained values
/// surface with their declared runtime type — `Int` for the `u64`
/// payload, `Bool` for the membership predicate.  The recorder also
/// registers `Table<address,u64>` as a parameterised struct key in the
/// type table so the type-args distinguish `Table<address,u64>` from
/// any other `Table<K,V>` instantiation.
#[test]
fn test_table_test_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_table_test_via_ct_print_full",
        "test_table",
        flow_test_named_source("table_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "table_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        functions,
        vec!["test_table", "new", "add", "borrow", "contains"],
    );

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(5));
    assert_eq!(counts["io_events"].as_u64(), Some(0));

    // 1 step + 5 call_entry + 5 call_exit = 11 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 11);
    assert_step_indices_monotonic(&doc);

    // Entry order: outer test entry first, then each helper.
    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "test_table".to_string(),
            "new".to_string(),
            "add".to_string(),
            "borrow".to_string(),
            "contains".to_string(),
        ],
    );

    // ----- Table<address,u64> registers as a parameterised struct key ---
    let types: Vec<&str> = doc["types"]
        .as_array()
        .expect("types array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    types
        .iter()
        .position(|t| *t == "Table<address,u64>")
        .unwrap_or_else(|| panic!("expected Table<address,u64> in the type table; got {types:?}"));

    // ----- Each table::* call's args -------------------------------------
    // Entry order: entries[0]=test_table, [1]=new, [2]=add, [3]=borrow,
    // [4]=contains.
    let entries: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();

    // ----- table::add(&mut entries, addr1, 100u64) -----------------------
    let add_args = entries[2]["args"].as_array().expect("add args");
    assert_eq!(add_args.len(), 3, "table::add takes 3 args");
    let add_arg0 = &add_args[0]["value"];
    assert_eq!(add_arg0["kind"].as_str(), Some("Reference"));
    assert_eq!(add_arg0["mutable"].as_bool(), Some(true));
    let table_pointee = &add_arg0["dereferenced"];
    assert_eq!(table_pointee["kind"].as_str(), Some("Struct"));
    let table_fields = table_pointee["field_values"]
        .as_array()
        .expect("Table.field_values");
    assert_eq!(table_fields.len(), 1, "Table {{ handle: address }}");
    assert_eq!(table_fields[0]["kind"].as_str(), Some("String"));
    assert_eq!(table_fields[0]["text"].as_str(), Some("0xCAFE"));
    let table_type_id = table_pointee["type_id"]
        .as_u64()
        .expect("Table<address,u64> type_id");
    assert_eq!(add_args[1]["value"]["kind"].as_str(), Some("String"));
    assert_eq!(add_args[1]["value"]["text"].as_str(), Some("0xAB"));
    assert_eq!(add_args[2]["value"]["kind"].as_str(), Some("Int"));
    assert_eq!(add_args[2]["value"]["i"].as_i64(), Some(100));

    // ----- table::borrow(&entries, addr1) --------------------------------
    let borrow_args = entries[3]["args"].as_array().expect("borrow args");
    assert_eq!(borrow_args.len(), 2);
    assert_eq!(borrow_args[0]["value"]["kind"].as_str(), Some("Reference"));
    assert_eq!(borrow_args[0]["value"]["mutable"].as_bool(), Some(false));
    assert_eq!(
        borrow_args[0]["value"]["dereferenced"]["type_id"].as_u64(),
        Some(table_type_id),
        "the &Table arg to borrow must share the Table<address,u64> type id",
    );
    assert_eq!(borrow_args[1]["value"]["text"].as_str(), Some("0xAB"));

    // ----- table::contains(&entries, addr1) ------------------------------
    let contains_args = entries[4]["args"].as_array().expect("contains args");
    assert_eq!(contains_args.len(), 2);
    assert_eq!(
        contains_args[0]["value"]["kind"].as_str(),
        Some("Reference")
    );
    assert_eq!(contains_args[0]["value"]["mutable"].as_bool(), Some(false));
    assert_eq!(
        contains_args[0]["value"]["dereferenced"]["type_id"].as_u64(),
        Some(table_type_id),
        "the &Table arg to contains must share the Table<address,u64> type id",
    );

    // ----- Return values (LIFO close order) ------------------------------
    // The four table helpers are direct children of the test entry
    // invoked in sequence, so they close in call order at indices 0..4;
    // the outer test_table entry is the last frame open (toplevel Return)
    // and closes last (index 4).
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 5);

    // table::new returns the freshly-minted Table<address,u64> struct.
    assert_eq!(exits[0].0, "new");
    let new_rv = &exits[0].1;
    assert_eq!(new_rv["kind"].as_str(), Some("Struct"));
    assert_eq!(
        new_rv["type_id"].as_u64(),
        Some(table_type_id),
        "table::new's return value must register as Table<address,u64>",
    );
    let new_fields = new_rv["field_values"]
        .as_array()
        .expect("Table.field_values");
    assert_eq!(new_fields.len(), 1);
    assert_eq!(new_fields[0]["kind"].as_str(), Some("String"));
    assert_eq!(new_fields[0]["text"].as_str(), Some("0xCAFE"));

    // table::add returns Void.
    assert_eq!(exits[1].0, "add");
    assert_eq!(exits[1].1["kind"].as_str(), Some("Void"));

    // table::borrow returns &u64 (Reference whose pointee is Int 100).
    assert_eq!(exits[2].0, "borrow");
    let borrow_rv = &exits[2].1;
    assert_eq!(borrow_rv["kind"].as_str(), Some("Reference"));
    assert_eq!(borrow_rv["mutable"].as_bool(), Some(false));
    assert_eq!(borrow_rv["dereferenced"]["kind"].as_str(), Some("Int"));
    assert_eq!(borrow_rv["dereferenced"]["i"].as_i64(), Some(100));

    // table::contains returns the membership verdict as a typed Bool.
    assert_eq!(exits[3].0, "contains");
    assert_eq!(exits[3].1["kind"].as_str(), Some("Bool"));
    assert_eq!(exits[3].1["b"].as_bool(), Some(true));
    assert_eq!(exits[3].1["text"].as_str(), Some("true"));

    // test_table -> Void (toplevel Return, closes last).
    assert_eq!(exits[4].0, "test_table");
    assert_eq!(exits[4].1["kind"].as_str(), Some("Void"));

    // ----- Step vars on the logical step --------------------------------
    // Every table operation's runtime effect (the `add` value arg 100,
    // the borrow-deref `local_2 = 100`, and the `contains` verdict
    // `local_3 = true`) is materialised on a column-nudge step attached to
    // the corresponding `table::*` call statement; `ct print --full`
    // surfaces the line-level logical step, which carries no variable
    // snapshots here.  Each op's value/verdict is already pinned exactly
    // through the call_exit return values above, so the logical step's
    // scalar var sets are empty.
    let ints = unique_int_pairs(&doc);
    assert!(
        ints.is_empty(),
        "table op values land on column-nudge steps; the logical step has no \
         scalar Int vars, got {ints:?}",
    );
    let bools = unique_bool_pairs(&doc);
    assert!(
        bools.is_empty(),
        "the contains verdict lands on a column-nudge step; the logical step has \
         no Bool vars, got {bools:?}",
    );
}

// ===========================================================================
// test_address_literals — @0x1cafe / @flow_test / @std (Move address literals)
// ===========================================================================

/// Records `flow_test::test_address_literals` (synthetic NDJSON).
///
/// Pins the recorder's surface for Move address literals (`@0x...`,
/// `@named_address`, `@std`).  Each address is materialised in a
/// local, then round-tripped through `id_addr(a: address): address` so
/// the bytecode compiler cannot constant-fold it away.  Each address
/// surfaces as a typed `ValueRecord::String { type_id: address_id }`
/// carrying the exact 64-hex-digit zero-padded 32-byte address text —
/// the same convention every other address-bearing fixture uses
/// (`signer_test`, `tx_context_test`, `object_lifecycle_test`,
/// `table_test`).  The address registry's TypeId is asserted to be
/// the canonical `address` slot in the type table.
#[test]
fn test_address_literals_test_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_address_literals_test_via_ct_print_full",
        "test_address_literals",
        flow_test_named_source("address_literals_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "address_literals_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(functions, vec!["test_address_literals", "id_addr"]);

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(4));
    assert_eq!(counts["io_events"].as_u64(), Some(0));

    // 1 step + 4 call_entry + 4 call_exit = 9 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 9);
    assert_step_indices_monotonic(&doc);

    // Entry order: outer test entry first, then three id_addr calls.
    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "test_address_literals".to_string(),
            "id_addr".to_string(),
            "id_addr".to_string(),
            "id_addr".to_string(),
        ],
    );

    // ----- The canonical 32-byte address payloads ------------------------
    let addr_a = "0x000000000000000000000000000000000000000000000000000000000001cafe";
    let addr_b = "0x00000000000000000000000000000000000000000000000000000000000abcde";
    let addr_c = "0x0000000000000000000000000000000000000000000000000000000000000001";

    // ----- The address `TypeId` is the canonical `address` slot ---------
    let types: Vec<&str> = doc["types"]
        .as_array()
        .expect("types array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    let address_type_id = types
        .iter()
        .position(|t| *t == "address")
        .expect("`address` slot in types table") as u64;

    // ----- id_addr(a) / id_addr(b) / id_addr(c) call args + returns -----
    // entries[0]=test_address_literals, [1..=3]=id_addr (entry order).
    let entries: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 4);

    // Exits in LIFO close order: the three id_addr calls are direct
    // children of the test entry invoked in sequence, so they close in
    // call order at exit indices 0..3; the outer test_address_literals
    // entry is the last frame open (toplevel Return) and closes last
    // (exit index 3).  Entry indices are still 1..=3 (0 = the test entry).
    for (entry_idx, exit_idx, expected) in
        [(1usize, 0usize, addr_a), (2, 1, addr_b), (3, 2, addr_c)]
    {
        let args = entries[entry_idx]["args"].as_array().expect("args array");
        assert_eq!(args.len(), 1, "id_addr takes a single address arg");
        let arg0 = &args[0]["value"];
        assert_eq!(arg0["kind"].as_str(), Some("String"));
        assert_eq!(
            arg0["type_id"].as_u64(),
            Some(address_type_id),
            "address arg must register against the canonical `address` TypeId",
        );
        assert_eq!(arg0["text"].as_str(), Some(expected));
        // Length pin: the 32-byte address text is exactly 66 chars
        // (`0x` + 64 hex digits) so the recorder is shown to capture
        // the FULL 32-byte payload, not a trimmed short form.
        assert_eq!(arg0["text"].as_str().unwrap().len(), 66);

        assert_eq!(exits[exit_idx].0, "id_addr");
        let rv = &exits[exit_idx].1;
        assert_eq!(rv["kind"].as_str(), Some("String"));
        assert_eq!(rv["type_id"].as_u64(), Some(address_type_id));
        assert_eq!(rv["text"].as_str(), Some(expected));
        assert_eq!(rv["text"].as_str().unwrap().len(), 66);
    }

    // test_address_literals -> Void (toplevel Return, closes last).
    assert_eq!(exits[3].0, "test_address_literals");
    assert_eq!(exits[3].1["kind"].as_str(), Some("Void"));

    // ----- The address literals surface in the logical step's vars -------
    // The Effect::Write events for local_0 / local_1 / local_2 (the three
    // address literals materialised up-front) plus the first id_addr's
    // reused `arg0` slot all attach to the line-level logical step that
    // `ct print --full` surfaces.  The per-call round-trip bindings
    // (local_3 / local_4 / local_5) and the arg0 reuses for the second
    // and third id_addr calls materialise on subsequent column-nudge
    // steps that the logical-step view — aligned with logicalStepCount —
    // does not carry; each call's full round-trip is already pinned
    // exactly through the call-arg + call_exit assertions above.
    let pairs = unique_raw_pairs(&doc);
    assert_eq!(
        pairs,
        vec![
            // The three address literals materialise first (Effect::Write
            // for local_0 / local_1 / local_2 before any id_addr call).
            ("local_0".to_string(), addr_a.to_string()),
            ("local_1".to_string(), addr_b.to_string()),
            ("local_2".to_string(), addr_c.to_string()),
            // The first id_addr(arg0) call's arg binding.
            ("arg0".to_string(), addr_a.to_string()),
        ],
    );
}

// ===========================================================================
// test_multi_test_module — three #[test] fns share one module / one source
// ===========================================================================

/// Records all THREE `#[test]` functions of `multi_test_module_test`
/// (each backed by its own NDJSON trace fixture, mirroring the
/// `sui move test --trace` per-test-function output convention).
///
/// Pins the per-test-isolation invariant: each trace lands on a
/// *separate* converter invocation that registers ONLY the functions
/// reached by that test body (no cross-contamination from peer
/// `#[test]`s in the same source file).  Each test body surfaces as
/// the toplevel `Function` entry of its own trace, and each call
/// graph is balanced (`call_entry` count equals `call_exit` count).
#[test]
fn test_multi_test_module_test_via_ct_print_full() {
    // ----- test_arithmetic: pure helper ----------------------------------
    let Some((doc_a, _)) = record_and_dump_full_with_source(
        "test_multi_test_module_test_via_ct_print_full[arithmetic]",
        "test_arithmetic",
        flow_test_named_source("multi_test_module_test"),
    ) else {
        return;
    };
    assert_metadata_program_is(&doc_a, "multi_test_module_test");
    let fns_a: Vec<&str> = doc_a["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        fns_a,
        vec!["test_arithmetic", "add"],
        "arithmetic trace must register ONLY its own test body + helper",
    );
    let counts_a = &doc_a["counts"];
    assert_eq!(counts_a["calls"].as_u64(), Some(2));
    assert_eq!(counts_a["steps"].as_u64(), Some(1));
    assert_eq!(counts_a["io_events"].as_u64(), Some(0));
    let events_a = doc_a["events"].as_array().expect("events array");
    assert_eq!(events_a.len(), 5, "1 step + 2 call_entry + 2 call_exit");
    assert_eq!(
        observed_call_sequence(&doc_a),
        vec!["test_arithmetic".to_string(), "add".to_string()],
    );
    // LIFO close order: inner add closes first, outer test entry last.
    let exits_a = observed_exit_sequence(&doc_a);
    assert_eq!(exits_a.len(), 2);
    assert_eq!(exits_a[0].0, "add");
    assert_eq!(exits_a[0].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits_a[0].1["i"].as_i64(), Some(5));
    assert_eq!(exits_a[1].0, "test_arithmetic");
    assert_eq!(exits_a[1].1["kind"].as_str(), Some("Void"));

    // ----- test_resource_lifecycle: struct construction + destructure ----
    let Some((doc_b, _)) = record_and_dump_full_with_source(
        "test_multi_test_module_test_via_ct_print_full[resource]",
        "test_resource_lifecycle",
        flow_test_named_source("multi_test_module_test"),
    ) else {
        return;
    };
    assert_metadata_program_is(&doc_b, "multi_test_module_test");
    let fns_b: Vec<&str> = doc_b["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        fns_b,
        vec!["test_resource_lifecycle", "make_counter"],
        "resource trace must register ONLY its own test body + helper",
    );
    let counts_b = &doc_b["counts"];
    assert_eq!(counts_b["calls"].as_u64(), Some(2));
    assert_eq!(counts_b["steps"].as_u64(), Some(1));
    assert_eq!(counts_b["io_events"].as_u64(), Some(0));
    let events_b = doc_b["events"].as_array().expect("events array");
    assert_eq!(events_b.len(), 5);
    assert_eq!(
        observed_call_sequence(&doc_b),
        vec![
            "test_resource_lifecycle".to_string(),
            "make_counter".to_string(),
        ],
    );
    // LIFO close order: inner make_counter closes first, outer test last.
    let exits_b = observed_exit_sequence(&doc_b);
    assert_eq!(exits_b.len(), 2);
    assert_eq!(exits_b[0].0, "make_counter");
    let counter_rv = &exits_b[0].1;
    assert_eq!(counter_rv["kind"].as_str(), Some("Struct"));
    let counter_fields = counter_rv["field_values"]
        .as_array()
        .expect("Counter.field_values");
    assert_eq!(counter_fields.len(), 1);
    assert_eq!(counter_fields[0]["kind"].as_str(), Some("Int"));
    assert_eq!(counter_fields[0]["i"].as_i64(), Some(7));
    assert_eq!(exits_b[1].0, "test_resource_lifecycle");
    assert_eq!(exits_b[1].1["kind"].as_str(), Some("Void"));

    // ----- test_event_emit: sui::event::emit Sui native ------------------
    let Some((doc_c, _)) = record_and_dump_full_with_source(
        "test_multi_test_module_test_via_ct_print_full[event]",
        "test_event_emit_multi",
        flow_test_named_source("multi_test_module_test"),
    ) else {
        return;
    };
    assert_metadata_program_is(&doc_c, "multi_test_module_test");
    let fns_c: Vec<&str> = doc_c["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        fns_c,
        vec!["test_event_emit", "emit"],
        "event trace must register ONLY its own test body + emit native",
    );
    let counts_c = &doc_c["counts"];
    assert_eq!(counts_c["calls"].as_u64(), Some(2));
    assert_eq!(counts_c["steps"].as_u64(), Some(1));
    assert_eq!(
        counts_c["io_events"].as_u64(),
        Some(1),
        "sui::event::emit must surface as exactly one MoveEvent io_event",
    );
    let events_c = doc_c["events"].as_array().expect("events array");
    // 1 step + 2 call_entry + 1 io + 2 call_exit = 6 events.
    assert_eq!(events_c.len(), 6);
    assert_eq!(
        observed_call_sequence(&doc_c),
        vec!["test_event_emit".to_string(), "emit".to_string()],
    );
    // LIFO close order: inner emit closes first, outer test entry last.
    let exits_c = observed_exit_sequence(&doc_c);
    assert_eq!(exits_c.len(), 2);
    assert_eq!(exits_c[0].0, "emit");
    assert_eq!(exits_c[0].1["kind"].as_str(), Some("Void"));
    assert_eq!(exits_c[1].0, "test_event_emit");
    assert_eq!(exits_c[1].1["kind"].as_str(), Some("Void"));

    // ----- Cross-trace isolation: each fn-table is disjoint --------------
    // The strict `assert_eq!` pins above already prove each trace's
    // function table contains exactly the test body + its helper, so
    // by construction no foreign test body can leak in.  This block is
    // intentionally a no-op verifier — it documents the isolation
    // contract without re-asserting via membership checks.
}

// ===========================================================================
// test_generic_constraints — multi-ability generic instantiations
// ===========================================================================

/// Records `flow_test::test_generic_constraints` (synthetic NDJSON).
///
/// Pins the recorder's surface for a multi-ability generic
/// (`store_value<T: copy + drop + store>`) instantiated with two
/// distinct primitives (`u64`, `bool`), plus a weaker-constraint
/// peer (`discard<T: drop>`).  Each `Container<T>` instantiation must
/// register a *distinct* `TypeId` keyed on `Container<u64>` /
/// `Container<bool>` in the type table (per
/// `TypeIds::ensure_parameterised_struct`); the two call_exit
/// records for the same generic `store_value` must surface with
/// these distinct return-type ids so downstream consumers can
/// distinguish the two instantiations even though the function
/// table lists `store_value` exactly once (the recorder's
/// generic-aware function-name policy intentionally keeps the bare
/// name and pushes the type identity into the value records).
#[test]
fn test_generic_constraints_test_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_generic_constraints_test_via_ct_print_full",
        "test_generic_constraints",
        flow_test_named_source("generic_constraints_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "generic_constraints_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        functions,
        vec!["test_generic_constraints", "store_value", "discard"],
    );

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(4));
    assert_eq!(counts["io_events"].as_u64(), Some(0));

    // 1 step + 4 call_entry + 4 call_exit = 9 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 9);
    assert_step_indices_monotonic(&doc);

    // Entry order: outer test entry first.
    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "test_generic_constraints".to_string(),
            "store_value".to_string(),
            "store_value".to_string(),
            "discard".to_string(),
        ],
    );

    // ----- Both Container<T> instantiations register as distinct types --
    let types: Vec<&str> = doc["types"]
        .as_array()
        .expect("types array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    let cu_type_id = types
        .iter()
        .position(|t| *t == "Container<u64>")
        .expect("Container<u64> slot in types table") as u64;
    let cb_type_id = types
        .iter()
        .position(|t| *t == "Container<bool>")
        .expect("Container<bool> slot in types table") as u64;
    assert_ne!(
        cu_type_id, cb_type_id,
        "Container<u64> and Container<bool> must register as distinct \
         TypeKind::Struct ids — multi-ability generic instantiations of \
         the same struct must not collapse into a single type id",
    );

    // ----- Each store_value<T> call: arg + return-type identity ---------
    // Entry order: entries[0]=test_generic_constraints (no args),
    // [1]=store_value<u64>, [2]=store_value<bool>, [3]=discard<u64>.
    let entries: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    // Exits in LIFO close order: the three helpers are direct children of
    // the test entry invoked in sequence, so they close in call order at
    // exit indices 0..3; the outer test_generic_constraints entry is the
    // last frame open (toplevel Return) and closes last (exit index 3).
    // Entry indices are still 1..=3 (0 = the test entry).
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 4);

    // store_value<u64>(42) — arg is Int 42.
    let sv_u64_args = entries[1]["args"]
        .as_array()
        .expect("store_value<u64> args");
    assert_eq!(sv_u64_args.len(), 1);
    assert_eq!(sv_u64_args[0]["value"]["kind"].as_str(), Some("Int"));
    assert_eq!(sv_u64_args[0]["value"]["i"].as_i64(), Some(42));
    assert_eq!(exits[0].0, "store_value");
    let cu_rv = &exits[0].1;
    assert_eq!(cu_rv["kind"].as_str(), Some("Struct"));
    assert_eq!(
        cu_rv["type_id"].as_u64(),
        Some(cu_type_id),
        "store_value<u64>'s return must carry the Container<u64> type id",
    );
    let cu_fields = cu_rv["field_values"]
        .as_array()
        .expect("Container<u64>.field_values");
    assert_eq!(cu_fields.len(), 1);
    assert_eq!(cu_fields[0]["kind"].as_str(), Some("Int"));
    assert_eq!(cu_fields[0]["i"].as_i64(), Some(42));

    // store_value<bool>(true) — arg is Bool true.
    let sv_bool_args = entries[2]["args"]
        .as_array()
        .expect("store_value<bool> args");
    assert_eq!(sv_bool_args.len(), 1);
    assert_eq!(sv_bool_args[0]["value"]["kind"].as_str(), Some("Bool"));
    assert_eq!(sv_bool_args[0]["value"]["b"].as_bool(), Some(true));
    assert_eq!(exits[1].0, "store_value");
    let cb_rv = &exits[1].1;
    assert_eq!(cb_rv["kind"].as_str(), Some("Struct"));
    assert_eq!(
        cb_rv["type_id"].as_u64(),
        Some(cb_type_id),
        "store_value<bool>'s return must carry the Container<bool> type id",
    );
    let cb_fields = cb_rv["field_values"]
        .as_array()
        .expect("Container<bool>.field_values");
    assert_eq!(cb_fields.len(), 1);
    assert_eq!(cb_fields[0]["kind"].as_str(), Some("Bool"));
    assert_eq!(cb_fields[0]["b"].as_bool(), Some(true));
    assert_eq!(cb_fields[0]["text"].as_str(), Some("true"));

    // discard<u64>(7) — arg is Int 7, returns Void.
    let dis_args = entries[3]["args"].as_array().expect("discard args");
    assert_eq!(dis_args.len(), 1);
    assert_eq!(dis_args[0]["value"]["kind"].as_str(), Some("Int"));
    assert_eq!(dis_args[0]["value"]["i"].as_i64(), Some(7));
    assert_eq!(exits[2].0, "discard");
    assert_eq!(exits[2].1["kind"].as_str(), Some("Void"));

    // test_generic_constraints -> Void (toplevel Return, closes last).
    assert_eq!(exits[3].0, "test_generic_constraints");
    assert_eq!(exits[3].1["kind"].as_str(), Some("Void"));

    // ----- Step vars: the first store_value arg surfaces -----------------
    // The `store_value<u64>(42)` call argument surfaces as the typed Int
    // `arg0` on the line-level logical step that `ct print --full`
    // presents.  The Container<u64>/Container<bool> Struct bindings, the
    // second store_value / discard args, and the destructured inner
    // values all materialise on subsequent column-nudge steps (distinct
    // source statements) that the logical-step view — aligned with
    // logicalStepCount — does not carry.  The typed Container<T> return
    // shapes and their phantom-distinct `type_id`s are already pinned
    // exactly on the store_value call_exit return values above.
    let _ = (cu_type_id, cb_type_id);
    let ints = unique_int_pairs(&doc);
    assert_eq!(ints, vec![("arg0".to_string(), 42)]);
}

// ===========================================================================
// test_public_package — Move 2024 `public(package) fun` visibility
// ===========================================================================

/// Records `flow_test::test_public_package` (synthetic NDJSON).
///
/// Pins the recorder's surface for a Move 2024 `public(package) fun`
/// call across a package-internal module boundary
/// (`pkg_app::call_helper` -> `pkg_lib::helper`).  Move 2024's
/// `public(package)` visibility class — the modern, package-scoped
/// replacement for the legacy `public(friend)` mechanism — must
/// survive the recorder's call surface as:
///
///   1. A normal balanced Call/Return pair across the boundary.
///   2. A `MoveCallVisibility` `TraceLogEvent` carrying the literal
///      visibility-class string (`"public(package)"` for the callee,
///      `"public"` for the intermediate caller).  The recorder
///      surfaces the tag immediately *before* the corresponding
///      `call_entry` so downstream consumers can pair the visibility
///      with the call by emission order.
///   3. The cross-module callee (`pkg_lib::helper`) takes the
///      qualified `module::name` form in the function table because
///      the callee module differs from the toplevel test frame's
///      module, mirroring the friend-boundary convention pinned by
///      `test_friend_visibility_test_via_ct_print_full`.
#[test]
fn test_public_package_test_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_public_package_test_via_ct_print_full",
        "test_public_package",
        flow_test_named_source("public_package_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "public_package_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        functions,
        vec!["test_public_package", "call_helper", "pkg_lib::helper"],
        "the package-boundary callee must surface as `pkg_lib::helper` \
         (qualified) because it crosses into a different user-code \
         module than the toplevel `pkg_app` frame",
    );

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(3));
    assert_eq!(
        counts["io_events"].as_u64(),
        Some(2),
        "two MoveCallVisibility io_events must surface — one for the \
         intermediate `call_helper` (`public`) and one for the \
         `public(package)` `pkg_lib::helper` callee",
    );

    // 1 step + 3 call_entry + 2 io + 3 call_exit = 9 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 9);
    assert_step_indices_monotonic(&doc);

    // Entry order: outer test entry first.
    assert_eq!(
        observed_call_sequence(&doc),
        vec![
            "test_public_package".to_string(),
            "call_helper".to_string(),
            "pkg_lib::helper".to_string(),
        ],
    );

    // ----- The two MoveCallVisibility io_events -------------------------
    // The recorder emits the visibility tag immediately *before* its
    // corresponding `call_entry`, so the io stream order mirrors the
    // OpenFrame order in the trace: first `call_helper` (public),
    // then `pkg_lib::helper` (public(package)).
    let io_events: Vec<&serde_json::Value> = events.iter().filter(|e| e["kind"] == "io").collect();
    assert_eq!(io_events.len(), 2);
    assert_eq!(io_events[0]["text"].as_str(), Some("public"));
    assert_eq!(io_events[1]["text"].as_str(), Some("public(package)"));

    // ----- Call_entry / call_exit pair across the package boundary -------
    // entries[0]=test_public_package, [1]=call_helper, [2]=pkg_lib::helper.
    let entries: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    assert_eq!(entries.len(), 3);
    let helper_entry = entries[2];
    assert_eq!(
        helper_entry["function"].as_str(),
        Some("pkg_lib::helper"),
        "the package-callee call_entry must use the qualified function name",
    );
    let helper_args = helper_entry["args"].as_array().expect("helper args");
    assert_eq!(helper_args.len(), 0, "pkg_lib::helper takes no parameters");

    // The intermediate `call_helper` and outer `test_public_package`
    // also have zero positional args.
    assert_eq!(entries[0]["args"].as_array().map(|a| a.len()), Some(0));
    assert_eq!(entries[1]["args"].as_array().map(|a| a.len()), Some(0));

    // ----- Return values across the package boundary (LIFO close order) --
    // pkg_lib::helper is called from inside call_helper, which is called
    // from inside the test entry, so helper closes first, then
    // call_helper, then the outer test_public_package frame (toplevel
    // Return) closes last.
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 3);

    // pkg_lib::helper returns the canonical 7.
    assert_eq!(exits[0].0, "pkg_lib::helper");
    assert_eq!(exits[0].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[0].1["i"].as_i64(), Some(7));

    // call_helper forwards the value across the package boundary.
    assert_eq!(exits[1].0, "call_helper");
    assert_eq!(exits[1].1["kind"].as_str(), Some("Int"));
    assert_eq!(exits[1].1["i"].as_i64(), Some(7));

    // The outer test frame returns Void (toplevel Return, closes last).
    assert_eq!(exits[2].0, "test_public_package");
    assert_eq!(exits[2].1["kind"].as_str(), Some("Void"));

    // ----- The `7` surfaces in the logical step's vars ------------------
    // The `7` forwarded across the package boundary surfaces as the
    // `stack_top` Move VM stack snapshot on the line-level logical step
    // that `ct print --full` presents.  The `local_0` return-value
    // binding materialises on a subsequent column-nudge step (a distinct
    // source statement) that the logical-step view — aligned with
    // logicalStepCount — does not carry; the cross-package dataflow is
    // already pinned exactly on the call_helper / pkg_lib::helper
    // call_exit return values above.
    let ints = unique_int_pairs(&doc);
    assert_eq!(ints, vec![("stack_top".to_string(), 7)]);
}

// ===========================================================================
// test_module_init — Sui `fun init(ctx: &mut TxContext)` one-time entry
// ===========================================================================

/// Records `flow_test::test_module_init` (synthetic NDJSON).
///
/// Pins the recorder's surface for Sui's one-time module-init entry
/// point — `fun init(ctx: &mut TxContext)`, the callback Sui runs
/// exactly once when a package is published.  `init` has no `public`
/// modifier and no `#[test]` attribute: it is a first-class entry
/// point recognised by the Sui runtime via its name + signature.
/// The recorder must:
///
///   1. Surface the `init` invocation as a normal Call/Return pair
///      that brackets the publish-time body (the `object::new` UID
///      mint plus the `Bootstrap` resource construction).
///   2. Surface the `&mut TxContext` parameter as a typed
///      `ValueRecord::Reference { mutable: true, .. }` whose
///      pointee is the underlying `TxContext` struct — the same
///      shape pinned by `test_tx_context_test_via_ct_print_full`.
///   3. Flag the invocation as one-time / module-init via a
///      `MoveCallVisibility` `TraceLogEvent` with content `"init"`
///      so downstream consumers can highlight the publish-time
///      bootstrap frame in the call graph.
#[test]
fn test_module_init_test_via_ct_print_full() {
    let Some((doc, _)) = record_and_dump_full_with_source(
        "test_module_init_test_via_ct_print_full",
        "test_module_init",
        flow_test_named_source("module_init_test"),
    ) else {
        return;
    };

    assert_metadata_program_is(&doc, "module_init_test");

    let functions: Vec<&str> = doc["functions"]
        .as_array()
        .expect("functions array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(functions, vec!["init", "new"]);

    let counts = &doc["counts"];
    assert_eq!(counts["steps"].as_u64(), Some(1));
    assert_eq!(counts["calls"].as_u64(), Some(2));
    assert_eq!(
        counts["io_events"].as_u64(),
        Some(1),
        "exactly one MoveCallVisibility io_event must surface tagging \
         the `init` frame as the Sui one-time module-init entry",
    );

    // 1 step + 1 io + 2 call_entry + 2 call_exit = 6 events.
    let events = doc["events"].as_array().expect("events array");
    assert_eq!(events.len(), 6);
    assert_step_indices_monotonic(&doc);

    // Entry order: outer init first, then new (called by init).
    assert_eq!(
        observed_call_sequence(&doc),
        vec!["init".to_string(), "new".to_string()],
    );

    // ----- The MoveCallVisibility io_event flags the init entry ---------
    let io_events: Vec<&serde_json::Value> = events.iter().filter(|e| e["kind"] == "io").collect();
    assert_eq!(io_events.len(), 1);
    assert_eq!(
        io_events[0]["text"].as_str(),
        Some("init"),
        "the `init` frame must surface a MoveCallVisibility tag with \
         content `\"init\"` — the stable hook downstream consumers key \
         off to highlight Sui's publish-time bootstrap frame",
    );

    // ----- init(ctx: &mut TxContext) — the &mut TxContext arg shape -----
    // entries[0]=init, entries[1]=new (entry order).
    let entries: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["kind"] == "call_entry")
        .collect();
    assert_eq!(entries.len(), 2);
    let init_entry = entries[0];
    assert_eq!(init_entry["function"].as_str(), Some("init"));
    let init_args = init_entry["args"].as_array().expect("init args");
    assert_eq!(init_args.len(), 1, "init takes a single &mut TxContext arg");
    let ctx_arg = &init_args[0]["value"];
    assert_eq!(ctx_arg["kind"].as_str(), Some("Reference"));
    assert_eq!(
        ctx_arg["mutable"].as_bool(),
        Some(true),
        "&mut TxContext must surface with mutable=true",
    );
    let ctx_pointee = &ctx_arg["dereferenced"];
    assert_eq!(ctx_pointee["kind"].as_str(), Some("Struct"));
    let ctx_fields = ctx_pointee["field_values"]
        .as_array()
        .expect("TxContext.field_values");
    assert_eq!(
        ctx_fields.len(),
        5,
        "TxContext fields: sender, tx_hash, epoch, epoch_timestamp_ms, ids_created",
    );
    // sender field is the typed address String.
    assert_eq!(ctx_fields[0]["kind"].as_str(), Some("String"));
    assert_eq!(ctx_fields[0]["text"].as_str(), Some("0xCAFE"));

    // The nested object::new(ctx) takes the same &mut TxContext.
    let new_args = entries[1]["args"].as_array().expect("new args");
    assert_eq!(new_args.len(), 1);
    assert_eq!(new_args[0]["value"]["kind"].as_str(), Some("Reference"));
    assert_eq!(new_args[0]["value"]["mutable"].as_bool(), Some(true));

    // ----- Return values (LIFO close order) ------------------------------
    // init is the Sui publish-time entry function here (no synthetic outer
    // test frame); it calls object::new, which closes before init's own
    // frame.  So the inner new closes first and init closes last.
    let exits = observed_exit_sequence(&doc);
    assert_eq!(exits.len(), 2);

    // object::new returns a fresh UID as a typed Struct whose inner
    // ID { bytes: address } preserves the byte-vector identity.
    assert_eq!(exits[0].0, "new");
    let uid_rv = &exits[0].1;
    assert_eq!(uid_rv["kind"].as_str(), Some("Struct"));
    let uid_fields = uid_rv["field_values"].as_array().expect("UID.field_values");
    assert_eq!(uid_fields.len(), 1, "UID {{ id: ID }}");
    assert_eq!(uid_fields[0]["kind"].as_str(), Some("Struct"));
    let id_fields = uid_fields[0]["field_values"]
        .as_array()
        .expect("ID.field_values");
    assert_eq!(id_fields.len(), 1, "ID {{ bytes: address }}");
    assert_eq!(id_fields[0]["kind"].as_str(), Some("String"));
    assert_eq!(id_fields[0]["text"].as_str(), Some("0xFEED"));

    // init returns Void — Sui's publish-time entry closes last.
    assert_eq!(exits[1].0, "init");
    assert_eq!(exits[1].1["kind"].as_str(), Some("Void"));
}
