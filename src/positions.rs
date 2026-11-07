//! Read a recorded container's step positions and call frames, without
//! decoding a single value.
//!
//! A chain-snapshot producer publishes, beside each container, the position of
//! every step (path, line, column) and the call frames.  Reading those through
//! `ct-print --events` decodes every step's variable values too, which for a
//! single mainnet Sui transaction took minutes and up to 17 GB of memory on a
//! container of a few megabytes.  The Nim reader answers the same questions
//! from the step and call streams directly: `step_locations_with_columns` in
//! bulk, and `call_fields` per call.

use std::path::Path;

use codetracer_trace_writer_nim::NimTraceReaderHandle;
use eyre::{Result, eyre};
use serde_json::{Value, json};

/// Steps are read in chunks of this many, so memory stays proportional to the
/// output rather than to a reader-side copy of it.
const STEP_CHUNK: u64 = 1 << 16;

/// The container's path table, every step's `(path id, line, column)` (a
/// column of 0 — "not recorded" — becomes `null`), and every call's function
/// name, depth and entry/exit steps, as one JSON document:
///
/// ```json
/// {"paths": [...], "steps": n, "pathId": [...], "line": [...], "column": [...],
///  "calls": [{"function": "...", "depth": d, "entryStep": s, "exitStep": e}, ...]}
/// ```
pub fn container_positions(ct: &Path) -> Result<Value> {
    let path_str = ct
        .to_str()
        .ok_or_else(|| eyre!("container path is not UTF-8: {}", ct.display()))?;
    let reader =
        NimTraceReaderHandle::open(path_str).map_err(|e| eyre!("cannot open {path_str}: {e}"))?;

    let paths: Vec<String> = (0..reader.path_count())
        .map(|id| reader.path(id).map_err(|e| eyre!("path {id}: {e}")))
        .collect::<Result<_>>()?;

    let n = reader.step_count();
    let (mut path_id, mut line, mut column) = (Vec::new(), Vec::new(), Vec::new());
    let mut start = 0;
    while start < n {
        let count = STEP_CHUNK.min(n - start);
        let len = count as usize;
        let (mut p, mut l, mut c) = (vec![0u64; len], vec![0u64; len], vec![0u64; len]);
        let written = if reader.has_column_aware_steps() {
            reader.step_locations_with_columns(start, count, &mut p, &mut l, &mut c)
        } else {
            reader.step_locations(start, count, &mut p, &mut l)
        }
        .map_err(|e| eyre!("steps {start}..{}: {e}", start + count))?;
        if written != count {
            return Err(eyre!("steps {start}..: read {written} of {count}"));
        }
        path_id.extend(p);
        line.extend(l);
        column.extend(c.into_iter().map(|c| (c != 0).then_some(c)));
        start += count;
    }

    let mut calls = Vec::new();
    for key in 0..reader.call_count() {
        let (function_id, _parent, entry_step, exit_step, depth, _children) = reader
            .call_fields(key)
            .map_err(|e| eyre!("call {key}: {e}"))?;
        let function = reader
            .function(function_id)
            .map_err(|e| eyre!("function {function_id}: {e}"))?;
        calls.push(json!({
            "function": function,
            "depth": depth,
            "entryStep": entry_step,
            "exitStep": exit_step,
        }));
    }

    Ok(json!({
        "paths": paths,
        "steps": n,
        "pathId": path_id,
        "line": line,
        "column": column,
        "calls": calls,
    }))
}
