//! Convert Move trace events into CodeTracer trace format.
//!
//! Reads NDJSON trace data (version 3), walks the trace events, and
//! emits CodeTracer steps, calls, returns, and variable records.

use std::path::Path;

use codetracer_trace_types::{Line, TypeKind, ValueRecord, NONE_VALUE};
use codetracer_trace_writer::trace_writer::TraceWriter;
use codetracer_trace_writer::{TraceEventsFileFormat, create_trace_writer};
use eyre::{Result, eyre};

use crate::move_types::{Effect, Frame, SerializableMoveValue, TraceEvent, VersionHeader};
use crate::source_map::SourceMapResolver;

/// Convert Move NDJSON trace data into CodeTracer trace files.
///
/// `trace_data` must be decompressed NDJSON (one JSON object per line).
/// The first line must be a `{"version":3}` header.
pub fn convert_trace(
    trace_data: &[u8],
    source_map: &SourceMapResolver,
    source_path: &Path,
    out_dir: &Path,
    format: TraceEventsFileFormat,
) -> Result<()> {
    // -- 1. Parse NDJSON -------------------------------------------------------
    let text = std::str::from_utf8(trace_data)
        .map_err(|e| eyre!("trace data is not valid UTF-8: {e}"))?;

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

    // -- 2. Create trace writer ------------------------------------------------
    let program_name = source_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "move_program".to_string());

    let mut writer = create_trace_writer(&program_name, &[], format);

    // -- 3. Set up output files ------------------------------------------------
    std::fs::create_dir_all(out_dir)
        .map_err(|e| eyre!("cannot create output dir: {e}"))?;

    let events_filename = match format {
        TraceEventsFileFormat::Json => "trace.json",
        TraceEventsFileFormat::Binary | TraceEventsFileFormat::BinaryV0 => "trace.bin",
    };
    let events_path = out_dir.join(events_filename);
    let metadata_path = out_dir.join("trace_metadata.json");
    let paths_path = out_dir.join("trace_paths.json");

    TraceWriter::begin_writing_trace_events(&mut *writer, &events_path)
        .map_err(|e| eyre!("{e}"))?;
    TraceWriter::begin_writing_trace_metadata(&mut *writer, &metadata_path)
        .map_err(|e| eyre!("{e}"))?;
    TraceWriter::begin_writing_trace_paths(&mut *writer, &paths_path)
        .map_err(|e| eyre!("{e}"))?;

    // -- 4. Start the trace ----------------------------------------------------
    TraceWriter::start(&mut *writer, source_path, Line(1));

    // Register common Move types.
    let type_ids = TypeIds::register(&mut *writer);

    // -- 5. Walk trace events --------------------------------------------------
    let mut prev_line: Option<u32> = None;
    let mut current_module: Option<String> = None;

    for event in &events {
        match event {
            TraceEvent::OpenFrame { frame, .. } => {
                let Frame {
                    function_name,
                    module,
                    ..
                } = frame;

                current_module = Some(module.name.clone());

                let fn_id = TraceWriter::ensure_function_id(
                    &mut *writer,
                    function_name,
                    source_path,
                    Line(1),
                );

                TraceWriter::register_call(&mut *writer, fn_id, vec![]);
            }

            TraceEvent::CloseFrame {
                return_: return_vals,
                ..
            } => {
                let ret_val = return_vals
                    .as_ref()
                    .and_then(|vals| vals.first())
                    .map(|v| convert_move_value(v, &type_ids))
                    .unwrap_or(NONE_VALUE);

                TraceWriter::register_return(&mut *writer, ret_val);
            }

            TraceEvent::Instruction { pc, .. } => {
                let module_name = current_module.as_deref().unwrap_or("");
                if let Some((_, line)) = source_map.lookup(module_name, *pc)
                    && prev_line != Some(line)
                {
                    TraceWriter::register_step(
                        &mut *writer,
                        source_path,
                        Line(line as i64),
                    );
                    prev_line = Some(line);
                }
            }

            TraceEvent::Effect { effect } => match effect {
                Effect::Write {
                    location, value, ..
                } => {
                    let name = format!("local_{}", location.local_index);
                    let val = convert_move_value(value.inner_value(), &type_ids);
                    TraceWriter::register_variable_with_full_value(
                        &mut *writer,
                        &name,
                        val,
                    );
                }
                Effect::Read {
                    location, value, ..
                } => {
                    let name = format!("local_{}", location.local_index);
                    let val = convert_move_value(value.inner_value(), &type_ids);
                    TraceWriter::register_variable_with_full_value(
                        &mut *writer,
                        &name,
                        val,
                    );
                }
                Effect::Push { value } => {
                    let val = convert_move_value(value.inner_value(), &type_ids);
                    TraceWriter::register_variable_with_full_value(
                        &mut *writer,
                        "stack_top",
                        val,
                    );
                }
                Effect::Pop { value } => {
                    let val = convert_move_value(value.inner_value(), &type_ids);
                    TraceWriter::register_variable_with_full_value(
                        &mut *writer,
                        "popped",
                        val,
                    );
                }
                Effect::ExecutionError { error } => {
                    eprintln!("Move execution error: {error}");
                }
                Effect::DataLoad { .. } => {
                    // Data load effects are informational; nothing to emit.
                }
            },

            TraceEvent::External { .. } => {
                // External effects are informational for now.
            }
        }
    }

    // -- 6. Finish writing -----------------------------------------------------
    // Close the implicit toplevel frame opened by `TraceWriter::start`.
    TraceWriter::register_return(&mut *writer, NONE_VALUE);

    TraceWriter::finish_writing_trace_events(&mut *writer)
        .map_err(|e| eyre!("{e}"))?;
    TraceWriter::finish_writing_trace_metadata(&mut *writer)
        .map_err(|e| eyre!("{e}"))?;
    TraceWriter::finish_writing_trace_paths(&mut *writer)
        .map_err(|e| eyre!("{e}"))?;

    Ok(())
}

/// Holds pre-registered CodeTracer type IDs for Move types.
pub struct TypeIds {
    pub u8_id: codetracer_trace_types::TypeId,
    pub u16_id: codetracer_trace_types::TypeId,
    pub u32_id: codetracer_trace_types::TypeId,
    pub u64_id: codetracer_trace_types::TypeId,
    pub u128_id: codetracer_trace_types::TypeId,
    pub u256_id: codetracer_trace_types::TypeId,
    pub bool_id: codetracer_trace_types::TypeId,
    pub address_id: codetracer_trace_types::TypeId,
    pub struct_id: codetracer_trace_types::TypeId,
    pub vector_id: codetracer_trace_types::TypeId,
    pub string_id: codetracer_trace_types::TypeId,
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
            string_id: TraceWriter::ensure_type_id(writer, TypeKind::String, "string"),
        }
    }
}

/// Convert a single `SerializableMoveValue` into a CodeTracer `ValueRecord`.
pub fn convert_move_value(
    value: &SerializableMoveValue,
    type_ids: &TypeIds,
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
        SerializableMoveValue::U64 { value: v } => ValueRecord::Int {
            i: *v as i64,
            type_id: type_ids.u64_id,
        },
        SerializableMoveValue::U128 { value: v } => ValueRecord::Int {
            i: *v as i64,
            type_id: type_ids.u128_id,
        },
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
        SerializableMoveValue::Struct { fields, type_ } => {
            let field_values: Vec<ValueRecord> = fields
                .iter()
                .map(|f| convert_move_value(f, type_ids))
                .collect();
            let field_strs: Vec<String> = field_values
                .iter()
                .enumerate()
                .map(|(i, v)| format!("field_{i}: {}", value_record_to_display(v)))
                .collect();
            let display = if type_.is_empty() {
                format!("{{ {} }}", field_strs.join(", "))
            } else {
                format!("{} {{ {} }}", type_, field_strs.join(", "))
            };
            ValueRecord::String {
                text: display,
                type_id: type_ids.struct_id,
            }
        }
        SerializableMoveValue::Vector { elements } => {
            let elem_strs: Vec<String> = elements
                .iter()
                .map(|e| {
                    let val = convert_move_value(e, type_ids);
                    value_record_to_display(&val)
                })
                .collect();
            ValueRecord::String {
                text: format!("[{}]", elem_strs.join(", ")),
                type_id: type_ids.vector_id,
            }
        }
        SerializableMoveValue::Variant {
            tag,
            fields,
            type_,
        } => {
            let field_strs: Vec<String> = fields
                .iter()
                .map(|f| {
                    let val = convert_move_value(f, type_ids);
                    value_record_to_display(&val)
                })
                .collect();
            let display = if type_.is_empty() {
                format!("Variant#{tag}({})", field_strs.join(", "))
            } else {
                format!("{type_}::Variant#{tag}({})", field_strs.join(", "))
            };
            ValueRecord::String {
                text: display,
                type_id: type_ids.string_id,
            }
        }
    }
}

/// Simple display helper for ValueRecord (used in struct/vector rendering).
fn value_record_to_display(val: &ValueRecord) -> String {
    match val {
        ValueRecord::Int { i, .. } => i.to_string(),
        ValueRecord::Bool { b, .. } => b.to_string(),
        ValueRecord::String { text, .. } => text.clone(),
        _ => "<complex>".to_string(),
    }
}
