//! Aptos trace data adapters.
//!
//! Aptos has much less trace infrastructure than Sui:
//! - `MOVE_VM_TRACE` env var produces minimal CSV (function_name,PC per instruction, no values)
//! - `--profile-gas` produces rich HTML/JSON flamegraphs but gas-centric, not state-centric
//! - No equivalent to Sui's move-trace-format with stack values and variable state
//!
//! ## Limitations vs Sui
//!
//! | Feature                  | Sui                          | Aptos                           |
//! |--------------------------|------------------------------|---------------------------------|
//! | Variable values          | Full typed values per step   | Not available                   |
//! | Stack inspection         | Push/Pop with values         | Not available                   |
//! | Reference tracking       | ImmRef/MutRef with snapshots | Not available                   |
//! | Source mapping            | .mvsm files with code_map   | PC only (no source mapping)     |
//! | Trace format             | Structured NDJSON (v3)       | Minimal CSV (function,PC)       |
//! | Gas profiling            | gas_left per instruction     | Rich flamegraph JSON            |
//! | Replay                   | sui replay --trace           | aptos move replay --profile-gas |
//! | Historical state         | sui_tryGetPastObject         | REST /v1/accounts/.../resources |

use std::collections::HashMap;
use std::path::Path;

use codetracer_trace_types::{Line, TypeKind, ValueRecord};
use codetracer_trace_writer_nim::trace_writer::TraceWriter;
use codetracer_trace_writer_nim::{TraceEventsFileFormat, create_trace_writer};
use eyre::{Result, eyre};
use serde::Deserialize;

/// The on-disk container produced by the recorder is always the canonical
/// multi-stream CTFS bundle.  Pre-2026-05-08 the recorder accepted a
/// `TraceEventsFileFormat` parameter and the CLI exposed a `--format` flag;
/// the convention now mandates CTFS-only output (see
/// `Recorder-CLI-Conventions.md` §4 in `codetracer-specs`).
const TRACE_FORMAT: TraceEventsFileFormat = TraceEventsFileFormat::Ctfs;

// ---------------------------------------------------------------------------
// MOVE_VM_TRACE CSV parsing
// ---------------------------------------------------------------------------

/// A single entry from Aptos's `MOVE_VM_TRACE` CSV output.
///
/// Each line in the CSV is: `function_name,PC`
/// This provides only the execution trace skeleton -- no variable values,
/// no stack state, no reference tracking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AptosTraceEntry {
    /// Fully qualified function name (e.g., `0x1::coin::transfer`).
    pub function_name: String,
    /// Program counter (bytecode offset within the function).
    pub pc: u64,
}

/// Parse Aptos `MOVE_VM_TRACE` CSV output into trace entries.
///
/// The CSV format is one entry per line: `function_name,PC`
/// Lines that are empty or cannot be parsed are skipped with a warning.
pub fn parse_move_vm_trace_csv(csv_data: &str) -> Vec<AptosTraceEntry> {
    let mut entries = Vec::new();

    for (line_no, line) in csv_data.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Split on the last comma to handle function names that may contain commas
        // (though in practice they don't in Move).
        if let Some(comma_pos) = line.rfind(',') {
            let func_name = line[..comma_pos].trim();
            let pc_str = line[comma_pos + 1..].trim();

            match pc_str.parse::<u64>() {
                Ok(pc) => {
                    entries.push(AptosTraceEntry {
                        function_name: func_name.to_string(),
                        pc,
                    });
                }
                Err(_) => {
                    eprintln!(
                        "warning: skipping MOVE_VM_TRACE line {}: invalid PC '{pc_str}'",
                        line_no + 1
                    );
                }
            }
        } else {
            eprintln!(
                "warning: skipping MOVE_VM_TRACE line {}: no comma separator",
                line_no + 1
            );
        }
    }

    entries
}

// ---------------------------------------------------------------------------
// --profile-gas JSON parsing
// ---------------------------------------------------------------------------

/// Gas profile data from Aptos's `--profile-gas` JSON output.
///
/// The gas profiler produces a call tree with gas costs per node.
/// This is gas-centric (not state-centric), but provides useful
/// structural information about the execution.
#[derive(Debug, Clone, Deserialize)]
pub struct AptosGasProfile {
    /// Total gas used by the transaction.
    #[serde(default)]
    pub gas_used: u64,
    /// Root node of the gas profile call tree.
    #[serde(default)]
    pub call_tree: Option<GasProfileNode>,
    /// Transaction metadata (if present).
    #[serde(default)]
    pub metadata: Option<GasProfileMetadata>,
}

/// A node in the gas profile call tree.
#[derive(Debug, Clone, Deserialize)]
pub struct GasProfileNode {
    /// Function or operation name.
    pub name: String,
    /// Gas cost of this node (excluding children).
    #[serde(default)]
    pub gas_cost: u64,
    /// Total gas cost including children.
    #[serde(default)]
    pub total_gas: u64,
    /// Child nodes in the call tree.
    #[serde(default)]
    pub children: Vec<GasProfileNode>,
}

/// Metadata from the gas profile.
#[derive(Debug, Clone, Deserialize)]
pub struct GasProfileMetadata {
    /// Transaction version (ledger version).
    #[serde(default)]
    pub transaction_version: Option<u64>,
    /// Gas unit price.
    #[serde(default)]
    pub gas_unit_price: Option<u64>,
    /// Max gas amount.
    #[serde(default)]
    pub max_gas_amount: Option<u64>,
}

/// Parse `--profile-gas` JSON output into an `AptosGasProfile`.
pub fn parse_gas_profile_json(json_data: &str) -> Result<AptosGasProfile> {
    serde_json::from_str(json_data).map_err(|e| eyre!("failed to parse gas profile JSON: {e}"))
}

/// Flatten a gas profile call tree into a list of (function_name, gas_cost, total_gas) tuples.
///
/// This is useful for merging gas data with trace entries.
pub fn flatten_gas_profile(node: &GasProfileNode) -> Vec<(String, u64, u64)> {
    let mut result = vec![(node.name.clone(), node.gas_cost, node.total_gas)];
    for child in &node.children {
        result.extend(flatten_gas_profile(child));
    }
    result
}

// ---------------------------------------------------------------------------
// REST API types for historical state
// ---------------------------------------------------------------------------

/// A resource entry from the Aptos REST API.
///
/// Returned by `GET /v1/accounts/{address}/resources?ledger_version={version}`.
#[derive(Debug, Clone, Deserialize)]
pub struct AptosResourceData {
    /// Resource type (e.g., `0x1::coin::CoinStore<0x1::aptos_coin::AptosCoin>`).
    #[serde(rename = "type")]
    pub resource_type: String,
    /// Resource data as a JSON value (structure depends on the resource type).
    pub data: serde_json::Value,
}

/// Configuration for Aptos REST API calls.
#[derive(Debug, Clone)]
pub struct AptosRestConfig {
    /// Base URL for the Aptos REST API (e.g., `https://fullnode.mainnet.aptoslabs.com/v1`).
    pub base_url: String,
}

impl AptosRestConfig {
    /// Create a config for the Aptos mainnet.
    pub fn mainnet() -> Self {
        Self {
            base_url: "https://fullnode.mainnet.aptoslabs.com/v1".to_string(),
        }
    }

    /// Create a config for the Aptos testnet.
    pub fn testnet() -> Self {
        Self {
            base_url: "https://fullnode.testnet.aptoslabs.com/v1".to_string(),
        }
    }

    /// Create a config for a local Aptos node.
    pub fn local() -> Self {
        Self {
            base_url: "http://localhost:8080/v1".to_string(),
        }
    }

    /// Build the URL for fetching account resources at a specific ledger version.
    pub fn resources_url(&self, address: &str, ledger_version: Option<u64>) -> String {
        let base = format!("{}/accounts/{}/resources", self.base_url, address);
        match ledger_version {
            Some(version) => format!("{base}?ledger_version={version}"),
            None => base,
        }
    }
}

/// Parse a JSON array of resource entries from the Aptos REST API.
pub fn parse_resources_response(json_data: &str) -> Result<Vec<AptosResourceData>> {
    serde_json::from_str(json_data)
        .map_err(|e| eyre!("failed to parse Aptos resources response: {e}"))
}

// ---------------------------------------------------------------------------
// Merged trace entry (MOVE_VM_TRACE + gas profiler)
// ---------------------------------------------------------------------------

/// A trace entry enriched with gas profiler data when available.
#[derive(Debug, Clone)]
pub struct AptosEnrichedEntry {
    /// The base trace entry (function + PC).
    pub trace: AptosTraceEntry,
    /// Gas cost for this function (from gas profiler, if available).
    pub gas_cost: Option<u64>,
    /// Total gas including callees (from gas profiler, if available).
    pub total_gas: Option<u64>,
}

/// Merge MOVE_VM_TRACE entries with gas profiler data.
///
/// For each trace entry, looks up gas data by function name from the
/// flattened gas profile. If multiple entries share a function name,
/// the first match is used (gas profiler data is aggregate, not per-PC).
pub fn merge_trace_and_gas(
    trace_entries: &[AptosTraceEntry],
    gas_profile: Option<&AptosGasProfile>,
) -> Vec<AptosEnrichedEntry> {
    // Build a lookup map from function name to gas data.
    let gas_map: HashMap<String, (u64, u64)> = match gas_profile {
        Some(profile) => {
            if let Some(ref tree) = profile.call_tree {
                flatten_gas_profile(tree)
                    .into_iter()
                    .map(|(name, cost, total)| (name, (cost, total)))
                    .collect()
            } else {
                HashMap::new()
            }
        }
        None => HashMap::new(),
    };

    trace_entries
        .iter()
        .map(|entry| {
            let gas_data = gas_map.get(&entry.function_name);
            AptosEnrichedEntry {
                trace: entry.clone(),
                gas_cost: gas_data.map(|(cost, _)| *cost),
                total_gas: gas_data.map(|(_, total)| *total),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Convert Aptos trace to CodeTracer format
// ---------------------------------------------------------------------------

/// Convert Aptos trace entries into CodeTracer trace files.
///
/// **Important limitations vs Sui:**
/// - No variable values: CodeTracer steps will not have associated variable state
/// - No stack inspection: no Push/Pop value events
/// - No reference tracking: no ImmRef/MutRef events
/// - Source mapping is best-effort: only function-level granularity from MOVE_VM_TRACE
///
/// When gas profiler data is available, gas cost information is included as
/// annotations on function calls.
pub fn convert_aptos_trace(
    entries: &[AptosEnrichedEntry],
    source_path: &Path,
    out_dir: &Path,
) -> Result<()> {
    if entries.is_empty() {
        return Err(eyre!("no trace entries to convert"));
    }

    let program_name = source_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "aptos_program".to_string());

    let mut writer = create_trace_writer(&program_name, &[], TRACE_FORMAT);

    // Set up output files.
    std::fs::create_dir_all(out_dir).map_err(|e| eyre!("cannot create output dir: {e}"))?;

    // CTFS multi-stream container — `db-backend` infers the format
    // from the `.bin` extension.  No JSON / legacy-binary alternative
    // is exposed.
    let events_path = out_dir.join("trace.bin");

    TraceWriter::begin_writing_trace_events(&mut *writer, &events_path)
        .map_err(|e| eyre!("{e}"))?;

    // Opt the writer into column-aware step encoding *before* the first
    // `start` / `register_step_with_column` call.  Sticky for the
    // lifetime of the trace; gates the writer's `DeltaColumn` (tag
    // 0x07) emission path plus the `meta.dat` bit 4 flag
    // (`FLAG_HAS_COLUMN_AWARE_STEPS`).  Legacy backends keep the trait
    // default (no-op); the Nim writer used here flips the real flag.
    //
    // Aptos's `MOVE_VM_TRACE` CSV carries only `(function_name, PC)`
    // — no source-level column information.  We still emit the flag so
    // the resulting trace is wire-compatible with the column-aware
    // replay reader; every step's column is `None`, which the reader
    // interprets as "no column override" (line-only step).  When Aptos
    // upstream eventually exposes per-PC source locations, the call
    // site below can start forwarding a real `Some(column)`.
    TraceWriter::enable_column_aware_steps(&mut *writer);

    // M-capability-flags: DELIBERATELY leave both capability bits
    // clear.  Aptos's CSV trace format doesn't expose per-PC source
    // columns yet, so individual VM instructions cannot be told apart
    // on a multi-statement source line.  Per spec, advertising the
    // capabilities would lie to the GUI about the recorder's column
    // precision; the GUI must continue to hide its per-column
    // breakpoint and per-column motion affordances on Aptos traces.
    // When upstream Aptos surfaces per-PC source locations, mirror
    // the Sui-side path in `converter.rs` and call both
    // `enable_column_breakpoints_support` / `enable_column_motions_support`.

    // Register the source path together with its per-line byte counts
    // (paths.dat Layout A) so the column-aware reader can map the
    // writer-side `global_position_index` back to (line, column).
    // Aptos has no source mapping today, so we emit an empty
    // line-length table when the source can't be read — the writer
    // accepts this and `column=None` steps stay well-formed.
    let line_lengths = match std::fs::read_to_string(source_path) {
        Ok(src) => crate::move_debug_info::compute_line_lengths(&src),
        Err(_) => Vec::new(),
    };
    if let Err(err) =
        TraceWriter::register_path_with_line_lengths(&mut *writer, source_path, &line_lengths)
    {
        eprintln!(
            "[codetracer-move-recorder] register_path_with_line_lengths failed for {}: {} \
             (column resolution will fall back to None for this file)",
            source_path.display(),
            err,
        );
    }

    // Start the trace.
    TraceWriter::start(&mut *writer, source_path, Line(1));

    // Register a type for gas annotations.
    let gas_type_id = TraceWriter::ensure_type_id(&mut *writer, TypeKind::Int, "gas");

    // Track function call stack for proper Call/Return pairing.
    let mut current_function: Option<String> = None;

    for (step_idx, entry) in entries.iter().enumerate() {
        let step_line = (step_idx + 1) as i64;
        let func_name = &entry.trace.function_name;

        // If we've entered a new function, emit Call/Return.
        if current_function.as_ref() != Some(func_name) {
            // Close previous function if any.
            if current_function.is_some() {
                TraceWriter::register_return(&mut *writer, codetracer_trace_types::NONE_VALUE);
            }

            // Open new function.
            let fn_id =
                TraceWriter::ensure_function_id(&mut *writer, func_name, source_path, Line(1));
            TraceWriter::register_call(&mut *writer, fn_id, vec![]);

            // If gas data is available, emit it as an annotation variable.
            if let Some(gas_cost) = entry.gas_cost {
                TraceWriter::register_variable_with_full_value(
                    &mut *writer,
                    "gas_cost",
                    ValueRecord::Int {
                        i: gas_cost as i64,
                        type_id: gas_type_id,
                    },
                );
            }
            if let Some(total_gas) = entry.total_gas {
                TraceWriter::register_variable_with_full_value(
                    &mut *writer,
                    "total_gas",
                    ValueRecord::Int {
                        i: total_gas as i64,
                        type_id: gas_type_id,
                    },
                );
            }

            current_function = Some(func_name.clone());
        }

        // Emit a step for each trace entry.
        // Note: without source maps, we use incrementing line numbers as placeholders.
        // Column-aware encoding: Aptos's MOVE_VM_TRACE has no source-
        // column info, so we forward `None` — the reader records a
        // line-only step (DeltaLine, no DeltaColumn override).
        TraceWriter::register_step_with_column(&mut *writer, source_path, Line(step_line), None);
    }

    // Close the last function.
    if current_function.is_some() {
        TraceWriter::register_return(&mut *writer, codetracer_trace_types::NONE_VALUE);
    }

    // Finish writing.
    TraceWriter::finish_writing_trace_events(&mut *writer).map_err(|e| eyre!("{e}"))?;
    writer
        .write_meta_dat("codetracer-move-recorder")
        .map_err(|e| eyre!("{e}"))?;
    writer.close().map_err(|e| eyre!("{e}"))?;

    Ok(())
}

/// Return a human-readable summary of Aptos limitations compared to Sui.
///
/// This is used in CLI output and error messages to set user expectations.
pub fn aptos_limitations_summary() -> &'static str {
    "\
Aptos trace support has significant limitations compared to Sui:

1. NO VARIABLE VALUES: Aptos MOVE_VM_TRACE only records function names and
   program counters. Variable values, stack state, and reference tracking
   are not available. CodeTracer steps will show execution flow but not
   variable state.

2. NO STRUCTURED TRACE FORMAT: Unlike Sui's rich NDJSON trace format (v3)
   with OpenFrame/CloseFrame/Effect events, Aptos produces minimal CSV.
   The trace provides execution skeleton only.

3. GAS-CENTRIC PROFILING: Aptos's --profile-gas produces flamegraph data
   focused on gas consumption, not execution state. Gas costs are included
   as annotations when available.

4. LIMITED SOURCE MAPPING: Without .mvsm-equivalent source map files,
   source location resolution is function-level only (no line-level mapping
   within functions).

5. HISTORICAL STATE: Account resources can be fetched via the REST API at
   specific ledger versions (GET /v1/accounts/{addr}/resources?ledger_version=N),
   but this provides account-level state snapshots, not per-instruction state
   changes.

For full debugging with variable inspection, consider using Sui's trace
infrastructure instead."
}
