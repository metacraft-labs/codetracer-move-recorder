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
//!     (slot `parameters.len()` == `locals[0]`, ...: the VM numbers
//!     parameters first; see `FunctionDebugInfo::slot_name`).
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
    /// The file an instruction is positioned in, for the instructions
    /// that are NOT positioned in the module's own source file: code the
    /// compiler inlined from another module keeps the position of the
    /// text it was written in.  `pc_to_line` / `pc_to_column` are lines
    /// and columns of THIS file for these pcs.  Empty for every kind of
    /// debug info except an Aptos published-source map.
    pub pc_to_path: HashMap<u64, PathBuf>,
    /// 1-indexed source line of the function's name, i.e. where the
    /// function is defined.  Registered as the function's location so
    /// a call-trace entry opens at the definition rather than at line 1.
    pub definition_line: u32,
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
    /// Every OTHER file an instruction of this module is positioned in
    /// (see [`FunctionDebugInfo::pc_to_path`]), with its per-line byte
    /// lengths, so the converter registers it like the module's own.
    pub other_sources: Vec<(PathBuf, Vec<u32>)>,
}

/// Workspace of debug-info modules.
///
/// Two keyings coexist because the two producers of debug info differ:
///
///   * a local `sui move build` tree describes ONE package, so modules are
///     keyed by short name (e.g. `flow_test`) and looked up by the
///     trace's `frame.module.name`;
///   * a `sui replay` output directory describes EVERY package an
///     on-chain transaction entered, where short names collide (`coin`
///     exists in `0x2` and in many third-party packages), so modules are
///     keyed by `(package storage id, module name)`.  The storage id is
///     the trace frame's `version_id`, which names the exact package
///     version that executed — an upgraded package resolves to the
///     version that ran, not to its latest publication.
#[derive(Debug, Default, Clone)]
pub struct DebugInfo {
    modules: HashMap<String, ModuleDebugInfo>,
    by_package: HashMap<(String, String), ModuleDebugInfo>,
}

/// Why a module in a `sui replay` output directory was not loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayModuleRejection {
    /// The debug-info JSON or its disassembly could not be read or parsed.
    Unreadable { json: PathBuf, reason: String },
    /// The disassembly's SHA-256 differs from the hash its debug info
    /// names, so the byte ranges would point into different text.
    HashMismatch {
        json: PathBuf,
        expected: String,
        actual: String,
    },
    /// An Aptos disassembly (`.mv.masm`) could not be read, does not name
    /// the module its location says it holds, or its pc markers disagree
    /// with the instructions counted, so its lines cannot be trusted.
    Disassembly { path: PathBuf, reason: String },
    /// An Aptos published-source map (`source/<module>.json`) could not be
    /// read, or is not bound to the module bytes and the source texts
    /// beside it, so the module stays on its disassembly.
    SourceMap { path: PathBuf, reason: String },
}

/// The format tag of an Aptos published-source map, the document a
/// source provider writes beside a module whose published source it
/// proved: `blocktracer`'s `tools/chain/lib/aptos-source.mjs` compiles the
/// package source an account published on chain and keeps the compiler's
/// source map only when the module bytes it compiled equal the chain's.
pub const APTOS_SOURCE_MAP_FORMAT: &str = "codetracer/aptos-source-map@1";

/// Normalise a Move address to 64 lowercase hex digits without `0x`, the
/// form `sui replay` uses for directory names and trace frames alike.
pub fn normalize_address(address: &str) -> String {
    let hex = address
        .strip_prefix("0x")
        .or_else(|| address.strip_prefix("0X"))
        .unwrap_or(address)
        .to_ascii_lowercase();
    format!("{hex:0>64}")
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

    /// Load the debug info of every package a `sui replay` run fetched.
    ///
    /// `sui replay --trace` writes, next to `trace.json.zst`, one
    /// directory per package storage id (`0x…`), each holding
    /// `bytecode/<module>.mvb` (the module's disassembly) and
    /// `bytecode/<module>.json` (debug info whose byte ranges point into
    /// that disassembly).  On-chain packages carry no source, so the
    /// disassembly is the source text: steps resolve to disassembly
    /// lines, and locals keep the names the bytecode carries.  Each
    /// module's path is recorded RELATIVE to `replay_dir`
    /// (`0x<package>/bytecode/<module>.mvb`).
    ///
    /// A module is loaded only if the disassembly's SHA-256 equals the
    /// `file_hash` its debug info names; otherwise its byte ranges would
    /// attribute steps to the wrong text.  Rejected modules are returned
    /// rather than dropped silently, so the caller can report them.
    pub fn from_sui_replay_dir(
        replay_dir: &Path,
    ) -> std::io::Result<(Self, Vec<ReplayModuleRejection>)> {
        let mut store = Self::empty();
        let mut rejected = Vec::new();
        for pkg in fs::read_dir(replay_dir)? {
            let pkg = pkg?;
            let pkg_name = pkg.file_name().to_string_lossy().into_owned();
            let bytecode_dir = pkg.path().join("bytecode");
            if !pkg_name.starts_with("0x") || !bytecode_dir.is_dir() {
                continue;
            }
            let package = normalize_address(&pkg_name);
            for entry in fs::read_dir(&bytecode_dir)? {
                let json = entry?.path();
                if json.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                // Recorded relative to the replay directory, so the bundle
                // names no host path and a consumer can ship the
                // disassembly beside it at the same relative location.
                let relative = Path::new(&pkg_name)
                    .join("bytecode")
                    .join(json.with_extension("mvb").file_name().unwrap_or_default());
                match parse_replay_module(&json, relative) {
                    Ok((module, info)) => {
                        store.by_package.insert((package.clone(), module), info);
                    }
                    Err(rejection) => rejected.push(rejection),
                }
            }
        }
        Ok((store, rejected))
    }

    /// Load the disassembly of every module an Aptos replay entered.
    ///
    /// The layout mirrors a `sui replay` directory: one directory per
    /// module address (`0x…`, 64 hex digits), each holding
    /// `bytecode/<module>.mv` (the module's bytecode as it was on chain
    /// when the transaction ran) and `bytecode/<module>.mv.masm` (what
    /// `aptos move disassemble` prints for it).  On-chain Aptos modules
    /// carry no source map, so the disassembly is the source text and pcs
    /// map to its instruction lines; see [`parse_aptos_disassembly`].
    /// Paths are recorded relative to `replay_dir`, as for Sui.
    ///
    /// The bytecode carries no local or parameter names, so frames keep
    /// the converter's `arg<N>` / `local_<N>` names.
    ///
    /// A module whose published source was proved against these exact
    /// bytes has, beside them, `source/<module>.move` (the published text)
    /// and `source/<module>.json` (the compiler's source map for those
    /// bytes, [`APTOS_SOURCE_MAP_FORMAT`]).  Such a module is positioned on
    /// its source and its locals take their source names — but only when
    /// the map names the SHA-256 of `bytecode/<module>.mv` and of every
    /// text it positions an instruction in, so a map can never be applied
    /// to bytes or text it was not made for.  A map that does not bind is
    /// returned as a rejection and the module stays on its disassembly.
    pub fn from_aptos_replay_dir(
        replay_dir: &Path,
    ) -> std::io::Result<(Self, Vec<ReplayModuleRejection>)> {
        let mut store = Self::empty();
        let mut rejected = Vec::new();
        let mut packages: Vec<_> = fs::read_dir(replay_dir)?.collect::<Result<_, _>>()?;
        packages.sort_by_key(|e| e.file_name());
        for pkg in packages {
            let pkg_name = pkg.file_name().to_string_lossy().into_owned();
            let bytecode_dir = pkg.path().join("bytecode");
            if !pkg_name.starts_with("0x") || !bytecode_dir.is_dir() {
                continue;
            }
            let package = normalize_address(&pkg_name);
            for entry in fs::read_dir(&bytecode_dir)? {
                let path = entry?.path();
                let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let Some(module) = file_name.strip_suffix(".mv.masm") else {
                    continue;
                };
                let source_map = pkg.path().join("source").join(format!("{module}.json"));
                if source_map.is_file() {
                    match parse_aptos_source_module(replay_dir, &pkg_name, &package, module) {
                        Ok(info) => {
                            store
                                .by_package
                                .insert((package.clone(), module.to_string()), info);
                            continue;
                        }
                        Err(reason) => rejected.push(ReplayModuleRejection::SourceMap {
                            path: source_map,
                            reason,
                        }),
                    }
                }
                let reject = |reason: String| ReplayModuleRejection::Disassembly {
                    path: path.clone(),
                    reason,
                };
                let text = match fs::read_to_string(&path) {
                    Ok(text) => text,
                    Err(e) => {
                        rejected.push(reject(e.to_string()));
                        continue;
                    }
                };
                match parse_aptos_disassembly(&text) {
                    Ok(parsed)
                        if normalize_address(&parsed.address) == package
                            && parsed.module == module =>
                    {
                        let relative = Path::new(&pkg_name).join("bytecode").join(file_name);
                        store.by_package.insert(
                            (package.clone(), parsed.module),
                            ModuleDebugInfo {
                                functions: parsed.functions,
                                source_path: Some(relative),
                                line_lengths: compute_line_lengths(&text),
                                other_sources: Vec::new(),
                            },
                        );
                    }
                    Ok(parsed) => rejected.push(reject(format!(
                        "disassembles module {}::{}, not 0x{package}::{module}",
                        parsed.address, parsed.module
                    ))),
                    Err(reason) => rejected.push(reject(reason)),
                }
            }
        }
        Ok((store, rejected))
    }

    /// Look up a module, preferring the package-qualified entry (loaded
    /// from a `sui replay` directory) and falling back to the short-name
    /// entry (loaded from a local build).  `package` may be empty.
    pub fn module(&self, package: &str, module_name: &str) -> Option<&ModuleDebugInfo> {
        if !package.is_empty()
            && let Some(m) = self
                .by_package
                .get(&(normalize_address(package), module_name.to_string()))
        {
            return Some(m);
        }
        self.modules.get(module_name)
    }

    /// Look up the per-function debug info for `(package, module_name,
    /// binary_member_index)`.  See [`DebugInfo::module`] for the lookup
    /// order.
    pub fn function_in(
        &self,
        package: &str,
        module_name: &str,
        binary_member_index: u64,
    ) -> Option<&FunctionDebugInfo> {
        self.module(package, module_name)
            .and_then(|m| m.functions.get(&binary_member_index))
    }

    /// Look up the per-function debug info for `(module_name,
    /// binary_member_index)`.  Returns `None` when either the module
    /// or the function is unknown.
    pub fn function(
        &self,
        module_name: &str,
        binary_member_index: u64,
    ) -> Option<&FunctionDebugInfo> {
        self.function_in("", module_name, binary_member_index)
    }

    /// The source file a module's lines refer to, when it is known.
    pub fn module_source_path(&self, package: &str, module_name: &str) -> Option<&Path> {
        self.module(package, module_name)
            .and_then(|m| m.source_path.as_deref())
    }

    /// Number of modules loaded, under either keying.
    pub fn module_count(&self) -> usize {
        self.modules.len() + self.by_package.len()
    }
}

/// Parse one `bytecode/<module>.json` from a `sui replay` directory
/// against its sibling `<module>.mvb` disassembly.
fn parse_replay_module(
    json: &Path,
    recorded_path: PathBuf,
) -> Result<(String, ModuleDebugInfo), ReplayModuleRejection> {
    use sha2::{Digest, Sha256};

    let unreadable = |reason: String| ReplayModuleRejection::Unreadable {
        json: json.to_path_buf(),
        reason,
    };
    let bytes = fs::read(json).map_err(|e| unreadable(e.to_string()))?;
    let raw: RawDebugInfo =
        serde_json::from_slice(&bytes).map_err(|e| unreadable(e.to_string()))?;
    let mvb = json.with_extension("mvb");
    let source_bytes = fs::read(&mvb).map_err(|e| unreadable(format!("{}: {e}", mvb.display())))?;

    let expected = raw
        .definition_location
        .as_ref()
        .and_then(|loc| loc.file_hash.clone())
        .ok_or_else(|| unreadable("debug info names no file_hash".to_string()))?;
    let actual: String = Sha256::digest(&source_bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err(ReplayModuleRejection::HashMismatch {
            json: json.to_path_buf(),
            expected,
            actual,
        });
    }

    let source_text = String::from_utf8(source_bytes)
        .map_err(|e| unreadable(format!("{}: {e}", mvb.display())))?;
    build_module(raw, &source_text, Some(recorded_path))
        .ok_or_else(|| unreadable("debug info names no module".to_string()))
}

/// A module's disassembly as [`parse_aptos_disassembly`] reads it.
#[derive(Debug)]
pub struct AptosDisassembly {
    /// The module address as the disassembly prints it (`0x1`).
    pub address: String,
    pub module: String,
    /// Per function definition index.
    pub functions: HashMap<u64, FunctionDebugInfo>,
}

/// Map each instruction of an Aptos module disassembly to its line.
///
/// `aptos move disassemble` (Aptos CLI 7.x and the 1.49 node line) prints
/// a module as `.masm` text:
///
/// ```text
/// module 0x1::coin
/// ...
/// // Function definition at index 0
/// #[persistent] public fun extract<T0>(l0: &mut Coin<T0>, l1: u64): Coin<T0>
///     local l2: u64
///     copy_loc l0
///     borrow_field Coin<T0>, value
///     ...
///     // @5
///     br_false l0
///     ...
/// l0: move_loc l0
///     pop
/// ```
///
/// The pc is not printed on each line; instead every instruction is one
/// line, in pc order, after the function header and its `local`
/// declarations (a branch target carries its label, `l0: `, in front of
/// the instruction), and a `// @<pc>` comment precedes every fifth
/// instruction.  So pcs are counted, and each `// @<pc>` marker is
/// checked against the count: a disassembler whose output does not follow
/// this shape is refused rather than mapped to the wrong lines.
///
/// A function's definition line is its header line; a native function
/// has no instructions.  Columns point at the instruction mnemonic.
pub fn parse_aptos_disassembly(text: &str) -> Result<AptosDisassembly, String> {
    let mut module_id: Option<(String, String)> = None;
    let mut functions: HashMap<u64, FunctionDebugInfo> = HashMap::new();

    // The function being read: its index, its info, the next pc, and
    // whether its header has been seen.
    struct Current {
        index: u64,
        info: FunctionDebugInfo,
        next_pc: u64,
        header_seen: bool,
        marker: Option<u64>,
    }
    let mut current: Option<Current> = None;
    let finish = |current: Option<Current>,
                  functions: &mut HashMap<u64, FunctionDebugInfo>|
     -> Result<(), String> {
        if let Some(c) = current {
            if !c.header_seen {
                return Err(format!("function definition {} has no header", c.index));
            }
            if let Some(pc) = c.marker {
                return Err(format!(
                    "marker // @{pc} in function {} is followed by no instruction",
                    c.info.name
                ));
            }
            functions.insert(c.index, c.info);
        }
        Ok(())
    };

    for (line_idx, raw) in text.lines().enumerate() {
        let line_no = line_idx as u32 + 1;
        let trimmed = raw.trim();

        if module_id.is_none()
            && let Some(rest) = trimmed.strip_prefix("module ")
        {
            let (address, name) = rest
                .trim()
                .rsplit_once("::")
                .ok_or_else(|| format!("line {line_no}: module header {trimmed:?}"))?;
            module_id = Some((address.to_string(), name.to_string()));
            continue;
        }

        if let Some(index) = trimmed.strip_prefix("// Function definition at index ") {
            finish(current.take(), &mut functions)?;
            let index = index
                .trim()
                .parse::<u64>()
                .map_err(|e| format!("line {line_no}: function index: {e}"))?;
            current = Some(Current {
                index,
                info: FunctionDebugInfo {
                    name: String::new(),
                    locals: Vec::new(),
                    parameters: Vec::new(),
                    pc_to_line: HashMap::new(),
                    pc_to_column: HashMap::new(),
                    pc_to_path: HashMap::new(),
                    definition_line: line_no,
                },
                next_pc: 0,
                header_seen: false,
                marker: None,
            });
            continue;
        }

        let Some(cur) = current.as_mut() else {
            continue;
        };
        if trimmed.is_empty() {
            // A blank line ends a function body.
            if cur.header_seen {
                finish(current.take(), &mut functions)?;
            }
            continue;
        }
        if !cur.header_seen {
            // The header is the first line after the marker comment that
            // declares the function (attributes precede `fun` on it).
            if let Some(name) = function_name_in_header(trimmed) {
                cur.info.name = name;
                cur.info.definition_line = line_no;
                cur.header_seen = true;
            }
            continue;
        }
        if let Some(comment) = trimmed.strip_prefix("//") {
            if let Some(pc) = comment.trim().strip_prefix('@') {
                let pc = pc
                    .trim()
                    .parse::<u64>()
                    .map_err(|e| format!("line {line_no}: pc marker: {e}"))?;
                cur.marker = Some(pc);
            }
            continue;
        }
        if trimmed.starts_with("local ") {
            continue;
        }
        // An instruction, possibly behind a branch label.
        let instruction = strip_label(trimmed);
        if let Some(expected) = cur.marker.take()
            && expected != cur.next_pc
        {
            return Err(format!(
                "line {line_no}: marker // @{expected} precedes instruction {} of function {}",
                cur.next_pc, cur.info.name
            ));
        }
        // `instruction` is a suffix slice of `raw`'s trimmed text.
        let column = (instruction.as_ptr() as usize - raw.as_ptr() as usize) as u32 + 1;
        cur.info.pc_to_line.insert(cur.next_pc, line_no);
        cur.info.pc_to_column.insert(cur.next_pc, column);
        cur.next_pc += 1;
    }
    finish(current.take(), &mut functions)?;

    let (address, module) =
        module_id.ok_or_else(|| "no `module <address>::<name>` header".to_string())?;
    Ok(AptosDisassembly {
        address,
        module,
        functions,
    })
}

/// The function name a `.masm` function header declares
/// (`#[persistent] public fun extract<T0>(...)` → `extract`), or `None`
/// when the line declares no function.
fn function_name_in_header(line: &str) -> Option<String> {
    let after = line
        .strip_prefix("fun ")
        .or_else(|| line.split_once(" fun ").map(|(_, rest)| rest))?;
    let name: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// The instruction on a `.masm` body line without its branch label
/// (`l0: move_loc l0` → `move_loc l0`).
fn strip_label(line: &str) -> &str {
    if let Some((label, rest)) = line.split_once(": ")
        && !label.is_empty()
        && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return rest.trim_start();
    }
    line
}

/// Parse a single `<Module>.json` debug-info file.  Returns
/// `(module_short_name, ModuleDebugInfo)` on success, `None` if the
/// file is malformed or missing required fields.
fn parse_module_debug_json(path: &Path) -> Option<(String, ModuleDebugInfo)> {
    let bytes = fs::read(path).ok()?;
    let raw: RawDebugInfo = serde_json::from_slice(&bytes).ok()?;

    let module_short_name = raw.module_name.get(1).cloned()?;
    let from_file_path = raw.from_file_path.clone().unwrap_or_default();

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
    let source_text = fs::read_to_string(&from_file_path).ok().or_else(|| {
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

    let source_path = resolve_source_path(path, &from_file_path, &module_short_name);
    build_module(raw, &source_text, source_path)
}

/// Build a module's debug info from its parsed JSON and the text its byte
/// ranges index into.  Returns `(module_short_name, ModuleDebugInfo)`, or
/// `None` when the JSON names no module or a function index is malformed.
fn build_module(
    raw: RawDebugInfo,
    source_text: &str,
    source_path: Option<PathBuf>,
) -> Option<(String, ModuleDebugInfo)> {
    let module_short_name = raw.module_name.get(1).cloned()?;

    // Precompute a byte-offset -> 1-indexed-line lookup for the source
    // text so we can resolve every `code_map` entry without re-scanning
    // the file per PC.  Storing one `Vec<u32>` of line offsets keeps
    // the resolver hot in cache and turns the lookup into a single
    // binary search per PC.
    let line_starts = build_line_starts(source_text);

    let mut functions = HashMap::new();
    for (idx_str, fn_raw) in raw.function_map {
        let idx: u64 = idx_str.parse().ok()?;
        let name = slice_or_empty(source_text, &fn_raw.definition_location);
        let definition_line = byte_offset_to_line(&line_starts, fn_raw.definition_location.start);
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
                pc_to_path: HashMap::new(),
                definition_line,
            },
        );
    }

    // Compute the per-line UTF-8 byte-length table once per module so
    // the converter can hand it to
    // `TraceWriter::register_path_with_line_lengths` (paths.dat Layout A,
    // see codetracer-trace-format-spec/trace-events.md §"paths.dat
    // per-line offset table — Layout A").
    let line_lengths = compute_line_lengths(source_text);

    Some((
        module_short_name,
        ModuleDebugInfo {
            functions,
            source_path,
            line_lengths,
            other_sources: Vec::new(),
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
    /// Name of frame slot `slot`.  The Move VM numbers a frame's slots
    /// parameters first, then locals, while the debug info lists the two
    /// separately — so slot `i` is `parameters[i]` below the parameter
    /// count and `locals[i - parameters.len()]` above it.
    pub fn slot_name(&self, slot: u64) -> Option<&str> {
        let slot = usize::try_from(slot).ok()?;
        match slot.checked_sub(self.parameters.len()) {
            None => self.parameters.get(slot),
            Some(local) => self.locals.get(local),
        }
        .map(String::as_str)
    }

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

    /// The file a pc is positioned in when it is not the module's own
    /// source (code inlined from another module); `None` means the
    /// module's own file.
    pub fn pc_path(&self, pc: u64) -> Option<&Path> {
        self.pc_to_path.get(&pc).map(PathBuf::as_path)
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
    ///
    /// Yielded in path order, so path ids — and with them the bundle's
    /// bytes — do not depend on hash-map iteration order.
    pub fn iter_source_line_lengths(&self) -> impl Iterator<Item = (&Path, &[u32])> + '_ {
        let mut paths: Vec<(&Path, &[u32])> = self
            .modules
            .values()
            .chain(self.by_package.values())
            .flat_map(|m| {
                m.source_path
                    .as_deref()
                    .map(|p| (p, m.line_lengths.as_slice()))
                    .into_iter()
                    .chain(
                        m.other_sources
                            .iter()
                            .map(|(p, l)| (p.as_path(), l.as_slice())),
                    )
            })
            .collect();
        paths.sort_by(|a, b| a.0.cmp(b.0));
        paths.into_iter()
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
/// Load an Aptos published-source map for `0x<package>::<module>` from
/// `<replay_dir>/<pkg_name>/source/<module>.json`, refusing one that is
/// not bound to the module bytes and to every text it positions an
/// instruction in.  See [`DebugInfo::from_aptos_replay_dir`].
fn parse_aptos_source_module(
    replay_dir: &Path,
    pkg_name: &str,
    package: &str,
    module: &str,
) -> Result<ModuleDebugInfo, String> {
    use sha2::{Digest, Sha256};
    use std::path::Component;

    let sha256 = |bytes: &[u8]| -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    let json = replay_dir
        .join(pkg_name)
        .join("source")
        .join(format!("{module}.json"));
    let bytes = fs::read(&json).map_err(|e| e.to_string())?;
    let doc: RawAptosSourceMap = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if doc.format != APTOS_SOURCE_MAP_FORMAT {
        return Err(format!(
            "format '{}', not '{APTOS_SOURCE_MAP_FORMAT}'",
            doc.format
        ));
    }
    match doc.module_name.as_slice() {
        [address, name] if normalize_address(address) == package && name == module => {}
        other => return Err(format!("maps module {other:?}, not 0x{package}::{module}")),
    }
    let mv = replay_dir
        .join(pkg_name)
        .join("bytecode")
        .join(format!("{module}.mv"));
    let code = fs::read(&mv).map_err(|e| format!("{}: {e}", mv.display()))?;
    let actual = sha256(&code);
    if !actual.eq_ignore_ascii_case(&doc.bytecode_sha256) {
        return Err(format!(
            "is the map of bytecode {}, and {} is {actual}",
            doc.bytecode_sha256,
            mv.display()
        ));
    }

    struct Text {
        path: PathBuf,
        text: String,
        line_starts: Vec<u32>,
    }
    let mut texts: HashMap<String, Text> = HashMap::new();
    for (hash, relative) in &doc.source_files {
        let path = Path::new(relative);
        if !path.components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(format!(
                "names {relative}, which is not inside the replay directory"
            ));
        }
        let content = fs::read(replay_dir.join(path)).map_err(|e| format!("{relative}: {e}"))?;
        let actual = sha256(&content);
        if !actual.eq_ignore_ascii_case(hash) {
            return Err(format!("names {relative} as {hash}, and it is {actual}"));
        }
        let text = String::from_utf8(content).map_err(|e| format!("{relative}: {e}"))?;
        texts.insert(
            hash.to_ascii_lowercase(),
            Text {
                path: path.to_path_buf(),
                line_starts: build_line_starts(&text),
                text,
            },
        );
    }
    let text_of = |loc: &Location| -> Result<&Text, String> {
        loc.file_hash
            .as_deref()
            .and_then(|h| texts.get(&h.to_ascii_lowercase()))
            .ok_or_else(|| {
                format!(
                    "positions code in {:?}, which it lists no text for",
                    loc.file_hash
                )
            })
    };
    let own = text_of(&doc.definition_location)?;
    let own_path = Path::new(pkg_name)
        .join("source")
        .join(format!("{module}.move"));
    if own.path != own_path {
        return Err(format!(
            "defines the module in {}, not {}",
            own.path.display(),
            own_path.display()
        ));
    }

    let mut functions = HashMap::new();
    for (index, raw) in &doc.function_map {
        let index: u64 = index
            .parse()
            .map_err(|e| format!("function index '{index}': {e}"))?;
        let defined_in = text_of(&raw.definition_location)?;
        let name = slice_or_empty(&defined_in.text, &raw.definition_location);
        if name.is_empty() {
            continue;
        }
        let mut pc_to_line = HashMap::new();
        let mut pc_to_column = HashMap::new();
        let mut pc_to_path = HashMap::new();
        for (pc, loc) in &raw.code_map {
            let pc: u64 = pc.parse().map_err(|e| format!("pc '{pc}': {e}"))?;
            let t = text_of(loc)?;
            let line = byte_offset_to_line(&t.line_starts, loc.start);
            pc_to_line.insert(pc, line);
            pc_to_column.insert(pc, byte_offset_to_column(&t.line_starts, loc.start, line));
            if t.path != own.path {
                pc_to_path.insert(pc, t.path.clone());
            }
        }
        functions.insert(
            index,
            FunctionDebugInfo {
                name,
                locals: raw
                    .locals
                    .iter()
                    .map(|(n, _)| strip_scope_suffix(n))
                    .collect(),
                parameters: raw
                    .parameters
                    .iter()
                    .map(|(n, _)| strip_scope_suffix(n))
                    .collect(),
                pc_to_line,
                pc_to_column,
                pc_to_path,
                definition_line: byte_offset_to_line(
                    &defined_in.line_starts,
                    raw.definition_location.start,
                ),
            },
        );
    }
    let mut other_sources: Vec<(PathBuf, Vec<u32>)> = texts
        .values()
        .filter(|t| t.path != own.path)
        .map(|t| (t.path.clone(), compute_line_lengths(&t.text)))
        .collect();
    other_sources.sort();
    Ok(ModuleDebugInfo {
        functions,
        source_path: Some(own.path.clone()),
        line_lengths: compute_line_lengths(&own.text),
        other_sources,
    })
}

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
    /// Absent (`null`) in the debug info `sui replay` writes for on-chain
    /// packages, whose source is the sibling disassembly.
    #[serde(default)]
    from_file_path: Option<PathBuf>,
    /// The module's own location; its `file_hash` is the SHA-256 of the
    /// text every byte range in this file indexes into.
    #[serde(default)]
    definition_location: Option<Location>,
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

/// An Aptos published-source map ([`APTOS_SOURCE_MAP_FORMAT`]): the Move
/// compiler's `SourceMap` for one module, as JSON in the shape of the
/// debug info above, with every position's `file_hash` the SHA-256 of the
/// text it indexes into, and two bindings — the module bytes it is the map
/// of, and the relative path of every text it names.
#[derive(Deserialize)]
struct RawAptosSourceMap {
    format: String,
    /// `[address, short_name]`.
    module_name: Vec<String>,
    /// SHA-256 (hex) of the module bytes this map was compiled with.
    bytecode_sha256: String,
    definition_location: Location,
    /// SHA-256 (hex) of a text -> its path relative to the replay directory.
    source_files: std::collections::BTreeMap<String, String>,
    function_map: std::collections::BTreeMap<String, RawFunction>,
}

#[derive(Deserialize)]
struct Location {
    #[serde(default)]
    file_hash: Option<String>,
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
