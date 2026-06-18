# codetracer-move-recorder

A recorder of Move smart-contract executions (Sui and Aptos) that produces
[CodeTracer](https://codetracer.com)-compatible trace bundles.

## CodeTracer

[CodeTracer](https://github.com/metacraft-labs/CodeTracer) is a time-traveling
debugger for compiled and interpreted languages.

### Overview

`codetracer-move-recorder` consumes Move execution data from two sources and
emits a CodeTracer CTFS multi-stream trace bundle:

* **Sui** — `sui replay --trace` / `sui move test --trace-execution` produce a
  rich [Move trace format v3](https://github.com/MystenLabs/sui/blob/main/external-crates/move/crates/move-trace-format)
  NDJSON file (per-instruction `OpenFrame` / `CloseFrame` / `Effect` records,
  full parameter values, stack snapshots).
* **Aptos** — `aptos move replay` with the `MOVE_VM_TRACE` env var and
  `--profile-gas` flag produces a thinner skeleton (function name + PC pairs,
  plus a gas flamegraph) which the recorder merges into a CodeTracer trace.

### Building

```bash
cargo build --locked
```

### Usage

Convert a Sui Move trace file (NDJSON, optionally zstd-compressed) into a
CodeTracer CTFS bundle:

```bash
codetracer-move-recorder record <trace-file> --out-dir <dir>
```

Replay an on-chain Sui transaction (requires the `sui` CLI in `$PATH`):

```bash
codetracer-move-recorder replay --digest <DIGEST> --out-dir <dir>
```

Replay an on-chain Aptos transaction (requires the `aptos` CLI in `$PATH`):

```bash
codetracer-move-recorder aptos-replay --txn-version <VERSION> --out-dir <dir>
```

The recorder always writes traces in the canonical CodeTracer CTFS multi-stream
format. There is no `--format` flag — see "Converting traces" below for
human-readable output.

#### Converting traces to JSON / text

The recorder is CTFS-only. To convert a recorded `.ct` bundle to a
human-readable form, use `ct print` from
[`codetracer-trace-format-nim`](../codetracer-trace-format-nim):

```bash
ct-print --json <recording-dir>/<program>.ct
```

`ct-print` accepts `--json`, `--json-events`, `--summary`, and `--follow`
modes; see its `--help` for details. This conversion path is the canonical
way to produce textual oracles for golden-snapshot tests, debugging, and
interop with non-CodeTracer tools — see `Recorder-CLI-Conventions.md` §4 in
the `codetracer-specs` repo.

### Architecture

The recorder is organized into the following modules:

* `converter.rs` — Sui Move trace v3 NDJSON → CodeTracer event conversion
* `aptos_adapter.rs` — Aptos `MOVE_VM_TRACE` CSV + `--profile-gas` JSON → CodeTracer
* `replay.rs` — Sui `sui replay --trace` orchestration
* `aptos_replay.rs` — Aptos `aptos move replay` orchestration
* `move_types.rs` — Sui Move trace v3 schema types
* `source_map.rs` / `source_lookup.rs` — Move source-map + filesystem source discovery

### Examples

See [`examples/`](./examples/README.md) for small Move programs you can
record with `ct record` and replay in the CodeTracer GUI, including a
walkthrough that demonstrates column-aware step-over on multi-statement
lines.

### Testing

Run the test suite with:

```bash
cargo test
just test     # also runs verify-cli-convention-no-silent-skip.sh
```

`tests/test_sui_integration.rs` exercises a real `sui move test --trace-execution`
run when the `sui` CLI is available; it skips gracefully otherwise.
`tests/test_aptos.rs` covers the Aptos data adapters, REST API parsing and
trace merging without requiring the `aptos` CLI.

### Environment variables

The recorder respects the standard CodeTracer recorder env-var contract
defined in `Recorder-CLI-Conventions.md` §5:

| Variable                              | CLI equivalent | Description                                                                                  |
|---------------------------------------|----------------|----------------------------------------------------------------------------------------------|
| `CODETRACER_MOVE_RECORDER_OUT_DIR`    | `--out-dir`    | Fallback output directory when `--out-dir` is omitted. The CLI flag always wins.             |
| `CODETRACER_MOVE_RECORDER_DISABLED`   | —              | Set to `1` or `true` to run the recorder in pass-through mode (no trace artefacts written).  |
| `CODETRACER_MOVE_RECORDER_LOG_LEVEL`  | —              | Recorder log verbosity (advisory; the Move recorder currently logs to stderr unconditionally).|

### Contributing

Pull requests welcome. See `AUDIT-CTFS-2026-05.md` for the recorder's CTFS
audit history.

### License

Apache 2.0.
