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
}

/// Per-module debug info, keyed by the bytecode function index
/// (`binary_member_index` in the trace's `OpenFrame`).
#[derive(Debug, Default, Clone)]
pub struct ModuleDebugInfo {
    pub functions: HashMap<u64, FunctionDebugInfo>,
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

    // The `from_file_path` field gives us the absolute path to the
    // source file at compile time; we read it once so we can extract
    // function names and parameter names from `definition_location`
    // byte ranges.
    let source_text = fs::read_to_string(&raw.from_file_path).ok()?;

    let module_short_name = raw.module_name.get(1).cloned()?;

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
        functions.insert(
            idx,
            FunctionDebugInfo {
                name,
                locals,
                parameters,
            },
        );
    }

    Some((module_short_name, ModuleDebugInfo { functions }))
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
