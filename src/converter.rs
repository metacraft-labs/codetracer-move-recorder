//! Convert Move trace events into CodeTracer trace format.
//!
//! Reads NDJSON trace data (version 3), walks the trace events, and
//! emits CodeTracer steps, calls, returns, and variable records.

use std::collections::HashMap;
use std::path::Path;

use codetracer_trace_types::{EventLogKind, Line, NONE_VALUE, TypeId, TypeKind, ValueRecord};
use codetracer_trace_writer_nim::trace_writer::TraceWriter;
use codetracer_trace_writer_nim::{TraceEventsFileFormat, create_trace_writer};
use eyre::{Result, eyre};

use crate::move_debug_info::DebugInfo;
use crate::move_types::{
    Effect, Location, SerializableMoveValue, TraceEvent, TraceValue, VersionHeader,
};
use crate::source_map::SourceMapResolver;

/// The on-disk container produced by the recorder is always the canonical
/// multi-stream CTFS bundle.  Pre-2026-05-08 the recorder accepted a
/// `TraceEventsFileFormat` parameter and the CLI exposed a `--format` flag;
/// the convention now mandates CTFS-only output (see
/// `Recorder-CLI-Conventions.md` §4 in `codetracer-specs`).
const TRACE_FORMAT: TraceEventsFileFormat = TraceEventsFileFormat::Ctfs;

/// Compile-time configuration for the trace converter.
///
/// Most converter behaviour is fixed by the Move v3 trace format spec.
/// The handful of knobs that vary between call sites — surfaced here
/// rather than as a flag on every entrypoint — control output
/// enrichments that should fire for the end-user `ct record` flow but
/// not for low-level snapshot tests that pin the exact event stream
/// shape (see `tests/test_full_coverage.rs`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ConverterOptions {
    /// Emit a `MoveTestEntry` `TraceLogEvent` for the toplevel
    /// function so the GUI event-log pane has at least one row even
    /// when the traced test performs no `sui::event::emit` /
    /// abort / visibility-tagged calls.  Required for the
    /// `ct record path/to/foo.move` flow (so the
    /// `move_example.spec.ts` event-log Playwright test surfaces a
    /// non-zero footer count); disabled for snapshot tests that
    /// enumerate every event in the resulting `.ct` container and
    /// expect `io_events == 0` for non-event-emitting fixtures.
    pub emit_test_entry_event: bool,

    /// Consult the package's `build/<pkg>/debug_info/<Module>.json`
    /// sidecar to recover per-bytecode-PC source line numbers when
    /// the explicit `SourceMapResolver` doesn't carry an entry for
    /// the current `(module, pc)` pair.
    ///
    /// Enabled by the `ct record path/to/foo.move` flow so the GUI
    /// surfaces meaningful per-source-line steps without the user
    /// having to hand-build a source map.  Disabled by default so
    /// the recorder's snapshot tests in `tests/test_full_coverage.rs`
    /// — which run against the same fixtures with
    /// `SourceMapResolver::empty()` and expect exactly one step (the
    /// synthetic entry) — keep passing without per-test plumbing.
    pub resolve_pc_lines_from_debug_info: bool,
}

// The library-default is *off* so existing callers (the recorder's own
// unit / golden-snapshot tests) keep their exact event-stream pin
// without per-test plumbing.  The CLI's `record` subcommand opts in via
// `with_test_entry_event` when it drives the full `move test --trace`
// pipeline.  See `ConverterOptions` doc-comment for the per-field
// rationale.

impl ConverterOptions {
    /// Enable the `MoveTestEntry` baseline event for the toplevel
    /// function.  Used by the `ct record path/to/foo.move` flow so
    /// the GUI event-log pane has at least one row to display even
    /// for the simplest unit tests.
    pub fn with_test_entry_event(mut self) -> Self {
        self.emit_test_entry_event = true;
        self
    }

    /// Enable per-PC source-line resolution from the package's
    /// `build/<pkg>/debug_info/<Module>.json` sidecar.  The
    /// `ct record path/to/foo.move` flow turns this on so the GUI
    /// step navigator can move between actual source lines without
    /// requiring the caller to hand-build a `SourceMapResolver`.
    pub fn with_debug_info_source_lines(mut self) -> Self {
        self.resolve_pc_lines_from_debug_info = true;
        self
    }

    /// Convenience: turn on every enrichment the `ct record` flow
    /// relies on.  New options that map to "the recorder-driven
    /// pipeline" should be added here so the CLI keeps opting in to
    /// the full set without a per-flag enumeration.
    pub fn for_ct_record_flow(self) -> Self {
        self.with_test_entry_event().with_debug_info_source_lines()
    }
}

/// Convert Move NDJSON trace data into a CodeTracer CTFS trace bundle.
///
/// `trace_data` must be decompressed NDJSON (one JSON object per line).
/// The first line must be a `{"version":3}` header.
///
/// The output format is fixed to CTFS — see
/// `Recorder-CLI-Conventions.md` §4 in `codetracer-specs`.  Use
/// `ct print` (from `codetracer-trace-format-nim`) for human-readable
/// conversion of the produced bundle.
pub fn convert_trace(
    trace_data: &[u8],
    source_map: &SourceMapResolver,
    source_path: &Path,
    out_dir: &Path,
) -> Result<()> {
    convert_trace_with_options(
        trace_data,
        source_map,
        source_path,
        out_dir,
        ConverterOptions::default(),
    )
}

/// Like `convert_trace` but lets the caller customise the conversion
/// via `ConverterOptions`.  Used by the CLI's `record` subcommand to
/// opt in to the `MoveTestEntry` baseline event the GUI event-log
/// pane depends on (see `ConverterOptions::with_test_entry_event`).
pub fn convert_trace_with_options(
    trace_data: &[u8],
    source_map: &SourceMapResolver,
    source_path: &Path,
    out_dir: &Path,
    options: ConverterOptions,
) -> Result<()> {
    // -- 1. Create trace writer ------------------------------------------------
    let program_name = source_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "move_program".to_string());

    let mut writer = create_trace_writer(&program_name, &[], TRACE_FORMAT);

    // -- 2. Set up output files ------------------------------------------------
    std::fs::create_dir_all(out_dir).map_err(|e| eyre!("cannot create output dir: {e}"))?;

    // CTFS multi-stream container — `db-backend` infers the format
    // from the `.bin` extension.  No JSON / legacy-binary alternative
    // is exposed.
    let events_path = out_dir.join("trace.bin");

    TraceWriter::begin_writing_trace_events(&mut *writer, &events_path)
        .map_err(|e| eyre!("{e}"))?;

    // -- 3. Convert events into writer ----------------------------------------
    convert_trace_into_writer_with_options(
        trace_data,
        source_map,
        source_path,
        &mut *writer,
        options,
    )?;

    // -- 4. Finish writing ----------------------------------------------------
    TraceWriter::finish_writing_trace_events(&mut *writer).map_err(|e| eyre!("{e}"))?;
    writer
        .write_meta_dat("codetracer-move-recorder")
        .map_err(|e| eyre!("{e}"))?;
    writer.close().map_err(|e| eyre!("{e}"))?;

    Ok(())
}

/// Convert Move NDJSON trace data into events on the given writer.
///
/// This is the core conversion logic, separated from file I/O so that tests
/// can use a `NonStreamingTraceWriter` for in-memory inspection.
pub fn convert_trace_into_writer(
    trace_data: &[u8],
    source_map: &SourceMapResolver,
    source_path: &Path,
    writer: &mut dyn TraceWriter,
) -> Result<()> {
    convert_trace_into_writer_with_options(
        trace_data,
        source_map,
        source_path,
        writer,
        ConverterOptions::default(),
    )
}

/// Like `convert_trace_into_writer` but accepts a `ConverterOptions`
/// to toggle output enrichments.  See `ConverterOptions` for the
/// available knobs and why they exist.
pub fn convert_trace_into_writer_with_options(
    trace_data: &[u8],
    source_map: &SourceMapResolver,
    source_path: &Path,
    writer: &mut dyn TraceWriter,
    options: ConverterOptions,
) -> Result<()> {
    // -- 1. Parse NDJSON -------------------------------------------------------
    let text =
        std::str::from_utf8(trace_data).map_err(|e| eyre!("trace data is not valid UTF-8: {e}"))?;

    let mut lines = text.lines();

    // First line: version header
    let version_line = lines.next().ok_or_else(|| eyre!("empty trace data"))?;
    let header: VersionHeader = serde_json::from_str(version_line)
        .map_err(|e| eyre!("failed to parse version header: {e}"))?;
    if header.version != 3 {
        return Err(eyre!(
            "unsupported trace format version: {} (expected 3)",
            header.version
        ));
    }

    // Remaining lines: trace events
    let mut events = Vec::new();
    for (i, line) in lines.enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: TraceEvent = serde_json::from_str(line)
            .map_err(|e| eyre!("failed to parse trace event at line {}: {e}", i + 2))?;
        events.push(event);
    }

    // Per-package debug info loaded from the Move compiler's
    // `<package_root>/build/<PackageName>/debug_info/<Module>.json`
    // sidecar.  When the source path lives inside a real package
    // (`sui move build`-produced layout), this carries the per-function
    // local / parameter names that we substitute for the synthetic
    // `local_<N>` / `argN` strings.  Synthetic NDJSON fixtures (where
    // no `build/` exists) get an empty store and the converter
    // gracefully falls back to the synthetic names.
    let debug_info = DebugInfo::discover(source_path);

    // -- 2. Start the trace ----------------------------------------------------
    //
    // Peek the first OpenFrame/Instruction pair in the event stream to
    // recover the user-facing source line of the first executed
    // bytecode op.  Without this, `TraceWriter::start` pins the entry
    // step at line 1 (the synthetic `module ... {` declaration), which
    // means the entry-step state panel — what the user sees when they
    // first load a trace — is always empty: the very next instruction
    // creates a new step at the real first source line, and every
    // Effect::Write/Read event after it attaches to that new step
    // rather than the synthetic one.  Seeding `start()` with the real
    // first line and then suppressing the duplicate `register_step`
    // for the first matching instruction keeps the entry step aligned
    // with the variables that get written there.
    //
    // The peek is best-effort: if no Instruction has a source mapping
    // (e.g. a hand-rolled fixture with no `build/` debug info and no
    // explicit `SourceMapResolver`) we fall back to line 1 verbatim so
    // the legacy single-step behaviour stays intact.  The debug-info
    // half of the lookup is also gated on
    // `options.resolve_pc_lines_from_debug_info` so the
    // snapshot-based unit tests (which use `SourceMapResolver::empty`
    // against fixtures that have a `build/` tree) keep observing
    // the original `line == 1` entry seed.
    let first_step_line = first_step_line(
        &events,
        source_map,
        &debug_info,
        options.resolve_pc_lines_from_debug_info,
    )
    .unwrap_or(1);

    // Opt the writer into column-aware step encoding *before* the first
    // `start` / `register_step_with_column` call.  Sticky for the
    // lifetime of the trace; gates the writer's `DeltaColumn` (tag
    // 0x07) emission path plus the `meta.dat` bit 4 flag
    // (`FLAG_HAS_COLUMN_AWARE_STEPS`).  Legacy / test-double backends
    // keep the trait default no-op; the canonical Nim writer
    // (production CLI flow) flips the real flag.
    TraceWriter::enable_column_aware_steps(writer);

    // Register every source path the trace touches together with its
    // per-line UTF-8 byte-length table (paths.dat Layout A) so the
    // column-aware reader can map the writer-side global byte position
    // back to (line, column) at replay time.  Soft-fails (logged) if
    // the writer rejects the call — the trace remains usable, but
    // columns for that file fall back to `None` on read.
    //
    // Two sources of paths:
    //   1. The primary source path passed in (always registered, even
    //      when no instruction lands on it — keeps metadata coherent).
    //   2. Per-module debug-info sidecars: each module's compiler-
    //      recorded `from_file_path` (or its canonical
    //      `sources/<Module>.move` fallback) plus pre-computed
    //      line-length table.
    let mut registered_paths: HashMap<std::path::PathBuf, ()> = HashMap::new();
    {
        let primary_lengths = std::fs::read_to_string(source_path)
            .ok()
            .map(|src| crate::move_debug_info::compute_line_lengths(&src))
            .unwrap_or_default();
        if let Err(err) =
            TraceWriter::register_path_with_line_lengths(writer, source_path, &primary_lengths)
        {
            eprintln!(
                "[codetracer-move-recorder] register_path_with_line_lengths failed for {}: {} \
                 (column resolution will fall back to None for this file)",
                source_path.display(),
                err,
            );
        }
        registered_paths.insert(source_path.to_path_buf(), ());
        for (path, lengths) in debug_info.iter_source_line_lengths() {
            if registered_paths.contains_key(path) {
                continue;
            }
            if let Err(err) =
                TraceWriter::register_path_with_line_lengths(writer, path, lengths)
            {
                eprintln!(
                    "[codetracer-move-recorder] register_path_with_line_lengths failed for \
                     {}: {} (column resolution will fall back to None for this file)",
                    path.display(),
                    err,
                );
            }
            registered_paths.insert(path.to_path_buf(), ());
        }
    }

    TraceWriter::start(writer, source_path, Line(first_step_line as i64));

    // Register common Move types.
    let mut type_ids = TypeIds::register(writer);

    // -- 3. Walk trace events --------------------------------------------------
    // Per-source-line stepping requires us to track:
    //   * `prev_line`: the source line of the last emitted step, so a run of
    //     same-line bytecode instructions collapses to a single step (basic
    //     dedup invariant — see
    //     `tests/test_comprehensive.rs::test_source_map_dedup_same_line_no_duplicate_steps`).
    //   * `prev_pc`:   the bytecode `pc` of the previously processed
    //     `Instruction` event.  When the next `pc` is *less than* `prev_pc`
    //     (a backward branch in the bytecode stream) we are re-entering the
    //     loop body for another iteration; force-emit a step at that point
    //     even when the resolved source line is unchanged so a `for i in
    //     0..N { body }` loop produces N step events at the body line, not
    //     just one — see
    //     `tests/test_full_coverage.rs::test_loops_one_step_per_source_line`
    //     and the spec at `metacraft-specs/policies/recorder-test-requirements.md`
    //     ("a `for i in 0..10` loop must produce exactly 10 step events at
    //     the loop body").
    //
    // Both pieces of state belong to the *current execution context*; they
    // are reset to `None` whenever a frame is opened or closed so that
    // crossing a call boundary does not spuriously fire (or suppress) a
    // backward-jump step in the caller.
    //
    // `prev_line` is *seeded* to the line we just gave `TraceWriter::start`
    // so the converter does not re-register a fresh step at that same
    // line on the first Instruction it processes — the synthetic
    // start-step would otherwise be followed by a duplicate at the
    // identical source line, splitting the variables across two
    // half-empty steps.
    let mut prev_line: Option<u32> = Some(first_step_line);
    let mut prev_pc: Option<u64> = None;
    let mut current_module: Option<String> = None;

    // Stack of (module_name, binary_member_index) for currently open
    // frames so that `Effect::Write` / `Effect::Read` events can name
    // locals using the debug info of the *enclosing* frame.  Push on
    // `OpenFrame`, pop on `CloseFrame`.
    let mut frame_stack: Vec<(String, u64)> = Vec::new();
    // The module of the outer (toplevel) frame.  When a callee runs in
    // a *different* user-code module (`address == 0x0`) than this
    // outer module, the recorder qualifies the function-table entry
    // as `module::name` so cross-module calls across a `friend`
    // boundary surface with their owning module preserved.  Stdlib
    // calls (`0x1::*`, `0x2::*`, ...) keep their bare name to stay
    // compatible with the pre-existing function-table conventions
    // exercised by the M5–M8 fixtures.  See
    // `tests/test_full_coverage.rs::test_friend_visibility_test_via_ct_print_full`.
    let mut outer_module: Option<String> = None;
    // Track the most recently observed Move VM `Instruction` mnemonic and the
    // value of the most recent `Effect::Pop`.  These two pieces of state are
    // needed solely to recover the abort code on `Effect::ExecutionError`:
    // the Move v3 trace format models an `abort` as the sequence
    //   Instruction{ABORT}  ->  Effect::Pop(<code>)  ->  Effect::ExecutionError("ABORTED")
    // i.e. the abort code arrives one event *before* the error marker, then
    // the marker itself carries no code.  Without stitching the two together
    // here, the io_event we emit for the abort would discard the code and
    // every abort site would surface as the indistinguishable string
    // "ABORTED" — see `tests/test_full_coverage.rs::test_abort_io_event_carries_abort_code`.
    let mut prev_instruction: Option<String> = None;
    let mut last_popped_value: Option<SerializableMoveValue> = None;

    // Track whether we have already crossed at least one OpenFrame.
    // The *very first* OpenFrame in the trace is the toplevel entry
    // frame; resetting `prev_line` for it would discard the
    // `first_step_line` seed and force a duplicate step at the entry
    // line.  Subsequent OpenFrame events are nested calls and must
    // still reset (their PC stream starts fresh at 0 with no relation
    // to the caller's prior line).
    let mut first_open_frame_seen = false;

    for event in &events {
        match event {
            TraceEvent::OpenFrame { frame, .. } => {
                current_module = Some(frame.module.name.clone());
                frame_stack.push((frame.module.name.clone(), frame.binary_member_index));
                // Reset per-frame step bookkeeping for *nested* frames:
                // a backward-pc relative to the caller's last
                // instruction is meaningless for the callee, and the
                // callee's first instruction must always get a step.
                // The *toplevel* frame keeps the seeded `prev_line` so
                // the synthetic entry step (registered by
                // `TraceWriter::start` above) is not duplicated by a
                // same-line `register_step` immediately after.
                if first_open_frame_seen {
                    prev_line = None;
                }
                first_open_frame_seen = true;
                prev_pc = None;

                // Detect `sui::event::emit<T>(payload)` — the canonical
                // Sui native used to publish a typed event from a Move
                // module.  When the OpenFrame names the `event::emit`
                // native at address `0x2`, surface a structured
                // `EventLogKind::TraceLogEvent` ahead of the call_entry
                // so downstream consumers can recover the typed payload
                // (struct name, field map) without re-parsing the
                // printed form.  See
                // `tests/test_full_coverage.rs::test_event_emit_test_via_ct_print_full`
                // for the strict shape pin.
                if is_sui_event_emit(&frame.module.name, &frame.module.address)
                    && frame.function_name == "emit"
                    && let Some(param) = frame.parameters.first()
                {
                    let payload_text = render_move_event_payload(param.inner_value());
                    TraceWriter::register_special_event(
                        writer,
                        EventLogKind::TraceLogEvent,
                        "MoveEvent",
                        &payload_text,
                    );
                }

                // Surface a per-frame visibility tag (Move 2024
                // `public(package)`, the legacy `public(friend)`, the
                // Sui one-time `init` entry, etc.) as a structured
                // `MoveCallVisibility` `TraceLogEvent` immediately
                // preceding the `call_entry`.  Sui's real v3 trace
                // format does not emit visibility today, so this fires
                // only for synthetic NDJSON that carries a non-empty
                // `frame.visibility`.  Pinned by:
                //   * `tests/test_full_coverage.rs::test_public_package_test_via_ct_print_full`
                //   * `tests/test_full_coverage.rs::test_module_init_test_via_ct_print_full`
                if let Some(visibility) = frame.visibility.as_deref()
                    && !visibility.is_empty()
                {
                    TraceWriter::register_special_event(
                        writer,
                        EventLogKind::TraceLogEvent,
                        "MoveCallVisibility",
                        visibility,
                    );
                }

                let is_toplevel = outer_module.is_none();
                if is_toplevel {
                    outer_module = Some(frame.module.name.clone());

                    // Surface the toplevel test/entry-function invocation
                    // as a `MoveTestEntry` `TraceLogEvent` so the
                    // event-log pane has at least one row even when the
                    // traced function performs no `sui::event::emit` /
                    // abort / visibility-tagged calls.  Without this
                    // hint, simple unit-test traces (the only kind the
                    // GUI tests record on machines without on-chain
                    // fixtures) surface an empty event log, which is
                    // indistinguishable from a broken event-log pipeline
                    // and silently fails the `loadedEventLog` /
                    // `event log has at least one event` contract from
                    // `move_example.spec.ts`.
                    //
                    // Opt-in via `ConverterOptions::emit_test_entry_event`
                    // so the snapshot-based unit tests in
                    // `tests/test_full_coverage.rs` that pin
                    // `io_events == 0` for non-event-emitting fixtures
                    // continue to pass — the CLI's `record` subcommand
                    // enables this flag when driving the full
                    // `move test --trace` pipeline.
                    //
                    // We emit one event per *toplevel* OpenFrame so
                    // multi-frame fixtures (the on-chain `sui replay`
                    // path that opens several test frames in sequence)
                    // surface one row per frame.  Inner / nested
                    // OpenFrames remain unannotated: their existence
                    // is already captured by the `call_entry` records
                    // that drive the call-trace pane.
                    if options.emit_test_entry_event {
                        let qualified = format!("{}::{}", frame.module.name, frame.function_name);
                        TraceWriter::register_special_event(
                            writer,
                            EventLogKind::TraceLogEvent,
                            "MoveTestEntry",
                            &qualified,
                        );
                    }
                }
                let display_name = qualified_function_name(
                    outer_module.as_deref(),
                    &frame.module,
                    &frame.function_name,
                );
                let fn_id =
                    TraceWriter::ensure_function_id(writer, &display_name, source_path, Line(1));

                // Stage each formal parameter as a call arg via the
                // canonical TraceWriter::arg(name, value) entry point so
                // the call record carries them.  Move's v3 trace format
                // delivers parameters per `OpenFrame` in `frame.parameters`
                // — mirrors the Ruby (1.22) / JS (1.38) call-arg staging
                // pattern used by other recorder audits.
                //
                // When the package's `build/<pkg>/debug_info/<module>.json`
                // sidecar is available we look up the source-level
                // parameter names (`a`, `b`, `factor`, ...) from the
                // Move compiler's debug info; otherwise we fall back to
                // synthetic positional names (`arg0`, `arg1`, ...) so
                // hand-rolled NDJSON fixtures without a `build/` dir
                // continue to work.
                let frame_dbg = debug_info.function(&frame.module.name, frame.binary_member_index);
                for (idx, param) in frame.parameters.iter().enumerate() {
                    let value = convert_trace_value(param, &mut type_ids, writer);
                    let arg_name = frame_dbg
                        .and_then(|d| d.parameters.get(idx))
                        .cloned()
                        .unwrap_or_else(|| format!("arg{idx}"));
                    let _ = TraceWriter::arg(writer, &arg_name, value);
                }

                TraceWriter::register_call(writer, fn_id, vec![]);
            }

            TraceEvent::CloseFrame { return_values, .. } => {
                // Move functions can return zero, one, or multiple values
                // (tuple returns, e.g. `fun compute_triple(...): (u64, u64, u64)`).
                // The Move VM v3 trace format encodes the *full* tuple as
                // distinct elements in the `return_` array.  Prior behaviour
                // surfaced only `return_[0]`, silently truncating the tuple
                // and leaving consumers no way to recover the trailing
                // elements (see
                // `tests/test_full_coverage.rs::test_nested_calls_tuple_return_decodes_full_tuple`).
                // We now wrap >=2 return values in a typed
                // `ValueRecord::Tuple`; single-value and void returns are
                // unchanged so the existing scalar-return tests stay intact.
                let ret_val = match return_values.len() {
                    0 => NONE_VALUE,
                    1 => convert_trace_value(&return_values[0], &mut type_ids, writer),
                    _ => {
                        let elements: Vec<ValueRecord> = return_values
                            .iter()
                            .map(|v| convert_trace_value(v, &mut type_ids, writer))
                            .collect();
                        ValueRecord::Tuple {
                            elements,
                            type_id: type_ids.tuple_id,
                        }
                    }
                };

                TraceWriter::register_return(writer, ret_val);
                frame_stack.pop();
                // Reset per-frame step bookkeeping on close as well so the
                // caller's next instruction (which resumes after the call)
                // is judged against the caller's own prior pc/line, not a
                // stale value carried over from the callee.
                prev_line = None;
                prev_pc = None;
            }

            TraceEvent::Instruction {
                pc, instruction, ..
            } => {
                // Emit a CodeTracer `step` event when the source map
                // resolves the current `(module, pc)` AND EITHER:
                //   (a) the resolved source line differs from `prev_line`
                //       (we crossed a source-line boundary), OR
                //   (b) `prev_pc` is set and `*pc < prev_pc` (the Move VM
                //       took a backward branch — i.e. another iteration of
                //       a loop body re-entered the same source line).
                //
                // The (b) clause is what makes a `for i in 0..N { body }`
                // loop surface N step events at the body line rather than
                // just one — without it the dedup in (a) would collapse
                // every iteration into a single step and the recorder would
                // not satisfy the spec at
                // `metacraft-specs/policies/recorder-test-requirements.md`
                // ("a `for i in 0..10` loop must produce exactly 10 step
                // events at the loop body").
                //
                // The explicit `SourceMapResolver` (legacy, hand-built)
                // takes precedence so synthetic NDJSON tests pinned to
                // bespoke pc→line tables stay green.  When that returns
                // nothing we fall back to the per-package debug-info
                // sidecar loaded from `build/<pkg>/debug_info/<module>.json`
                // — this is the canonical path for traces generated by
                // `sui move test --trace` / `aptos move trace` and is
                // what the `ct record path/to/foo.move` flow relies on
                // for real per-source-line stepping.  Without it, every
                // bytecode instruction would resolve to line 1 (the
                // synthetic `TraceWriter::start` line) and the GUI
                // call-trace / step-navigation panes would collapse the
                // entire run to a single step (see
                // GUI-Test-Stabilization-2026-05.status.org M5).
                let module_name = current_module.as_deref().unwrap_or("");
                let line = source_map
                    .lookup(module_name, *pc)
                    .map(|(_, line)| line)
                    .or_else(|| {
                        if !options.resolve_pc_lines_from_debug_info {
                            return None;
                        }
                        // The PC space is per-function in the Move VM,
                        // so we resolve against the *currently active*
                        // frame's binary_member_index (top of
                        // `frame_stack`).  Falling back to "any function
                        // in the module" would mis-attribute PCs that
                        // happen to collide across functions.
                        let (_module_top, bmi) = frame_stack.last()?;
                        debug_info
                            .function(module_name, *bmi)
                            .and_then(|fi| fi.pc_to_line(*pc))
                    });
                // Look up the column for this PC via the debug-info
                // `code_map` entry (1-based byte column on the resolved
                // line).  Gated on `resolve_pc_lines_from_debug_info`
                // for the same reason as the line fallback — the
                // legacy `SourceMapResolver` carries only lines, so
                // when it answers we forward `column=None` (line-only
                // step).  Synthesised entry/exit ops without a
                // `code_map` entry also resolve to `None`, which the
                // column-aware reader treats as "no column override".
                let column = if options.resolve_pc_lines_from_debug_info {
                    frame_stack
                        .last()
                        .and_then(|(_m, bmi)| debug_info.function(module_name, *bmi))
                        .and_then(|fi| fi.pc_to_column(*pc))
                } else {
                    None
                };
                if let Some(line) = line {
                    let line_changed = prev_line != Some(line);
                    let backward_jump = prev_pc.is_some_and(|p| *pc < p);
                    if line_changed || backward_jump {
                        // M-move: column-aware step emission.  Forward
                        // `Some(column)` when the compiler debug info
                        // recorded one (1-based byte column on `line`);
                        // forward `None` otherwise so the reader records
                        // a line-only step (DeltaLine, no DeltaColumn
                        // override).  Mirrors the EVM recorder's M14
                        // pattern.
                        TraceWriter::register_step_with_column(
                            writer,
                            source_path,
                            Line(line as i64),
                            column.map(|c| Line(c as i64)),
                        );
                        prev_line = Some(line);
                    }
                }
                prev_pc = Some(*pc);
                // Remember the instruction so the upcoming `Effect::Pop` /
                // `Effect::ExecutionError` pair can recognise an abort and
                // recover the code (see `last_popped_value` below).
                prev_instruction = Some(instruction.clone());
            }

            TraceEvent::Effect(effect) => match effect {
                Effect::Write {
                    location,
                    root_value_after_write,
                } => {
                    let name = local_slot_name(&frame_stack, &debug_info, location.local_index());
                    let val = convert_move_value(
                        root_value_after_write.inner_value(),
                        &mut type_ids,
                        writer,
                    );
                    TraceWriter::register_variable_with_full_value(writer, &name, val);
                }
                Effect::Read {
                    location,
                    root_value_read,
                    ..
                } => {
                    let name = local_slot_name(&frame_stack, &debug_info, location.local_index());
                    let val =
                        convert_move_value(root_value_read.inner_value(), &mut type_ids, writer);
                    TraceWriter::register_variable_with_full_value(writer, &name, val);
                }
                Effect::Push(value) => {
                    let val = convert_move_value(value.inner_value(), &mut type_ids, writer);
                    TraceWriter::register_variable_with_full_value(writer, "stack_top", val);
                }
                Effect::Pop(value) => {
                    let inner = value.inner_value();
                    let val = convert_move_value(inner, &mut type_ids, writer);
                    TraceWriter::register_variable_with_full_value(writer, "popped", val);
                    // Cache the popped value verbatim so an immediately-
                    // following `Effect::ExecutionError("ABORTED")` can
                    // stitch the abort code into the io_event payload.
                    last_popped_value = Some(inner.clone());
                }
                Effect::ExecutionError(error) => {
                    // Surface execution errors as an Error special event so
                    // they appear in the CodeTracer event-log pane.  Prior
                    // behaviour silently dropped the message via eprintln!.
                    // `metadata` carries a stable tag the frontend can key
                    // off; `content` is the human-readable message.
                    //
                    // The Move VM v3 trace format encodes a Move `abort` as
                    //   Instruction{ABORT} -> Effect::Pop(<code>) -> Effect::ExecutionError("ABORTED")
                    // and the `ExecutionError` payload itself carries the
                    // bare marker `"ABORTED"` with no code.  Recover the
                    // code from the immediately preceding `Pop` so that
                    // distinct abort sites surface distinct io_event
                    // payloads (e.g. `"ABORTED: code 42"` rather than the
                    // ambiguous `"ABORTED"` shared by every abort).
                    let content = if error == "ABORTED"
                        && prev_instruction.as_deref() == Some("ABORT")
                        && let Some(code) =
                            last_popped_value.as_ref().and_then(abort_code_from_value)
                    {
                        format!("ABORTED: code {code}")
                    } else {
                        error.clone()
                    };
                    TraceWriter::register_special_event(
                        writer,
                        EventLogKind::Error,
                        "MoveExecutionError",
                        &content,
                    );
                }
                Effect::DataLoad { .. } => {
                    // Data load effects are informational; nothing to emit.
                }
            },

            TraceEvent::External(ext) => {
                // External effects represent Sui-specific side effects
                // (object transfers, native event emission, etc.) that
                // are recorded outside the Move VM's stack/local state.
                // Route them through the canonical `register_special_event`
                // entry point so the trace contains a structured record
                // rather than silently dropping the data.  Use
                // `TraceLogEvent` (the "structured trace event" bucket
                // shared with Solana's non-stdout syscalls — see Solana
                // audit 1.44) so they do not pollute the program-log
                // pane.
                TraceWriter::register_special_event(
                    writer,
                    EventLogKind::TraceLogEvent,
                    "MoveExternalEffect",
                    &ext.kind,
                );
            }
        }
    }

    // Close the implicit toplevel frame opened by `TraceWriter::start`.
    TraceWriter::register_return(writer, NONE_VALUE);

    Ok(())
}

/// Holds pre-registered CodeTracer type IDs for Move types.
///
/// Per-struct `TypeId`s for named Move structs are populated lazily via
/// [`TypeIds::ensure_struct`] as the converter encounters them — the
/// generic `struct_id` (registered with the bare `"struct"` lang-name)
/// remains as the fallback for anonymous / un-named struct payloads.
pub struct TypeIds {
    pub u8_id: TypeId,
    pub u16_id: TypeId,
    pub u32_id: TypeId,
    pub u64_id: TypeId,
    pub u128_id: TypeId,
    pub u256_id: TypeId,
    pub bool_id: TypeId,
    pub address_id: TypeId,
    pub struct_id: TypeId,
    pub vector_id: TypeId,
    pub tuple_id: TypeId,
    pub string_id: TypeId,
    /// Type id used for `&T` immutable references — registered with
    /// `TypeKind::Ref` so downstream consumers can distinguish a
    /// borrowed reference from an owned printed-form payload.  Move
    /// has no per-pointee reference type registry today, so all
    /// `&T` / `&mut T` parameters share this generic id (the
    /// pointee carries its own typed `TypeId` on the `dereferenced`
    /// child).
    pub ref_id: TypeId,
    /// Type id used for `&mut T` mutable references — registered as
    /// `TypeKind::Ref` with the lang-name `"&mut"` so the Reference
    /// payload's mutability is reflected in the registered type as
    /// well as the `mutable` flag on the value record.
    pub mut_ref_id: TypeId,
    /// Lazily registered per-struct-name `TypeKind::Struct` ids so that
    /// `ValueRecord::Struct { type_id, .. }` carries a stable, named
    /// type for downstream consumers (ct-print, frontend) instead of
    /// the generic `struct_id` fallback.
    structs: HashMap<String, TypeId>,
}

impl TypeIds {
    fn register(writer: &mut dyn TraceWriter) -> Self {
        Self {
            u8_id: TraceWriter::ensure_type_id(writer, TypeKind::Int, "u8"),
            u16_id: TraceWriter::ensure_type_id(writer, TypeKind::Int, "u16"),
            u32_id: TraceWriter::ensure_type_id(writer, TypeKind::Int, "u32"),
            u64_id: TraceWriter::ensure_type_id(writer, TypeKind::Int, "u64"),
            u128_id: TraceWriter::ensure_type_id(writer, TypeKind::Int, "u128"),
            u256_id: TraceWriter::ensure_type_id(writer, TypeKind::Int, "u256"),
            bool_id: TraceWriter::ensure_type_id(writer, TypeKind::Bool, "bool"),
            address_id: TraceWriter::ensure_type_id(writer, TypeKind::String, "address"),
            struct_id: TraceWriter::ensure_type_id(writer, TypeKind::Struct, "struct"),
            vector_id: TraceWriter::ensure_type_id(writer, TypeKind::Seq, "vector"),
            tuple_id: TraceWriter::ensure_type_id(writer, TypeKind::Tuple, "tuple"),
            string_id: TraceWriter::ensure_type_id(writer, TypeKind::String, "string"),
            ref_id: TraceWriter::ensure_type_id(writer, TypeKind::Ref, "&"),
            mut_ref_id: TraceWriter::ensure_type_id(writer, TypeKind::Ref, "&mut"),
            structs: HashMap::new(),
        }
    }

    /// Resolve (and lazily register) a `TypeKind::Struct` `TypeId` for the
    /// given Move struct name.  An empty `name` falls back to the
    /// generic `struct_id` so anonymous structs still get a typed
    /// `ValueRecord::Struct` payload.
    fn ensure_struct(&mut self, writer: &mut dyn TraceWriter, name: &str) -> TypeId {
        self.ensure_parameterised_struct(writer, name, &[])
    }

    /// Resolve (and lazily register) a `TypeKind::Struct` `TypeId` keyed
    /// by `(name, type_args)`.  Phantom-type instantiations of the same
    /// underlying struct (e.g. `TypedCoin<USD>` vs `TypedCoin<EUR>`) share
    /// runtime layout but must surface as distinct `TypeId`s so downstream
    /// consumers can tell them apart in the type table.  When `type_args`
    /// is empty we register the struct under its bare `name` (preserving
    /// pre-2026-05 behaviour); when present, we register under the
    /// canonical `name<arg0,arg1,...>` form using each arg's `name`
    /// extracted from the trace JSON (e.g. `TypedCoin<USD>`).  See
    /// `tests/test_full_coverage.rs::test_phantom_types_test_via_ct_print_full`.
    fn ensure_parameterised_struct(
        &mut self,
        writer: &mut dyn TraceWriter,
        name: &str,
        type_args: &[serde_json::Value],
    ) -> TypeId {
        if name.is_empty() && type_args.is_empty() {
            return self.struct_id;
        }
        let key = parameterised_struct_key(name, type_args);
        if key.is_empty() {
            return self.struct_id;
        }
        if let Some(id) = self.structs.get(&key) {
            return *id;
        }
        let id = TraceWriter::ensure_type_id(writer, TypeKind::Struct, &key);
        self.structs.insert(key, id);
        id
    }
}

/// Build a canonical type-table key for a (potentially generic) Move
/// struct.  Empty `type_args` returns the bare `name`; non-empty
/// arguments format as `name<arg0,arg1,...>` where each `argN` is the
/// recursively-formatted struct/primitive name from the trace JSON.
/// Unknown arg shapes fall back to the JSON's debug rendering so the
/// key remains stable and unique.
/// Peek the trace events to recover the first source line that the
/// converter would emit a step at.  Returns `None` when no Instruction
/// resolves to a source line via either the explicit `SourceMapResolver`
/// or the package's loaded `DebugInfo` (typically: synthetic hand-rolled
/// fixtures with no `build/` debug info and an empty resolver).
///
/// Used by `convert_trace_into_writer` to seed `TraceWriter::start` with
/// the user-facing first line of the trace so the entry step lines up
/// with the variables that get written there — see the comment at the
/// call site for the GUI-test-stabilization rationale.
fn first_step_line(
    events: &[TraceEvent],
    source_map: &SourceMapResolver,
    debug_info: &DebugInfo,
    consult_debug_info: bool,
) -> Option<u32> {
    // Walk the event stream while maintaining a minimal frame stack so
    // an `Instruction` is resolved against the *currently active*
    // function's binary_member_index (the Move VM's PC space is
    // per-function, so picking the wrong function would yield the
    // wrong line — or no mapping at all).
    let mut stack: Vec<(String, u64)> = Vec::new();
    for event in events {
        match event {
            TraceEvent::OpenFrame { frame, .. } => {
                stack.push((frame.module.name.clone(), frame.binary_member_index));
            }
            TraceEvent::CloseFrame { .. } => {
                stack.pop();
            }
            TraceEvent::Instruction { pc, .. } => {
                let (module_name, bmi) = match stack.last() {
                    Some(top) => top,
                    None => continue,
                };
                if let Some((_, line)) = source_map.lookup(module_name, *pc) {
                    return Some(line);
                }
                if consult_debug_info
                    && let Some(line) = debug_info
                        .function(module_name, *bmi)
                        .and_then(|fi| fi.pc_to_line(*pc))
                {
                    return Some(line);
                }
            }
            _ => {}
        }
    }
    None
}

fn parameterised_struct_key(name: &str, type_args: &[serde_json::Value]) -> String {
    if type_args.is_empty() {
        return name.to_string();
    }
    let parts: Vec<String> = type_args.iter().map(format_type_arg).collect();
    format!("{name}<{}>", parts.join(","))
}

fn format_type_arg(arg: &serde_json::Value) -> String {
    if let Some(s) = arg.as_str() {
        return s.to_string();
    }
    if let Some(obj) = arg.as_object()
        && let Some(struct_obj) = obj.get("struct").and_then(|v| v.as_object())
    {
        let inner_name = struct_obj
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let inner_args: Vec<serde_json::Value> = struct_obj
            .get("type_args")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        return parameterised_struct_key(inner_name, &inner_args);
    }
    arg.to_string()
}

/// Encode a `u128` as its minimal big-endian unsigned-integer byte
/// representation — the canonical payload shape for
/// `ValueRecord::BigInt { b, negative: false, .. }`.  Leading zero
/// bytes are stripped so a small value packs to a few bytes; the
/// special case `0` returns a single `0` byte rather than an empty
/// slice so the magnitude survives a round trip.
fn u128_be_bytes_trimmed(v: u128) -> Vec<u8> {
    let bytes = v.to_be_bytes();
    let first_nonzero = bytes
        .iter()
        .position(|&b| b != 0)
        .unwrap_or(bytes.len() - 1);
    bytes[first_nonzero..].to_vec()
}

/// Compute the display name to register in the function table for a
/// frame.  Cross-module calls *within user code* (i.e. callee module
/// at address `0x0` whose name differs from the outer/toplevel
/// frame's module) qualify as `module::function_name` so a
/// `public(friend)` callee surfaces with its owning module preserved
/// across the boundary.  All other frames (toplevel, same-module
/// helpers, stdlib `0x1`/`0x2`/... natives) keep their bare
/// `function_name` to remain compatible with the M5–M8 fixtures'
/// function-table assertions.  See
/// `tests/test_full_coverage.rs::test_friend_visibility_test_via_ct_print_full`.
fn qualified_function_name(
    outer_module: Option<&str>,
    module: &crate::move_types::ModuleId,
    function_name: &str,
) -> String {
    let user_code_address = matches!(
        module.address.as_str(),
        "0x0" | "0x00" | "0000000000000000000000000000000000000000000000000000000000000000",
    );
    let cross_user_module = user_code_address
        && outer_module.is_some_and(|outer| outer != module.name && !module.name.is_empty());
    if cross_user_module {
        format!("{}::{}", module.name, function_name)
    } else {
        function_name.to_string()
    }
}

/// True when a `(module_name, module_address)` pair points at the Sui
/// `0x2::event` module — the home of `sui::event::emit`.  Sui's address
/// renders both as the short `"0x2"` (synthetic / hand-crafted traces)
/// and as the 64-hex-digit zero-padded form (`"0000…0002"`) that the
/// real Sui binary emits, so we accept either.
fn is_sui_event_emit(module_name: &str, module_address: &str) -> bool {
    if module_name != "event" {
        return false;
    }
    matches!(
        module_address,
        "0x2" | "0x02" | "0000000000000000000000000000000000000000000000000000000000000002"
    )
}

/// Render the typed payload of a `sui::event::emit<T>(payload)` call
/// into a single JSON-string snippet that ct-print --full surfaces in
/// the `text` field of the resulting `TraceLogEvent`.  The output is
/// shaped as `{"struct":"<name>","fields":{<name>:<value>,...}}` so
/// downstream consumers can recover the event's struct name + field
/// map without re-parsing a printed form.  Non-struct payloads (the
/// type system forbids them today, but we are defensive) surface as a
/// bare `{"payload":"<debug>"}` object.
fn render_move_event_payload(value: &SerializableMoveValue) -> String {
    match value {
        SerializableMoveValue::Struct { value: content } => {
            let struct_name = content
                .type_
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let mut fields = serde_json::Map::new();
            for (fname, fval) in &content.fields {
                fields.insert(fname.clone(), move_value_to_json(fval));
            }
            let mut root = serde_json::Map::new();
            root.insert("struct".to_string(), serde_json::Value::String(struct_name));
            root.insert("fields".to_string(), serde_json::Value::Object(fields));
            serde_json::Value::Object(root).to_string()
        }
        other => {
            let mut root = serde_json::Map::new();
            root.insert("payload".to_string(), move_value_to_json(other));
            serde_json::Value::Object(root).to_string()
        }
    }
}

/// Project a `SerializableMoveValue` into a `serde_json::Value` for the
/// `MoveEvent` TraceLogEvent metadata payload.  Strings/addresses/bools
/// surface as their JSON-native shape; numeric values surface as JSON
/// numbers (u64 stays as a number; u128 falls back to a string when it
/// exceeds the JSON safe-integer range).  Compound values recurse.
fn move_value_to_json(value: &SerializableMoveValue) -> serde_json::Value {
    use serde_json::Value as J;
    match value {
        SerializableMoveValue::U8 { value: v } => J::from(*v),
        SerializableMoveValue::U16 { value: v } => J::from(*v),
        SerializableMoveValue::U32 { value: v } => J::from(*v),
        SerializableMoveValue::U64 { value: v } => J::from(*v),
        SerializableMoveValue::U128 { value: v } => {
            if *v <= u64::MAX as u128 {
                J::from(*v as u64)
            } else {
                J::String(v.to_string())
            }
        }
        SerializableMoveValue::U256 { value: v } => J::String(v.clone()),
        SerializableMoveValue::Bool { value: v } => J::from(*v),
        SerializableMoveValue::Address { value: v } => J::String(v.clone()),
        SerializableMoveValue::Struct { value: content } => {
            let mut fields = serde_json::Map::new();
            for (fname, fval) in &content.fields {
                fields.insert(fname.clone(), move_value_to_json(fval));
            }
            let struct_name = content
                .type_
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let mut obj = serde_json::Map::new();
            obj.insert("struct".to_string(), J::String(struct_name.to_string()));
            obj.insert("fields".to_string(), J::Object(fields));
            J::Object(obj)
        }
        SerializableMoveValue::Vector { elements } => {
            J::Array(elements.iter().map(move_value_to_json).collect())
        }
        SerializableMoveValue::Variant { tag, fields, type_ } => {
            let mut obj = serde_json::Map::new();
            obj.insert("variant_type".to_string(), J::String(type_.clone()));
            obj.insert("tag".to_string(), J::from(*tag));
            obj.insert(
                "fields".to_string(),
                J::Array(fields.iter().map(move_value_to_json).collect()),
            );
            J::Object(obj)
        }
    }
}

/// Render the abort code for the integer Move VM value popped immediately
/// before an `Effect::ExecutionError("ABORTED")`.  Returns the canonical
/// decimal representation Move source uses for `abort` codes (`u64`),
/// or `None` if the popped value is not a numeric scalar — in which case
/// the caller falls back to the bare `"ABORTED"` marker.
fn abort_code_from_value(value: &SerializableMoveValue) -> Option<String> {
    match value {
        SerializableMoveValue::U8 { value } => Some(value.to_string()),
        SerializableMoveValue::U16 { value } => Some(value.to_string()),
        SerializableMoveValue::U32 { value } => Some(value.to_string()),
        SerializableMoveValue::U64 { value } => Some(value.to_string()),
        SerializableMoveValue::U128 { value } => Some(value.to_string()),
        SerializableMoveValue::U256 { value } => Some(value.clone()),
        _ => None,
    }
}

/// Convert a `TraceValue` (the outer wrapper that distinguishes owned
/// runtime values from `&T` / `&mut T` borrows) into a CodeTracer
/// `ValueRecord`.
///
/// `TraceValue::RuntimeValue` unwraps to the underlying owned value via
/// [`convert_move_value`].  `TraceValue::ImmRef` / `TraceValue::MutRef`
/// surface as a typed `ValueRecord::Reference` carrying the pointee
/// (`dereferenced`) and a synthetic `address` derived from the borrow
/// `Location` so reference identity survives a round-trip through the
/// trace — the previous behaviour stripped the borrow wrapper via
/// `inner_value()` and the call_entry args silently rendered as the
/// pointee's printed snapshot, indistinguishable from owned values
/// (see `tests/test_full_coverage.rs::test_references_use_typed_reference_value_record`).
pub fn convert_trace_value(
    value: &TraceValue,
    type_ids: &mut TypeIds,
    writer: &mut dyn TraceWriter,
) -> ValueRecord {
    match value {
        TraceValue::RuntimeValue { value } => convert_move_value(value, type_ids, writer),
        TraceValue::ImmRef { location, snapshot } => {
            let dereferenced = convert_move_value(snapshot, type_ids, writer);
            ValueRecord::Reference {
                dereferenced: Box::new(dereferenced),
                address: synthetic_ref_address(location),
                mutable: false,
                type_id: type_ids.ref_id,
            }
        }
        TraceValue::MutRef { location, snapshot } => {
            let dereferenced = convert_move_value(snapshot, type_ids, writer);
            ValueRecord::Reference {
                dereferenced: Box::new(dereferenced),
                address: synthetic_ref_address(location),
                mutable: true,
                type_id: type_ids.mut_ref_id,
            }
        }
    }
}

/// Resolve a local-slot index to a human-readable name for the
/// currently-executing Move frame.
///
/// When the Move compiler's `<package_root>/build/<pkg>/debug_info/
/// <module>.json` sidecar is available for the active frame's
/// `(module_name, binary_member_index)`, the name comes straight from
/// the `function_map[idx].locals[slot]` entry — i.e. the source-level
/// identifier (with the compiler-internal `#scope#unique` suffix
/// stripped by [`crate::move_debug_info::strip_scope_suffix`]).
///
/// When the debug info is absent (synthetic NDJSON fixtures, or a
/// real fixture whose `build/` directory was pruned), the fallback is
/// the historical synthetic name `local_<slot>` that the recorder has
/// emitted since M2 — pinned by the `tests/test_comprehensive.rs` and
/// `tests/test_converter.rs` suites which feed hand-rolled NDJSON
/// without any package context.  This means the `local_<N>` shape
/// remains the contract for synthetic fixtures while real
/// `sui move test --trace-execution` captures get spec-correct names.
fn local_slot_name(frame_stack: &[(String, u64)], debug_info: &DebugInfo, slot: u64) -> String {
    if let Some((module_name, binary_member_index)) = frame_stack.last()
        && let Some(fn_dbg) = debug_info.function(module_name, *binary_member_index)
        && let Some(name) = fn_dbg.locals.get(slot as usize)
    {
        return name.clone();
    }
    format!("local_{slot}")
}

/// Derive a stable synthetic `u64` address for a reference borrowed
/// from a Move stack location.  The Move VM v3 trace format does not
/// expose raw runtime addresses, but every borrow points to a
/// `Location` that uniquely identifies the storage cell within the
/// invocation: a `(frame_id, local_index)` pair, optionally indexed
/// into a struct field.  We pack `frame_id` into the high 32 bits and
/// `local_index` into the low 32 bits so two borrows of the same local
/// surface the same `address`, while borrows of different locals (or
/// the same local across frames) get distinct values.  Indexed
/// borrows fold the field index into the low half via XOR — coarse
/// but sufficient to keep field-level borrows distinct from the
/// surrounding struct borrow without inventing addresses out of thin
/// air.
fn synthetic_ref_address(location: &Location) -> u64 {
    match location {
        Location::Local(frame_id, local_index) => (*frame_id << 32) | (*local_index & 0xffff_ffff),
        Location::Indexed(inner, field_index) => {
            synthetic_ref_address(inner) ^ ((*field_index & 0xffff_ffff) << 16)
        }
    }
}

/// Convert a single `SerializableMoveValue` into a CodeTracer `ValueRecord`.
///
/// Compound Move values (`Struct`, `Vector`) emit typed
/// `ValueRecord::Struct` / `ValueRecord::Sequence` payloads carrying
/// recursively-converted children, so downstream consumers (ct-print,
/// frontend object inspector) can walk fields/elements rather than
/// re-parsing the historical printed-form fallback.  `&mut TypeIds` is
/// taken so per-struct-name `TypeId`s can be lazily registered against
/// `writer` as the converter discovers them — the `writer` argument is
/// required because `ensure_type_id` is the canonical FFI entry point
/// for type registration.
pub fn convert_move_value(
    value: &SerializableMoveValue,
    type_ids: &mut TypeIds,
    writer: &mut dyn TraceWriter,
) -> ValueRecord {
    match value {
        SerializableMoveValue::U8 { value: v } => ValueRecord::Int {
            i: *v as i64,
            type_id: type_ids.u8_id,
        },
        SerializableMoveValue::U16 { value: v } => ValueRecord::Int {
            i: *v as i64,
            type_id: type_ids.u16_id,
        },
        SerializableMoveValue::U32 { value: v } => ValueRecord::Int {
            i: *v as i64,
            type_id: type_ids.u32_id,
        },
        SerializableMoveValue::U64 { value: v } => {
            // u64 values up to i64::MAX fit in `ValueRecord::Int { i: i64 }`;
            // anything larger would silently wrap to a negative i64, so we
            // surface those as a BigInt with the canonical big-endian
            // unsigned-integer payload.  See
            // `tests/test_full_coverage.rs::test_boolean_and_integers_u128_overflow_uses_bigint`.
            if *v <= i64::MAX as u64 {
                ValueRecord::Int {
                    i: *v as i64,
                    type_id: type_ids.u64_id,
                }
            } else {
                ValueRecord::BigInt {
                    b: u128_be_bytes_trimmed(*v as u128),
                    negative: false,
                    type_id: type_ids.u64_id,
                }
            }
        }
        SerializableMoveValue::U128 { value: v } => {
            // u128 values up to i64::MAX fit in `ValueRecord::Int`; anything
            // larger MUST surface as a `BigInt` so downstream consumers see
            // the full magnitude.  Truncating to i64 silently loses the
            // high 64 bits and (for values > i64::MAX) flips the sign — see
            // `tests/test_full_coverage.rs::test_boolean_and_integers_u128_overflow_uses_bigint`.
            if *v <= i64::MAX as u128 {
                ValueRecord::Int {
                    i: *v as i64,
                    type_id: type_ids.u128_id,
                }
            } else {
                ValueRecord::BigInt {
                    b: u128_be_bytes_trimmed(*v),
                    negative: false,
                    type_id: type_ids.u128_id,
                }
            }
        }
        SerializableMoveValue::U256 { value: v } => ValueRecord::String {
            text: v.clone(),
            type_id: type_ids.u256_id,
        },
        SerializableMoveValue::Bool { value: v } => ValueRecord::Bool {
            b: *v,
            type_id: type_ids.bool_id,
        },
        SerializableMoveValue::Address { value: v } => ValueRecord::String {
            text: v.clone(),
            type_id: type_ids.address_id,
        },
        SerializableMoveValue::Struct { value: content } => {
            // Extract the struct type name from the JSON value if present;
            // an empty name falls back to the generic `struct_id` so the
            // payload still surfaces as a typed `ValueRecord::Struct`.
            // Also pull `type_args` so phantom-type instantiations of the
            // same underlying struct (e.g. `TypedCoin<USD>` vs
            // `TypedCoin<EUR>`) register as distinct `TypeId`s — see
            // `TypeIds::ensure_parameterised_struct`.
            let type_name = content
                .type_
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let type_args: Vec<serde_json::Value> = content
                .type_
                .get("type_args")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let type_id = type_ids.ensure_parameterised_struct(writer, &type_name, &type_args);
            let field_values: Vec<ValueRecord> = content
                .fields
                .iter()
                .map(|(_, val)| convert_move_value(val, type_ids, writer))
                .collect();
            ValueRecord::Struct {
                field_values,
                type_id,
            }
        }
        SerializableMoveValue::Vector { elements } => {
            let elements: Vec<ValueRecord> = elements
                .iter()
                .map(|e| convert_move_value(e, type_ids, writer))
                .collect();
            ValueRecord::Sequence {
                elements,
                is_slice: false,
                type_id: type_ids.vector_id,
            }
        }
        SerializableMoveValue::Variant { tag, fields, type_ } => {
            // Emit a typed `ValueRecord::Variant { discriminator, contents,
            // type_id }` so downstream consumers (ct-print --full, frontend
            // object inspector) can walk the variant's payload structurally
            // instead of re-parsing the historical printed-form fallback
            // (`"Variant#1(42)"`).  The discriminator is the tag's decimal
            // string — prepending the `type_` (e.g.
            // `0x1::option::Option::Variant#1`) when it is non-empty so the
            // surface name is human-meaningful for Move 2024 enums and
            // `Option<T>`/`Result<T,E>` shapes.
            //
            // Contents are wrapped in a `ValueRecord::Struct` whose
            // `field_values` carry the recursively-converted variant
            // payload — Move variants are positional tuples in the trace
            // format (no field names), so we surface them as a Struct
            // with the variant's owned `type_id` so the variant *and*
            // its contents share the same type identity in the type
            // table.  This matches `ValueRecord::Variant`'s documented
            // shape ("contents: usually a Struct or a Tuple") and keeps
            // typed walks through `value["contents"]["field_values"]`
            // cheap for ct-print consumers.
            let discriminator = if type_.is_empty() {
                format!("Variant#{tag}")
            } else {
                format!("{type_}::Variant#{tag}")
            };
            let variant_type_id = type_ids.ensure_struct(writer, type_);
            let field_values: Vec<ValueRecord> = fields
                .iter()
                .map(|f| convert_move_value(f, type_ids, writer))
                .collect();
            let contents = ValueRecord::Struct {
                field_values,
                type_id: variant_type_id,
            };
            ValueRecord::Variant {
                discriminator,
                contents: Box::new(contents),
                type_id: variant_type_id,
            }
        }
    }
}
