//! Move bytecode source-map ("debug info") loader.
//!
//! The Sui Move v3 trace format that this recorder consumes carries
//! only `locals_types` (per-slot type tags) on each `OpenFrame` — there
//! are no source-level identifiers for any local.  Without an external
//! source of names, the converter falls back to synthetic
//! `local_<slot_index>` strings (see `Frame::local_name` below) which
//! makes the resulting CTFS bundle decidedly less readable than e.g.
//! the Aptos / EVM recorders.
//!
//! The Move *compiler*, however, does emit per-function debug info
//! when it builds a package: the file
//! `<package_root>/build/<PackageName>/debug_info/<Module>.json`
//! (alongside the `.mvd` binary form) is a superset of the on-chain
//! source-map and includes:
//!
//!   * `function_map[binary_member_index].locals` — a `Vec<(name,
//!     source_loc)>` whose order matches the bytecode slot allocation
//!     (slot 0 == `locals[0]`, slot 1 == `locals[1]`, ...).
//!   * `function_map[binary_member_index].parameters` — same shape,
//!     for function arguments.
//!   * `function_map[binary_member_index].definition_location` — a
//!     `(file_hash, start, end)` byte-range covering the function name
//!     in the source file, used here to recover the qualified
//!     `module::function` name for matching against the trace's
//!     `frame.function_name`.
//!
//! The format is documented (informally) at
//! `https://github.com/move-language/move/tree/main/external-crates/move/crates/move-bytecode-source-map`
//! and the upstream Rust crate is `move-bytecode-source-map`; we
//! deliberately do NOT pull that crate in (it has a deep transitive
//! dependency on the entire Move VM and isn't published to crates.io
//! in a form that fits the workspace's Nix-pinned toolchain).  Instead
//! we parse the JSON sidecar with `serde_json`, since the JSON shape
//! is stable across Move 2024 releases and matches the format the
//! Sui CLI emits at `sui move build` time.
//!
//! The loader is a best-effort enrichment: if the debug-info file
//! isn't found, the converter silently falls back to `local_<N>` so
//! synthetic NDJSON fixtures (which never have a build/) keep working
//! unchanged.  When it IS found, the converter substitutes the
//! source-level local names (with the compiler-internal `#scope#unique`
//! suffix stripped) so a real `sui move test --trace-execution`
//! capture surfaces user-meaningful identifiers.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Per-function debug info extracted from `<Module>.json`.
#[derive(Debug, Clone)]
pub struct FunctionDebugInfo {
    /// Source-extracted function name (e.g. `test_loops`, `add_points`).
    pub name: String,
    /// Per-slot local names in slot-allocation order.  Index `i` is
    /// the name of slot `i` for `Effect::Read`/`Effect::Write` events
    /// whose `Location::Local` carries `local_index == i`.
    ///
    /// Names are post-processed: the compiler-internal `#scope#unique`
    /// suffix (e.g. `accumulator#1#0`) is stripped to recover the
    /// source-level identifier (`accumulator`).  Compiler-generated
    /// temps that lack a source name (e.g. `%#1`) are passed through
    /// unchanged so they remain visually distinct from real user
    /// bindings.
    pub locals: Vec<String>,
    /// Per-position parameter names (same `#scope#unique` stripping).
    /// Used to label `arg0`, `arg1`, ... with source identifiers when
    /// the debug info is present.
    pub parameters: Vec<String>,
    /// Per-bytecode-PC source line numbers (1-indexed).  The Move
    /// compiler's `code_map` records a byte range per PC inside the
    /// source file; we resolve the range start to a line number while
    /// loading the debug info so the converter can look it up in O(1)
    /// without re-scanning the source on every `Instruction` event.
    ///
    /// Indexed by the bytecode `pc` (u64 in the trace's `Instruction`
    /// event).  Missing entries (`None` returned from `pc_to_line`)
    /// mean the compiler did not record a source mapping for that
    /// instruction — typically a synthesised prologue/epilogue op.
    pub pc_to_line: HashMap<u64, u32>,
    /// Per-bytecode-PC source column numbers (1-indexed), aligned with
    /// `pc_to_line`.  Derived from the `code_map` range start minus the
    /// containing line's first-byte offset (+1 for 1-based columns) so
    /// the column-aware replay reader can map back from the writer-side
    /// `global_position_index` to (line, column).  Missing entries mean
    /// the PC had no source mapping (synthesised op) and the converter
    /// emits `Option<column>=None` for that step.
    pub pc_to_column: HashMap<u64, u32>,
}

/// Per-module debug info, keyed by the bytecode function index
/// (`binary_member_index` in the trace's `OpenFrame`).
#[derive(Debug, Default, Clone)]
pub struct ModuleDebugInfo {
    pub functions: HashMap<u64, FunctionDebugInfo>,
    /// Path to the on-disk `.move` source file the compiler used when
    /// building this module, when discoverable.  Carried alongside the
    /// per-PC line/column tables so the converter can register the
    /// `paths.dat` Layout A line-length table (column-aware mode)
    /// without re-walking the package layout for every module.
    pub source_path: Option<PathBuf>,
    /// Per-line byte-length table (line `i+1`'s UTF-8 byte count
    /// excluding the trailing `\n`).  Pre-computed at debug-info load
    /// time and forwarded verbatim to
    /// `TraceWriter::register_path_with_line_lengths` so the column-
    /// aware reader can decode columns from the running
    /// `global_position_index`.  Empty when the source file could not
    /// be read.
    pub line_lengths: Vec<u32>,
}

/// Workspace of debug-info modules, keyed by module short name
/// (e.g. `flow_test`).  Looked up by the trace's `frame.module.name`.
#[derive(Debug, Default, Clone)]
pub struct DebugInfo {
    modules: HashMap<String, ModuleDebugInfo>,
}

impl DebugInfo {
    /// Construct an empty debug-info store.  The converter uses this
    /// when no `build/` directory is found alongside the source.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Attempt to load debug info for the package containing the given
    /// `.move` source file.  Walks up the directory tree until a
    /// `build/<PackageName>/debug_info/` directory is found and parses
    /// every `*.json` file inside it (the `.mvd` binary sibling is
    /// ignored — the JSON form is the canonical, version-tagged
    /// representation).
    ///
    /// Returns `Self::empty()` (never an error) if no debug info is
    /// available.  This keeps the caller's contract simple: synthetic
    /// fixtures without a real `build/` directory continue to work
    /// unchanged.
    pub fn discover(source_path: &Path) -> Self {
        match Self::try_discover(source_path) {
            Ok(info) => info,
            Err(_) => Self::empty(),
        }
    }

    fn try_discover(source_path: &Path) -> Result<Self, std::io::Error> {
        // The Sui Move build layout places debug info at
        //   <package_root>/build/<PackageName>/debug_info/<Module>.json
        // and the source file lives at
        //   <package_root>/sources/<Module>.move
        // so walk up from the source file looking for a sibling `build/`
        // directory; the package_root is the parent of `sources/`.
        let mut cursor = source_path.parent();
        while let Some(dir) = cursor {
            let build_dir = dir.join("build");
            if build_dir.is_dir() {
                let mut store = Self::empty();
                store.load_from_build_dir(&build_dir)?;
                return Ok(store);
            }
            cursor = dir.parent();
        }
        Ok(Self::empty())
    }

    fn load_from_build_dir(&mut self, build_dir: &Path) -> Result<(), std::io::Error> {
        // build/ has one subdirectory per package; each carries its own
        // debug_info/.  Walk all of them — typical packages declare
        // exactly one but resource-heavy fixtures can split across
        // multiple sub-packages.
        for pkg in fs::read_dir(build_dir)? {
            let pkg = pkg?;
            let info_dir = pkg.path().join("debug_info");
            if info_dir.is_dir() {
                self.load_from_info_dir(&info_dir);
            }
        }
        Ok(())
    }

    fn load_from_info_dir(&mut self, info_dir: &Path) {
        let Ok(entries) = fs::read_dir(info_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Some((module_name, info)) = parse_module_debug_json(&path) {
                self.modules.insert(module_name, info);
            }
        }
    }

    /// Look up the per-function debug info for `(module_name,
    /// binary_member_index)`.  Returns `None` when either the module
    /// or the function is unknown.
    pub fn function(
        &self,
        module_name: &str,
        binary_member_index: u64,
    ) -> Option<&FunctionDebugInfo> {
        self.modules
            .get(module_name)
            .and_then(|m| m.functions.get(&binary_member_index))
    }
}

/// Parse a single `<Module>.json` debug-info file.  Returns
/// `(module_short_name, ModuleDebugInfo)` on success, `None` if the
/// file is malformed or missing required fields.
fn parse_module_debug_json(path: &Path) -> Option<(String, ModuleDebugInfo)> {
    let bytes = fs::read(path).ok()?;
    let raw: RawDebugInfo = serde_json::from_slice(&bytes).ok()?;

    let module_short_name = raw.module_name.get(1).cloned()?;

    // `from_file_path` records the *absolute* source path as it was on the
    // machine that compiled the package.  Prefer it, but it is not
    // portable: a debug-info JSON committed as a test fixture (or a
    // `build/` tree copied between machines / OSes) carries a path that
    // does not exist on the current host.  Fall back to the canonical Sui
    // package layout, where this JSON lives at
    //   <package_root>/build/<PackageName>/debug_info/<Module>.json
    // and the source at
    //   <package_root>/sources/<Module>.move
    // so the function/parameter-name extraction still works regardless of
    // where (or on which OS) the package was originally built.
    let source_text = fs::read_to_string(&raw.from_file_path).ok().or_else(|| {
        let package_root = path // <Module>.json
            .parent() // debug_info/
            .and_then(Path::parent) // <PackageName>/
            .and_then(Path::parent) // build/
            .and_then(Path::parent)?; // <package_root>
        let candidate = package_root
            .join("sources")
            .join(format!("{module_short_name}.move"));
        fs::read_to_string(candidate).ok()
    })?;

    // Precompute a byte-offset -> 1-indexed-line lookup for the source
    // text so we can resolve every `code_map` entry without re-scanning
    // the file per PC.  Storing one `Vec<u32>` of line offsets keeps
    // the resolver hot in cache and turns the lookup into a single
    // binary search per PC.
    let line_starts = build_line_starts(&source_text);

    let mut functions = HashMap::new();
    for (idx_str, fn_raw) in raw.function_map {
        let idx: u64 = idx_str.parse().ok()?;
        let name = slice_or_empty(&source_text, &fn_raw.definition_location);
        if name.is_empty() {
            // Skip the synthetic per-module entry: the Move compiler
            // emits one extra `function_map` entry whose
            // `definition_location` covers the whole module body, not
            // a function name.  It carries no useful per-slot info
            // for the converter.
            continue;
        }
        let locals = fn_raw
            .locals
            .into_iter()
            .map(|(raw_name, _)| strip_scope_suffix(&raw_name))
            .collect();
        let parameters = fn_raw
            .parameters
            .into_iter()
            .map(|(raw_name, _)| strip_scope_suffix(&raw_name))
            .collect();
        // Build pc -> (line, column) from the compiler's `code_map`.
        // Each entry pins a single bytecode PC to a byte range in the
        // source; the range *start* identifies the user-facing line and
        // (start - line_start_offset + 1) is the 1-based column for the
        // column-aware step encoding.
        let mut pc_to_line: HashMap<u64, u32> = HashMap::new();
        let mut pc_to_column: HashMap<u64, u32> = HashMap::new();
        for (pc_str, loc) in fn_raw.code_map {
            if let Ok(pc) = pc_str.parse::<u64>() {
                let line = byte_offset_to_line(&line_starts, loc.start);
                let column = byte_offset_to_column(&line_starts, loc.start, line);
                pc_to_line.insert(pc, line);
                pc_to_column.insert(pc, column);
            }
        }
        functions.insert(
            idx,
            FunctionDebugInfo {
                name,
                locals,
                parameters,
                pc_to_line,
                pc_to_column,
            },
        );
    }

    // Compute the per-line UTF-8 byte-length table once per module so
    // the converter can hand it to
    // `TraceWriter::register_path_with_line_lengths` (paths.dat Layout A,
    // see codetracer-trace-format-spec/trace-events.md §"paths.dat
    // per-line offset table — Layout A").
    let line_lengths = compute_line_lengths(&source_text);
    let source_path = resolve_source_path(path, &raw.from_file_path, &module_short_name);

    Some((
        module_short_name,
        ModuleDebugInfo {
            functions,
            source_path,
            line_lengths,
        },
    ))
}

/// Resolve the on-disk `.move` source path the debug-info JSON refers
/// to.  Prefers the compiler-recorded absolute path when it still exists
/// on the host; otherwise falls back to the canonical package layout
/// (`<package_root>/sources/<Module>.move`).  Returns `None` when
/// neither candidate is readable — the converter then skips column
/// resolution for that module and emits steps with `column=None`.
fn resolve_source_path(
    debug_json: &Path,
    from_file_path: &Path,
    module_short_name: &str,
) -> Option<PathBuf> {
    if from_file_path.exists() {
        return Some(from_file_path.to_path_buf());
    }
    let package_root = debug_json
        .parent() // debug_info/
        .and_then(Path::parent) // <PackageName>/
        .and_then(Path::parent) // build/
        .and_then(Path::parent)?; // <package_root>
    let candidate = package_root
        .join("sources")
        .join(format!("{module_short_name}.move"));
    candidate.exists().then_some(candidate)
}

/// Compute the per-line UTF-8 byte-length table required by the
/// `paths.dat` Layout A record (column-aware mode).
///
/// `line_lengths[i]` is the byte count of source line `i+1` (1-based,
/// matching the CTFS spec), excluding the trailing `\n`.  An `\r\n`
/// terminator contributes its `\r` to the line's byte count so the
/// table is consistent with the column byte offsets the Move compiler's
/// `code_map` emits.  A file that doesn't end with `\n` still has its
/// final line counted.  Mirrors the EVM/Solana recorder helper.
pub fn compute_line_lengths(source: &str) -> Vec<u32> {
    let mut lengths: Vec<u32> = Vec::new();
    let mut line_start: usize = 0;
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            lengths.push((i - line_start) as u32);
            line_start = i + 1;
        }
    }
    if line_start < source.len() {
        lengths.push((source.len() - line_start) as u32);
    }
    lengths
}

/// Build a sorted list of byte offsets where each source line begins.
/// `line_starts[0] == 0` and `line_starts[i]` is the byte offset of the
/// `i+1`-th line (1-indexed for callers).  Uses byte semantics so it
/// matches the Move compiler's `start` offsets verbatim — both are raw
/// byte indices into the source's UTF-8 representation.
fn build_line_starts(source: &str) -> Vec<u32> {
    let mut starts: Vec<u32> = Vec::with_capacity(source.len() / 32 + 1);
    starts.push(0);
    for (idx, ch) in source.bytes().enumerate() {
        if ch == b'\n' {
            // The next byte is the start of the next line.
            starts.push((idx + 1) as u32);
        }
    }
    starts
}

/// Resolve a byte offset into a 1-indexed line number using a precomputed
/// `line_starts` table.  The result is `1` when `offset == 0` and
/// monotonically non-decreasing in `offset`.
fn byte_offset_to_line(line_starts: &[u32], offset: u32) -> u32 {
    // partition_point returns the first index whose start is *strictly
    // greater* than `offset`; the line containing `offset` is therefore
    // `idx` (0-indexed) → `idx` as 1-indexed.
    let idx = line_starts.partition_point(|&start| start <= offset);
    idx.max(1) as u32
}

/// Resolve a byte offset into a 1-indexed column number on `line`.
/// Returns `1` (start-of-line) when `line` is out of bounds — keeping
/// the column-aware reader's wire format well-formed for the rare case
/// of a code_map entry that points outside the source we loaded.
fn byte_offset_to_column(line_starts: &[u32], offset: u32, line: u32) -> u32 {
    let idx = line.saturating_sub(1) as usize;
    let line_start = line_starts.get(idx).copied().unwrap_or(0);
    (offset.saturating_sub(line_start) + 1).max(1)
}

impl FunctionDebugInfo {
    /// Look up the 1-indexed source line for a bytecode PC, if the
    /// compiler recorded one.  Returns `None` when no mapping exists
    /// (typically a synthesised entry/exit op).
    pub fn pc_to_line(&self, pc: u64) -> Option<u32> {
        self.pc_to_line.get(&pc).copied()
    }

    /// Look up the 1-indexed source column for a bytecode PC.  Returns
    /// `None` for synthesised ops (same gating as `pc_to_line`); the
    /// converter forwards that `None` straight into
    /// `register_step_with_column` so the column-aware reader records a
    /// line-only step (DeltaLine without a DeltaColumn override).
    pub fn pc_to_column(&self, pc: u64) -> Option<u32> {
        self.pc_to_column.get(&pc).copied()
    }
}

impl DebugInfo {
    /// Iterate `(module_short_name, binary_member_index, pc, line)`
    /// for every PC mapping the compiler recorded.  Used to seed a
    /// `SourceMapResolver` directly from the loaded debug info,
    /// without re-parsing the build/ tree.
    pub fn iter_pc_lines(&self) -> impl Iterator<Item = (&str, u64, u64, u32)> + '_ {
        self.modules.iter().flat_map(|(module_name, mod_info)| {
            mod_info.functions.iter().flat_map(move |(idx, fn_info)| {
                let module_name = module_name.as_str();
                let idx = *idx;
                fn_info
                    .pc_to_line
                    .iter()
                    .map(move |(pc, line)| (module_name, idx, *pc, *line))
            })
        })
    }

    /// Iterate `(source_path, line_lengths)` pairs for every module
    /// whose debug-info JSON pointed at a readable on-disk source file.
    /// The converter consumes this to call
    /// `TraceWriter::register_path_with_line_lengths` once per source
    /// path before emitting the first column-aware step.
    pub fn iter_source_line_lengths(&self) -> impl Iterator<Item = (&Path, &[u32])> + '_ {
        self.modules.values().filter_map(|m| {
            let path = m.source_path.as_deref()?;
            Some((path, m.line_lengths.as_slice()))
        })
    }
}

/// Strip the compiler-internal `#scope#unique` suffix from a local /
/// parameter name.  The Move compiler appends `#<scope>#<unique>` to
/// every source-level binding (e.g. `accumulator#1#0`,
/// `counter#1#0`); the suffix is meaningful only inside the bytecode
/// register allocator.  Source-level identifiers always lack a `#`
/// (which is not a valid Move identifier character) so any name
/// starting with `%` (compiler-generated temp like `%#1`) is passed
/// through unchanged — those have no source-level counterpart and
/// stripping `%` would produce ambiguous `1`/`2`/... names.
fn strip_scope_suffix(raw: &str) -> String {
    if raw.starts_with('%') {
        return raw.to_string();
    }
    match raw.find('#') {
        Some(idx) => raw[..idx].to_string(),
        None => raw.to_string(),
    }
}

/// Read the byte range `[start, end)` from `text` as a UTF-8 substring.
/// Returns `""` if the range is out of bounds — the caller treats an
/// empty name as "skip this entry" rather than panicking on a
/// hand-edited debug-info file.
fn slice_or_empty(text: &str, loc: &Location) -> String {
    let bytes = text.as_bytes();
    let start = loc.start as usize;
    let end = loc.end as usize;
    if start <= end && end <= bytes.len() {
        std::str::from_utf8(&bytes[start..end])
            .unwrap_or("")
            .to_string()
    } else {
        String::new()
    }
}

// ---------------------------------------------------------------------
// Raw deserialization shapes mirroring Move's debug-info JSON layout.
// ---------------------------------------------------------------------

#[derive(Deserialize)]
struct RawDebugInfo {
    from_file_path: PathBuf,
    /// `[address, short_name]` — we use only the short name.
    module_name: Vec<String>,
    function_map: HashMap<String, RawFunction>,
}

#[derive(Deserialize)]
struct RawFunction {
    definition_location: Location,
    #[serde(default)]
    parameters: Vec<(String, Location)>,
    #[serde(default)]
    locals: Vec<(String, Location)>,
    /// `code_map[pc] = {file_hash, start, end}` — byte ranges in the
    /// source that the compiler associates with each bytecode PC.  Used
    /// here to recover per-PC source lines.  The PC keys arrive as
    /// JSON strings ("0", "1", ...) so we deserialise them as strings
    /// and parse to u64 later.
    #[serde(default)]
    code_map: HashMap<String, Location>,
}

#[derive(Deserialize)]
struct Location {
    start: u32,
    end: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_scope_suffix_preserves_compiler_temps() {
        // Compiler-generated temps lack a source-level counterpart and
        // must surface verbatim so they're visually distinct from
        // user bindings.
        assert_eq!(strip_scope_suffix("%#1"), "%#1");
        assert_eq!(strip_scope_suffix("%#42"), "%#42");
    }

    #[test]
    fn strip_scope_suffix_drops_scope_unique_for_user_bindings() {
        assert_eq!(strip_scope_suffix("accumulator#1#0"), "accumulator");
        assert_eq!(strip_scope_suffix("counter#1#0"), "counter");
        assert_eq!(strip_scope_suffix("len#1#0"), "len");
        assert_eq!(strip_scope_suffix("y#1#0"), "y");
    }

    #[test]
    fn strip_scope_suffix_passes_through_clean_names() {
        // No `#` anywhere → passes through unchanged.
        assert_eq!(strip_scope_suffix("plain_name"), "plain_name");
    }
}
