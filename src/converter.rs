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
    let metadata_path = out_dir.join("trace_metadata.json");
    let paths_path = out_dir.join("trace_paths.json");

    TraceWriter::begin_writing_trace_events(&mut *writer, &events_path)
        .map_err(|e| eyre!("{e}"))?;
    TraceWriter::begin_writing_trace_metadata(&mut *writer, &metadata_path)
        .map_err(|e| eyre!("{e}"))?;
    TraceWriter::begin_writing_trace_paths(&mut *writer, &paths_path).map_err(|e| eyre!("{e}"))?;

    // -- 3. Convert events into writer ----------------------------------------
    convert_trace_into_writer(trace_data, source_map, source_path, &mut *writer)?;

    // -- 4. Finish writing ----------------------------------------------------
    TraceWriter::finish_writing_trace_events(&mut *writer).map_err(|e| eyre!("{e}"))?;
    TraceWriter::finish_writing_trace_metadata(&mut *writer).map_err(|e| eyre!("{e}"))?;
    TraceWriter::finish_writing_trace_paths(&mut *writer).map_err(|e| eyre!("{e}"))?;
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

    // -- 2. Start the trace ----------------------------------------------------
    TraceWriter::start(writer, source_path, Line(1));

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
    let mut prev_line: Option<u32> = None;
    let mut prev_pc: Option<u64> = None;
    let mut current_module: Option<String> = None;
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

    for event in &events {
        match event {
            TraceEvent::OpenFrame { frame, .. } => {
                current_module = Some(frame.module.name.clone());
                // Reset per-frame step bookkeeping: a backward-pc relative
                // to the *caller's* last instruction is meaningless for the
                // callee, and the callee's first instruction must always
                // get a step.  See the `prev_line`/`prev_pc` notes above.
                prev_line = None;
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

                if outer_module.is_none() {
                    outer_module = Some(frame.module.name.clone());
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
                // We synthesise positional names (`arg0`, `arg1`, ...)
                // because Sui's frame schema only carries the *values*
                // of the parameters, not their declared identifiers.
                // Higher-fidelity names would require parsing the
                // function's source-map (.mvsm) — tracked as a follow-up.
                for (idx, param) in frame.parameters.iter().enumerate() {
                    let value = convert_trace_value(param, &mut type_ids, writer);
                    let _ = TraceWriter::arg(writer, &format!("arg{idx}"), value);
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
                // When the source map has no entry for `(module, pc)` we
                // emit nothing; the .mvsm parser is a follow-up so today
                // the integration fixtures use `SourceMapResolver::empty()`
                // and only the implicit `start()` step surfaces at the
                // function level (the per-source-line behaviour is
                // exercised by the synthetic-source-map unit tests in
                // `tests/test_comprehensive.rs`).
                let module_name = current_module.as_deref().unwrap_or("");
                if let Some((_, line)) = source_map.lookup(module_name, *pc) {
                    let line_changed = prev_line != Some(line);
                    let backward_jump = prev_pc.is_some_and(|p| *pc < p);
                    if line_changed || backward_jump {
                        TraceWriter::register_step(writer, source_path, Line(line as i64));
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
                    let name = format!("local_{}", location.local_index());
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
                    let name = format!("local_{}", location.local_index());
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
