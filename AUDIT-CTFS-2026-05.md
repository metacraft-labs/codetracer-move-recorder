# Move Recorder CTFS Audit — 2026-05-02

This audit checks `codetracer-move-recorder` against the canonical
CodeTracer multi-stream CTFS schema and the section 5.6 audit checklist
maintained in `/tmp/isonim-migration.txt`.  Prior audits set the
canonical patterns: Ruby (1.21, 1.22), Python (1.27), JavaScript (1.38),
EVM (1.39), PHP (1.41) and Solana (1.44).

The Move recorder is a *post-execution* trace converter rather than a
live tracer.  It consumes:

* Sui's `sui replay --trace` NDJSON (Move trace format v3) — the rich
  path with per-instruction `OpenFrame` / `CloseFrame` / `Effect`
  records, full `parameters` and stack values; and
* Aptos's `MOVE_VM_TRACE` CSV plus `--profile-gas` JSON — a much
  thinner skeleton (function name + PC only, no values).

Both paths use the **Rust-native NimTraceWriter** (`codetracer_trace_writer_nim`
crate, not the C FFI), so every canonical entry point is reachable.
The recorder does **not** suffer from the C-FFI gaps documented in
section 5.6 of the migration handoff (PHP audit findings).

## Summary

| # | Check | Status (pre-fix) | Status (post-fix) | Notes |
|---|---|---|---|---|
| a | `register_call` for each call | OK | OK | `converter.rs` (Sui) emits `register_call` on every `OpenFrame`; `aptos_adapter.rs` (Aptos) emits one per function transition.  No `add_event(Call(..))` call anywhere. |
| b | Call args via `register_call_arg` / `arg()` | **GAP** | **PARTIAL** | Pre-fix: every `register_call(fn_id, vec![])` site passed an empty arg list and ignored Move's `OpenFrame.frame.parameters`.  Post-fix: the Sui path stages each `OpenFrame.frame.parameters[i]` via `TraceWriter::arg("arg{i}", value)` before `register_call`.  The Aptos path still passes `vec![]` because `MOVE_VM_TRACE` provides no parameter values — see "Open: parameter recovery for Aptos" below. |
| c | Write/WriteOther/Error/TraceLogEvent for IO and structured events via `register_special_event` | **GAP** | **OK** | Pre-fix: `Effect::ExecutionError` was printed via `eprintln!` (lost from the trace) and `TraceEvent::External` was silently dropped.  Post-fix: execution errors emit `register_special_event(Error, "MoveExecutionError", message)`; external effects emit `register_special_event(TraceLogEvent, "MoveExternalEffect", kind)`.  Move has no native stdout/stderr (programs are pure smart contracts). |
| d | Thread events (ThreadStart / Exit / Switch) | OK (N/A) | OK (N/A) | The Move VM is single-threaded by construction — every smart-contract execution runs in a single VM thread.  The recorder correctly emits no thread events. |
| e | Step records for line navigation | OK | OK | `converter.rs` emits `register_step(path, line)` on every distinct source line transition (Sui path).  `aptos_adapter.rs` emits one Step per trace entry (Aptos path; granularity matches the input data). |
| f | Canonical CTFS schema match | **GAP** | **OK** | Pre-fix: CLI `--format` accepted only `binary` / `json` strings (defaulting to `binary`, the legacy CBOR+Zstd format), with no way to request the canonical CTFS multi-stream container.  Post-fix: CLI exposes a typed `OutputFormat` enum (`Ctfs` / `Binary` / `Json`), defaults to `ctfs`, and every recorder dispatch converts via the `OutputFormat → TraceEventsFileFormat` impl.  This is the same legacy-format issue caught in the EVM (1.39) and Solana (1.44) audits. |
| g | Obsolete `#[no_mangle]` stubs | OK | OK | `grep -r '#\[no_mangle\]' src/` returns no results.  This recorder predates the JS-recorder pattern that introduced the FFI stubs that conflicted with upstream Nim exports. |
| C-FFI vs native | OK | OK | `Cargo.toml` depends on `codetracer_trace_writer_nim` (sibling-path dep), not on the C FFI.  Every canonical API (`register_call`, `arg`, `register_special_event`, `register_thread_*`) is reachable.  No FFI-extension blockers. |

## Concrete fixes applied

### 1. CLI now exposes and defaults to `Ctfs`

`src/main.rs`'s `--format` CLI arg used to be a `String` matched with
`match format.as_str() { "json" => ..., _ => Binary }` — there was no
way to request the canonical CTFS multi-stream container, and the
fallback was the legacy CBOR+Zstd `Binary` format.

Post-fix: a typed `OutputFormat` enum (`Ctfs` / `Binary` / `Json`)
implementing `clap::ValueEnum`, with `Ctfs` listed first and used as
the default for all three subcommands (`record`, `replay`,
`aptos-replay`).  An `impl From<OutputFormat> for TraceEventsFileFormat`
keeps the call sites uniform.  This mirrors the canonical-format fix
applied in the EVM (1.39) and Solana (1.44) recorders.

### 2. OpenFrame parameters staged as call args

`src/converter.rs::convert_trace_into_writer` previously walked every
`OpenFrame` and emitted:

```rust
TraceWriter::register_call(writer, fn_id, vec![]);
```

dropping `frame.parameters` on the floor.  Move's v3 trace format
already delivers each formal parameter's value with the `OpenFrame`
record (as a `TraceValue` carrying a `RuntimeValue` / `ImmRef` /
`MutRef` snapshot) — so the parameter values are right there, ready to
attach to the call record.

Post-fix:

```rust
for (idx, param) in frame.parameters.iter().enumerate() {
    let value = convert_move_value(param.inner_value(), &type_ids);
    let _ = TraceWriter::arg(writer, &format!("arg{idx}"), value);
}
TraceWriter::register_call(writer, fn_id, vec![]);
```

`TraceWriter::arg(name, value)` on the live `NimTraceWriter` both
registers the value as a step variable (so it surfaces in
`ct/load-locals` for the caller) and stages it on the writer's
pending-args buffer that the next `register_call` consumes — see
`codetracer_trace_writer_nim/src/lib.rs:927` for the canonical
implementation.  This mirrors the Ruby (1.22), JS (1.38) and Solana
(1.44) call-arg staging fix pattern.

The synthesised `arg{idx}` names are positional because Sui's frame
schema only carries the *values* of parameters, not their declared
identifiers.  Higher-fidelity names would require parsing the Move
function's source-map (`.mvsm` file) and matching `binary_member_index`
to the function's `parameters_idx` list — tracked as a follow-up below.

### 3. ExecutionError and External effects routed through register_special_event

`Effect::ExecutionError` and `TraceEvent::External` were silently
dropped from the trace pre-fix:

```rust
Effect::ExecutionError(error) => {
    eprintln!("Move execution error: {error}");
}
// ...
TraceEvent::External(_) => {
    // External effects are informational for now.
}
```

This meant arithmetic overflows, divide-by-zero, abort codes, and Sui
side effects (object transfers, native event emission, etc.) never
appeared in the CodeTracer event-log pane.

Post-fix, both route through `register_special_event` with the
canonical EventLogKind mapping established by prior audits:

* `Effect::ExecutionError(msg)` →
  `register_special_event(EventLogKind::Error, "MoveExecutionError", msg)`
  — so abort messages appear in the event-log pane's error bucket
  (`toIOEventKind` maps `Error` → `error`).
* `TraceEvent::External { kind }` →
  `register_special_event(EventLogKind::TraceLogEvent, "MoveExternalEffect", kind)`
  — same convention as Solana (1.44) for non-stdout structured trace
  events.  These appear in the event-log pane without polluting the
  program-log (stdout) bucket.

The `metadata` field on each `RecordEvent` carries a stable tag
(`"MoveExecutionError"` / `"MoveExternalEffect"`) so frontend
consumers can route Move-specific events distinctly.

## Tests added

`tests/test_ctfs_audit.rs` (new) locks in the post-fix behaviour with
four regression tests:

* `test_open_frame_parameters_staged_as_call_args` — exercises the
  parameter-staging code path and verifies it doesn't corrupt event
  ordering.  See the test's doc-comment for why we cannot directly
  assert on `CallRecord.args` from an in-memory `NonStreamingTraceWriter`
  (its `arg()` is a no-op test double); the live `NimTraceWriter`
  attaches the staged args via the FFI `register_call_arg` path.
* `test_execution_error_emits_special_event` — asserts that
  `Effect::ExecutionError` produces a `RecordEvent { kind: Error,
  metadata: "MoveExecutionError", content: <message> }` event.
* `test_external_effect_emits_special_event` — asserts that
  `TraceEvent::External` produces a `RecordEvent { kind:
  TraceLogEvent, metadata: "MoveExternalEffect", content: <kind> }`
  event.
* `test_ctfs_format_advertised_in_help` — smoke test that runs the
  CLI binary with `record --help` and asserts the output advertises
  `ctfs` as a `--format` value with `[default: ctfs]`.

## Tests run

`cargo test --release` after fixes:

* `lib` unit tests: 0/0 (no inline tests; lib is small)
* `test_aptos`: 4/4 passing
* `test_cli`: 4/4 passing
* `test_comprehensive`: 52/52 passing
* `test_converter`: 5/5 passing
* `test_ctfs_audit` (new): 4/4 passing
* `test_replay`: 8/8 passing
* `test_sui_integration`: 2/2 passing
* `test_aptos_adapter` (lib integration): 20/20 passing

Total: **99/99 passing**, 0 regressions.

## Open gaps / follow-ups

### Parameter recovery for Aptos `MOVE_VM_TRACE`

Aptos's `MOVE_VM_TRACE` CSV contains only `(function_name, PC)` pairs —
no parameter values.  `aptos_adapter::convert_aptos_trace` therefore
still calls `register_call(fn_id, vec![])` with no `arg()` staging.

Fixing this requires either:
1. Parsing the Move bytecode's function signature (via the published
   module's `.mv` binary) to recover *declared* parameter types and
   names (no values), allowing zero-valued placeholder args; or
2. A pluggable hook into the Aptos VM's instruction dispatch (similar
   to the trait wired in the Solana recorder at `tracer_trait.rs`)
   that emits parameter values at call boundaries.

Approach (2) requires Aptos VM patching that is out of scope for the
recorder.  Approach (1) would give partial fidelity (signature only,
no values) and is non-trivial because it requires linking against the
Move bytecode parser.

Mirrors the EVM recorder's open `Call.args` gap (1.39) and Solana's
open internal-call symbolic-arg gap (1.44).

### Higher-fidelity Sui parameter names

The Sui path now stages parameters as `arg0`, `arg1`, etc.  Sui's
`.mvsm` source-map files carry the declared parameter identifiers
(under each function's `parameters` table mapping to `LocalAccessIndex`
entries with `name_index` strings).  Plumbing those names through
`SourceMapResolver` to replace the synthetic `arg{idx}` names is a
small follow-up.  Concrete shape:

1. Extend `SourceMapResolver::lookup` (or add a sister method) to
   return parameter names for a given `(module, function_name)` pair.
2. In `converter.rs::OpenFrame` handling, look up the per-function
   parameter names and use them in `TraceWriter::arg(name, value)`
   instead of `format!("arg{idx}")`.

Skipped here because the current `SourceMapResolver` is the empty
placeholder (`SourceMapResolver::empty()`); proper `.mvsm` parsing is
a separate milestone tracked in `move_types.rs`'s comments.

### Sui `External` effect granularity

`TraceEvent::External` is the catch-all for Sui-specific side effects
that the move-trace-format types don't fully model.  Pre-fix the
recorder dropped them; post-fix it forwards `ExternalEffect.kind` as a
`TraceLogEvent`.  But Sui actually emits richer external data
(transferred-object IDs, event payloads, gas adjustments) that the
current `move_types::ExternalEffect` struct flattens to just `kind:
String` (see `move_types.rs:246`).

To preserve more, the `ExternalEffect` deserialiser would need to
capture the full `serde_json::Value` payload, and the recorder would
JSON-serialise it into the special-event `content` field.  Tracked as
a follow-up; not blocking.

### Multi-stream IO event collapse (cross-cutting infrastructure issue)

As documented in section 5.6 of `/tmp/isonim-migration.txt` ("New
issues uncovered"), the Nim multi-stream IO event stream's
`toIOEventKind` collapse drops most of the 13 `EventLogKind` variants
into 4 buckets (`stdout`, `stderr`, `fileOp`, `error`).  The Move
recorder's `TraceLogEvent` (used for external effects above) lands in
the `stderr` bucket, alongside `EvmEvent` from the EVM recorder and
`TraceLogEvent` from the Solana recorder.

This is acceptable for the audit (Error → `error` bucket lands
cleanly; external effects in `stderr` is a reasonable default), but a
follow-up to preserve the original `EventLogKind` byte through the
multi-stream format would let the frontend distinguish Move external
effects from terminal stderr and from EVM LOG opcodes.

This is an infrastructure change in
`codetracer-trace-format-nim/src/codetracer_trace_writer_ffi.nim` —
out of scope for any single recorder audit.

### `start()` toplevel function still has `vec![]` args

`TraceWriter::start(writer, source_path, Line(1))` opens an implicit
toplevel call (the synthetic `<toplevel>` frame).  No args are
attached to it in either recorder path.  This is correct: the
toplevel has no callsite-visible args.  Listed here for completeness.

### `is_native` Sui frames

The Sui frame struct carries `is_native: bool` to flag Move native
function calls.  The recorder currently treats native and non-native
frames identically.  Surfacing this distinction (e.g. via a
`metadata` arg `"native: true"` or a tag in the function name) would
help users debug Move programs that exercise native functions.
Tracked as a small follow-up; not a CTFS-format issue.

## Convention compliance follow-up — 2026-05-08

The 2026-05-02 audit landed a `--format ctfs|binary|json` `clap::ValueEnum`
defaulting to `Ctfs`, mirroring the EVM (1.39) / Solana (1.44) /
Cardano (1.48) / Cairo (1.50) / Flow (1.52) / Fuel (1.53) / PolkaVM
(1.55) / Miden (1.56) / TON (1.57) / Circom (1.58) / Leo audits.
Subsequent to that audit,
`Recorder-CLI-Conventions.md` §4 in `codetracer-specs` was tightened
to require **CTFS-only** output: recorders no longer accept a
`--format` flag and `ct print` (shipped with
`codetracer-trace-format-nim`) is the canonical conversion tool for
human-readable output.  `Repo-Requirements.md` §2.2 / §2.3 reflect
this contract.

This entry records the convention compliance follow-up applied to the
Move recorder on 2026-05-08, mirroring the cairo (2710b5e), cardano
(0698f00), circom (2d8b280), flow (49a4fa9), fuel (0ec716d), leo
(d567b52) and miden (c7bafaa) precedents:

* The `--format` / `-f` CLI flag was removed from all three subcommands
  (`record`, `replay`, `aptos-replay`).  The `OutputFormat` enum and
  its `impl From<OutputFormat> for TraceEventsFileFormat` block were
  deleted from `src/main.rs`.  Clap rejects `--format <anything>`
  with an "unexpected argument" diagnostic (verified by
  `test_format_flag_rejected_by_clap`).
* The `format` parameter was removed from `converter::convert_trace`,
  `replay::replay_from_existing_trace`,
  `aptos_replay::aptos_replay_from_existing_data`,
  `aptos_adapter::convert_aptos_trace`, `ReplayConfig` and
  `AptosReplayConfig`.  Each `create_trace_writer` call site is
  hard-pinned to a module-level
  `const TRACE_FORMAT: TraceEventsFileFormat = TraceEventsFileFormat::Ctfs;`
  in both `src/converter.rs` and `src/aptos_adapter.rs`.  The
  `events_filename` match (which used to dispatch on
  `Json` / `Binary` / `BinaryV0` / `Ctfs`) was collapsed to the
  single CTFS arm (`trace.bin`) at every site.
* `CODETRACER_MOVE_RECORDER_OUT_DIR` was added as a fallback for
  `--out-dir` on all three subcommands.  Lookup order is CLI flag →
  env var → `./ct-traces/`.
* `CODETRACER_MOVE_RECORDER_DISABLED=1` (or `true`) skips the trace
  emission entirely on every subcommand; the Move recorder doesn't
  run a separate target subprocess so "disabled" simply means
  "don't write any artefacts".
* `CODETRACER_MOVE_RECORDER_LOG_LEVEL` is documented (advisory) in
  the `--help` output and the README.
* The CTFS-only contract is now in force across the codebase: the
  binary's `--help` output mentions `ct print` as the conversion tool;
  the README documents only CTFS, the env-var contract, and the
  `ct print` workflow.
* `tests/test_cli.rs` was rewritten:
  - The pre-existing smoke tests (`help_succeeds_and_mentions_name`,
    `version_succeeds_and_contains_version`,
    `record_nonexistent_file_fails`, `record_creates_output_files`)
    were preserved.  `record_creates_output_files` no longer relies
    on the old `--format ctfs` default; it asserts on the CTFS magic
    bytes of the produced `.ct` file directly.
  - `test_recorded_trace_via_ct_print_json` (new) drives the
    recorder against the real `flow_test__test_computation`
    Sui-format NDJSON fixture in
    `test-programs/move/flow_test/traces/`, pipes the produced `.ct`
    bundle through `ct-print --json`, and asserts on **structural
    anchors** — the fixture source path filename (`flow_test.move`)
    and at least one of the Move function names
    (`test_computation` / `toplevel`) — rather than on integer
    values, because the Move recorder's typed-value payload doesn't
    round-trip cleanly through `ct-print` today (same pre-existing
    limitation as cardano / circom / flow / fuel / leo / miden).
    Skips gracefully when `ct-print` is not present (i.e. when this
    crate is built outside the metacraft workspace).
  - `test_env_out_dir_used_when_flag_omitted` (new) records a
    minimal NDJSON trace without `--out-dir`, sets
    `CODETRACER_MOVE_RECORDER_OUT_DIR` to a tempdir path, and
    asserts the `.ct` bundle landed in the env-supplied directory.
  - `test_env_disabled_skips_recording` (new) records with
    `CODETRACER_MOVE_RECORDER_DISABLED=1` and asserts no artefacts
    were produced (recorder still exits 0).
  - `test_format_flag_rejected_by_clap` (new) confirms clap
    rejection of `--format json`.
  - `test_no_format_flag_in_help` (new) walks top-level / record /
    replay / aptos-replay `--help` and asserts neither `--format`
    nor `CODETRACER_FORMAT` is advertised.
  - `test_help_mentions_ct_print` (new) asserts the conversion-tool
    pointer is present in `--help`.
* `tests/test_ctfs_audit.rs` was updated:
  - The pre-existing `test_ctfs_format_advertised_in_help` test was
    deleted (it asserted on the `[default: ctfs]` clap doc string;
    with the flag gone, it would lock in the regression).  Its
    content coverage is preserved by `test_no_format_flag_in_help` /
    `test_help_mentions_ct_print` / `test_format_flag_rejected_by_clap`
    in `tests/test_cli.rs` plus the new
    `test_recorded_trace_via_ct_print_json` ct-print round-trip.  It
    was not `#[ignore]`'d at the time of deletion.
  - `test_open_frame_parameters_staged_as_call_args`,
    `test_execution_error_emits_special_event`,
    `test_external_effect_emits_special_event` survive unchanged
    in spirit (they exercise `convert_trace_into_writer` against an
    in-memory `NonStreamingTraceWriter` and never depended on the
    `--format` flag).
* `tests/test_aptos.rs`, `tests/test_replay.rs`,
  `tests/test_converter.rs`, `tests/test_comprehensive.rs` and
  `tests/test_sui_integration.rs` had their `convert_trace` /
  `convert_aptos_trace` / `replay_from_existing_trace` /
  `aptos_replay_from_existing_data` / `AptosReplayConfig` /
  `ReplayConfig` call sites updated to omit the
  `TraceEventsFileFormat` argument.  `test_sui_integration` no
  longer asserts on `trace.json` (the recorder writes only
  `trace.bin` now); it asserts on the `.ct` CTFS magic bytes and
  the size lower bound, mirroring the
  `tests/test_replay.rs::test_replay_end_to_end_with_existing_trace`
  pattern.
* `tests/verify-cli-convention-no-silent-skip.sh` was added as a
  shell-level guard that runs the binary's `--help`, asserts
  `--format` and `CODETRACER_FORMAT` are absent, asserts the
  standard flags (`--out-dir`, `--version`) are present, asserts
  `ct print` is mentioned, and asserts the
  `CODETRACER_MOVE_RECORDER_OUT_DIR` /
  `CODETRACER_MOVE_RECORDER_DISABLED` env vars are referenced in
  source.  A `Justfile` was added at repo root to wire it into
  `just lint` / `just test`.
* `README.md` was added documenting the CTFS-only contract, the
  env-var contract, the `ct print` workflow, and the recorder's
  Sui / Aptos data sources.

References:

* [`codetracer-specs/Recorder-CLI-Conventions.md`](../codetracer-specs/Recorder-CLI-Conventions.md) §4 (CTFS-only) and §5 (env vars).
* [`codetracer-specs/Repo-Requirements.md`](../codetracer-specs/Repo-Requirements.md) §2.2 (CLI compliance) and §2.3 (trace format compatibility).
* Cairo precedent: `codetracer-cairo-recorder` commit `2710b5e`.
* Cardano follow-up: `codetracer-cardano-recorder` commit `0698f00`.
* Circom follow-up: `codetracer-circom-recorder` commit `2d8b280`.
* Flow follow-up: `codetracer-flow-recorder` commit `49a4fa9`.
* Fuel follow-up: `codetracer-fuel-recorder` commit `0ec716d`.
* Leo follow-up: `codetracer-leo-recorder` commit `d567b52`.
* Miden follow-up: `codetracer-miden-recorder` commit `c7bafaa`.
