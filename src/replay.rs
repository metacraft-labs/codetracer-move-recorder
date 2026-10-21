//! On-chain transaction replay pipeline.
//!
//! Orchestrates the process of replaying a Sui transaction and converting
//! the resulting trace into CodeTracer format.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use eyre::{Result, WrapErr, bail, eyre};

use crate::converter;
use crate::move_debug_info::{DebugInfo, ReplayModuleRejection};
use crate::move_types::TraceEvent;
use crate::source_lookup::SourceLookup;

/// Configuration for replaying a transaction.
///
/// The output format is fixed to CTFS — see
/// `Recorder-CLI-Conventions.md` §4 in `codetracer-specs`.  Use
/// `ct print` (from `codetracer-trace-format-nim`) for human-readable
/// conversion of the produced bundle.
pub struct ReplayConfig {
    /// Network to replay against: `mainnet`, `testnet`, `devnet`, or a
    /// GraphQL service URL.  Passed to `sui replay --node`, which reads
    /// the transaction and the exact object versions it names from that
    /// network's GraphQL service (Sui's JSON-RPC was retired on public
    /// fullnodes in 2026).
    pub node: String,
    /// Transaction digest to replay.
    pub digest: String,
    /// Where `sui replay` writes its artifacts, kept after the run.  If it
    /// already holds `<digest>/trace.json.zst`, the transaction is not
    /// replayed again and the existing artifacts are converted.  `None`
    /// replays into a scratch directory that is removed afterwards.
    pub replay_dir: Option<PathBuf>,
    /// Refuse (by name, `TRACE-TOO-LARGE`) a trace that decompresses to more
    /// than this many bytes, before reading it into memory.
    pub max_trace_bytes: u64,
    /// Directory containing Move source files (optional).
    pub source_dir: Option<PathBuf>,
    /// Output directory for CodeTracer trace files.
    pub out_dir: PathBuf,
}

impl ReplayConfig {
    /// Create a config with sensible defaults.
    pub fn new(digest: String) -> Self {
        Self {
            node: "mainnet".to_string(),
            digest,
            replay_dir: None,
            max_trace_bytes: DEFAULT_MAX_TRACE_BYTES,
            source_dir: None,
            out_dir: PathBuf::from("./ct-traces/"),
        }
    }
}

/// Replay a transaction and produce a CodeTracer trace.
///
/// Steps:
/// 1. Run `sui replay --trace --digest <DIGEST>` to produce a trace file
/// 2. Locate the trace file in the replay output directory
/// 3. Locate source code via `SourceLookup`
/// 4. Decompress and convert using `convert_trace()`
pub fn replay_transaction(config: &ReplayConfig) -> Result<()> {
    // Step 1: Run sui replay, unless the kept replay directory already
    // holds this transaction.  A scratch directory is removed when
    // `_scratch` drops, after the trace has been converted.
    let (base, _scratch) = match &config.replay_dir {
        Some(dir) => (dir.clone(), None),
        None => {
            let scratch = tempfile::Builder::new()
                .prefix(&format!("sui-replay-{}-", config.digest))
                .tempdir()
                .wrap_err("Failed to create replay output directory")?;
            (scratch.path().to_path_buf(), Some(scratch))
        }
    };
    let tx_dir = base.join(&config.digest);
    if !tx_dir.join("trace.json.zst").is_file() {
        run_sui_replay(&config.node, &config.digest, &base)?;
    }

    // Step 2: With explicit Move sources, convert against them (the
    // local-development workflow); otherwise convert against the
    // disassembly and debug info `sui replay` wrote for every package.
    match &config.source_dir {
        Some(dir) => {
            let trace_file = find_trace_file(&tx_dir)?;
            replay_from_existing_trace(&trace_file, std::slice::from_ref(dir), &config.out_dir)
        }
        None => convert_sui_replay_dir_bounded(&tx_dir, &config.out_dir, config.max_trace_bytes)
            .map(|_| ()),
    }
}

/// What converting a `sui replay` directory loaded and refused.
#[derive(Debug)]
pub struct ReplayConversion {
    /// Modules whose debug info was loaded and validated.
    pub modules_loaded: usize,
    /// Modules whose debug info was refused, with the reason.
    pub modules_rejected: Vec<ReplayModuleRejection>,
}

/// Convert the artifacts `sui replay --trace` wrote for one transaction
/// (`<replay_out>/<digest>/`) into a CTFS bundle in `out_dir`.
///
/// Every package the execution entered is resolved by the storage id the
/// trace records for each frame, so a step lands on the disassembly of
/// the module version that actually ran.  Refused modules are reported on
/// stderr and returned; their frames keep values and calls but have no
/// line mapping.
pub fn convert_sui_replay_dir(tx_dir: &Path, out_dir: &Path) -> Result<ReplayConversion> {
    convert_sui_replay_dir_bounded(tx_dir, out_dir, DEFAULT_MAX_TRACE_BYTES)
}

/// [`convert_sui_replay_dir`] with an explicit bound on the decompressed
/// trace size.  The converter holds the whole trace in memory, and a single
/// mainnet transaction's trace can decompress to tens of gigabytes (every
/// event carries full snapshots of the values it touches): one at checkpoint
/// 28,845,373 is 4 GB compressed and ~79 GB decompressed, and reading it drove
/// the process to 24 GB resident before it was killed.  The size is measured
/// by streaming the decompression into a counter, so the refusal itself
/// holds nothing.
pub fn convert_sui_replay_dir_bounded(
    tx_dir: &Path,
    out_dir: &Path,
    max_trace_bytes: u64,
) -> Result<ReplayConversion> {
    let trace_file = tx_dir.join("trace.json.zst");
    if let Some(size) = decompressed_size_over(&trace_file, max_trace_bytes)? {
        bail!(
            "{TRACE_TOO_LARGE}: {} decompresses to more than {max_trace_bytes} bytes \
             (counted {size} before stopping); the converter holds a trace in memory, so it \
             is refused rather than read. Raise --max-trace-bytes to convert it anyway.",
            trace_file.display()
        );
    }
    let trace_data = read_trace_file(&trace_file)?;
    if trace_event_count(&trace_data) == 0 {
        // Sui protocol versions before 38 run on execution layers without
        // trace hooks (`sui-execution/v0`..`v2`); `sui replay` re-executes
        // such a transaction successfully and writes a trace that is only
        // the version header.  Converting it would produce a bundle that
        // looks recorded and contains nothing, so it is refused by name.
        bail!(
            "{NO_TRACE_EVENTS}: {} holds no trace events (the execution layer that ran \
             this transaction emits none; on Sui mainnet, protocol versions before 38)",
            trace_file.display()
        );
    }
    let (debug_info, rejected) = DebugInfo::from_sui_replay_dir(tx_dir)
        .wrap_err_with(|| format!("Failed to read replay directory: {}", tx_dir.display()))?;
    for rejection in &rejected {
        eprintln!("[codetracer-move-recorder] debug info refused: {rejection:?}");
    }

    // The primary path names the bundle and anchors the entry step: the
    // disassembly of the first frame's module, when it was loaded.
    // A block of non-Move commands only has no first module; its primary path is then
    // the RELATIVE `transaction.mvb`, which names no file and no host directory.
    let source_path = first_frame_source(&trace_data, &debug_info)
        .unwrap_or_else(|| PathBuf::from("transaction.mvb"));

    fs::create_dir_all(out_dir)
        .wrap_err_with(|| format!("Failed to create output directory: {}", out_dir.display()))?;
    let modules_loaded = debug_info.module_count();
    // Convert into a sibling scratch directory and move the result into
    // `out_dir` only when conversion succeeded, so a failure part-way leaves
    // no container behind that a caller could mistake for a recording.
    let staging = tempfile::Builder::new()
        .prefix(".partial-")
        .tempdir_in(out_dir)
        .wrap_err("Failed to create a staging directory")?;
    converter::convert_trace_with_debug_info(
        &trace_data,
        &source_path,
        debug_info,
        staging.path(),
        converter::ConverterOptions::default(),
    )?;
    for entry in fs::read_dir(staging.path())? {
        let entry = entry?;
        fs::rename(entry.path(), out_dir.join(entry.file_name()))?;
    }
    eprintln!("Trace files written to {}", out_dir.display());
    Ok(ReplayConversion {
        modules_loaded,
        modules_rejected: rejected,
    })
}

/// Default bound on a trace's decompressed size: well above the largest
/// mainnet trace measured to convert (5.5 GB) and well below what a
/// workstation can hold alongside the parsed events.
pub const DEFAULT_MAX_TRACE_BYTES: u64 = 16 << 30;

/// Stable token that starts the error for a trace over the size bound.
pub const TRACE_TOO_LARGE: &str = "TRACE-TOO-LARGE";

/// `Some(counted)` when `trace_file` decompresses to more than `limit` bytes,
/// `None` when it fits.  Streams the decompression into a counting sink and
/// stops at `limit + 1`, so nothing is held.
fn decompressed_size_over(trace_file: &Path, limit: u64) -> Result<Option<u64>> {
    let file = fs::File::open(trace_file)
        .wrap_err_with(|| format!("Failed to open trace file: {}", trace_file.display()))?;
    let counted = if trace_file.extension().is_some_and(|ext| ext == "zst") {
        let decoder = zstd::Decoder::new(file).wrap_err("Failed to create zstd decoder")?;
        std::io::copy(&mut decoder.take(limit + 1), &mut std::io::sink())
    } else {
        std::io::copy(&mut file.take(limit + 1), &mut std::io::sink())
    }
    .wrap_err("Failed to measure the trace")?;
    Ok((counted > limit).then_some(counted))
}

/// Stable token that starts the error for a trace with no events, so a
/// caller driving the CLI can tell this refusal from a failure.
pub const NO_TRACE_EVENTS: &str = "NO-TRACE-EVENTS";

/// Number of non-empty lines after the version header.
fn trace_event_count(trace_data: &[u8]) -> usize {
    String::from_utf8_lossy(trace_data)
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .count()
}

/// The disassembly path of the module the first `OpenFrame` runs in.
fn first_frame_source(trace_data: &[u8], debug_info: &DebugInfo) -> Option<PathBuf> {
    let text = std::str::from_utf8(trace_data).ok()?;
    text.lines()
        .skip(1)
        .filter_map(|line| serde_json::from_str::<TraceEvent>(line).ok())
        .find_map(|event| match event {
            TraceEvent::OpenFrame { frame, .. } => Some(
                debug_info
                    .module_source_path(&frame.version_id, &frame.module.name)?
                    .to_path_buf(),
            ),
            _ => None,
        })
}

/// Read a trace file, decompressing it when it is `.zst`.
fn read_trace_file(trace_file: &Path) -> Result<Vec<u8>> {
    let raw_bytes = fs::read(trace_file)
        .wrap_err_with(|| format!("Failed to read trace file: {}", trace_file.display()))?;
    if trace_file.extension().is_some_and(|ext| ext == "zst") {
        let mut decoder =
            zstd::Decoder::new(raw_bytes.as_slice()).wrap_err("Failed to create zstd decoder")?;
        let mut decompressed = Vec::new();
        decoder
            .read_to_end(&mut decompressed)
            .wrap_err("Failed to decompress zstd trace file")?;
        Ok(decompressed)
    } else {
        Ok(raw_bytes)
    }
}

/// Process an existing trace file through the replay pipeline.
///
/// This skips the `sui replay` step and directly processes a trace file.
/// Useful for testing and for cases where the trace file already exists.
///
/// The output format is fixed to CTFS — see
/// `Recorder-CLI-Conventions.md` §4 in `codetracer-specs`.
pub fn replay_from_existing_trace(
    trace_file: &Path,
    search_dirs: &[PathBuf],
    out_dir: &Path,
) -> Result<()> {
    let trace_data = read_trace_file(trace_file)?;

    // Discover source files.
    let source_lookup = SourceLookup::new(search_dirs.to_vec());
    let source_map = source_lookup.build_source_map();

    // Determine source path: try to find any .move file in the search dirs,
    // otherwise fall back to a generic name.
    let source_path = source_lookup
        .resolve("main")
        .or_else(|| {
            // Try to find any .move file
            search_dirs.iter().find_map(|dir| {
                if dir.exists() {
                    walkdir::WalkDir::new(dir)
                        .into_iter()
                        .filter_map(|e| e.ok())
                        .find(|e| e.path().extension().is_some_and(|ext| ext == "move"))
                        .map(|e| e.path().to_path_buf())
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| PathBuf::from("transaction.move"));

    // Create output directory.
    fs::create_dir_all(out_dir)
        .wrap_err_with(|| format!("Failed to create output directory: {}", out_dir.display()))?;

    // Convert the trace.
    converter::convert_trace(&trace_data, &source_map, &source_path, out_dir)?;

    eprintln!("Trace files written to {}", out_dir.display());

    Ok(())
}

/// Run `sui replay --trace --digest <DIGEST> --node <NODE> -o <OUT>`, which
/// writes the transaction's artifacts to `<OUT>/<DIGEST>/`: the trace, the
/// transaction data and effects, and each entered package's bytecode,
/// disassembly and debug info.
fn run_sui_replay(node: &str, digest: &str, out: &Path) -> Result<()> {
    // Check that sui CLI is available.
    let sui_check = Command::new("sui").arg("--version").output();
    if sui_check.is_err() {
        bail!(
            "sui CLI not found. Please install the Sui CLI and ensure it is in your PATH.\n\
             See: https://docs.sui.io/build/install"
        );
    }

    fs::create_dir_all(out)
        .wrap_err_with(|| format!("Failed to create replay directory: {}", out.display()))?;
    let output = Command::new("sui")
        .args(["replay", "--trace", "--digest", digest, "--node", node])
        .arg("-o")
        .arg(out)
        .current_dir(out)
        .output()
        .wrap_err("Failed to execute sui replay")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("pruned") || stderr.contains("not found") {
            bail!(
                "Transaction {digest} could not be replayed. It may have been pruned \
                 from the node or does not exist.\nNode: {node}\nError: {stderr}"
            );
        }
        bail!(
            "sui replay failed with exit code {:?}\nstderr: {}",
            output.status.code(),
            stderr
        );
    }

    if !out.join(digest).join("trace.json.zst").is_file() {
        bail!(
            "sui replay exited successfully but wrote no trace at {}; \
             is the sui binary built with the `tracing` feature?",
            out.join(digest).join("trace.json.zst").display()
        );
    }
    Ok(())
}

/// Find a trace file (`.json.zst` or `.json`) in the given directory.
///
/// Prefers `.json.zst` (compressed) over plain `.json`.
pub fn find_trace_file(dir: &Path) -> Result<PathBuf> {
    if !dir.exists() {
        return Err(eyre!(
            "Replay output directory does not exist: {}",
            dir.display()
        ));
    }

    let entries: Vec<_> = fs::read_dir(dir)
        .wrap_err_with(|| format!("Failed to read directory: {}", dir.display()))?
        .filter_map(|e| e.ok())
        .collect();

    // First, look for .json.zst files (compressed traces).
    for entry in &entries {
        let path = entry.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if name.ends_with(".json.zst") {
            return Ok(path);
        }
    }

    // Then, look for .json files.
    for entry in &entries {
        let path = entry.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if name.ends_with(".json") && !name.ends_with(".json.zst") {
            return Ok(path);
        }
    }

    Err(eyre!(
        "No trace file (.json.zst or .json) found in: {}",
        dir.display()
    ))
}

/// Find Move source files matching a module name in search directories.
pub fn find_source_files(module_name: &str, search_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let lookup = SourceLookup::new(search_dirs.to_vec());
    match lookup.resolve(module_name) {
        Some(path) => vec![path],
        None => vec![],
    }
}
