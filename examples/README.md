# Move recorder examples

Small Move (Aptos / Sui) programs that you can record with
`codetracer-move-recorder` and step through in the CodeTracer GUI.

The fixtures here are deliberately tiny — they exist to demonstrate the
end-to-end `ct` workflow, not to exercise the full language surface (the
broader `flow_test` package under `test-programs/move/` covers that).

## Prerequisites

* The `ct` driver on your `PATH` (from `codetracer-nim` or a CodeTracer
  release bundle). All commands below shell out to `ct` only — you
  should never need to invoke `codetracer-move-recorder` directly.
* A Move toolchain CLI on `PATH`: either `sui` or `aptos`. The recorder
  auto-detects whichever is available and drives `move test --trace`.
* The Move recorder built and discoverable by `ct`. From the repo root:

  ```bash
  cargo build --locked --release
  ```

  Then either install the resulting binary, point `ct` at it through its
  recorder-catalog config, or run `ct` from a CodeTracer environment
  that already bundles a recent `codetracer-move-recorder`.
* (Optional) Pre-build the example package if you want to iterate on it
  before recording — `ct record` will do this for you, but you can run
  `sui move build` (or `aptos move compile`) inside the example
  directory first to surface compilation errors quickly.

## Two-step flow: record, then replay

`ct record` produces a `.ct` trace bundle on disk; `ct replay -t` opens
that bundle in the CodeTracer GUI.

```bash
# 1. Record. Produces ./column_aware.ct (a CTFS multi-stream bundle).
ct record examples/column_aware/sources/column_aware.move

# 2. Replay. Opens the GUI at the recorded entry point.
ct replay -t column_aware.ct
```

`ct record` accepts the `.move` source file directly. The recorder walks
up to the enclosing `Move.toml`, invokes the Move toolchain with tracing
enabled, picks the trace that corresponds to your source file, and
converts it into a CTFS bundle.

## One-step flow: record + open in one go

```bash
ct run examples/column_aware/sources/column_aware.move
```

`ct run` is the convenience wrapper around `record` + `replay` — useful
when you just want to inspect a single run and don't need to keep the
`.ct` bundle around for later.

## Walkthrough: `column_aware`

`examples/column_aware/` is the simplest fixture: a single module with
one `#[test]` function whose body packs three statements onto the same
source line so each starts at a distinct column:

```move
let mut v = std::vector::empty<u64>();
blackbox(&mut v, 100); blackbox(&mut v, 200); blackbox(&mut v, 300);
let len = std::vector::length(&v);
```

Record and replay:

```bash
ct run examples/column_aware/sources/column_aware.move
```

In the CodeTracer GUI, set the cursor on the line with the three
`blackbox(...)` calls and use **Step Over** three times. You should see
the highlight advance from column to column on the same line — once per
statement — rather than jumping to the next line after the first step.

This works because the Move compiler emits a `code_map` in its
`debug_info` that records each statement's full `(start_line,
start_column, end_line, end_column)` range. The recorder consumes that
map and surfaces a distinct CTFS step for every statement; the GUI then
uses the column metadata to decide what "next statement" means. Without
column-aware navigation the three calls would collapse onto
`(line, column=1)` and only the first would surface as a stop.

The fixture asserts `vector::length(&v) == 3` at the end, so if the
recorder somehow loses one of the three statements the test in
`tests/test_column_aware.rs` fails alongside the visible regression in
the GUI.

## Other fixtures

* `test-programs/move/flow_test/` — broad Move language coverage
  (abilities, generics, resources, events, vectors, …). Recorded as
  part of the regression suite; useful as a reference but too large to
  serve as an introductory example.
* `test-programs/move/sui_flow_test/` — Sui-specific variant of the
  above (object lifecycle, `TxContext`). Same caveat.

Both can be recorded with the same `ct record <path-to-.move>`
invocation if you want to explore them.
