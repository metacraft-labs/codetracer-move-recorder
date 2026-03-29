//! On-chain transaction replay pipeline.
//!
//! Orchestrates the process of replaying a Sui transaction and converting
//! the resulting trace into CodeTracer format.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use codetracer_trace_writer::TraceEventsFileFormat;
use eyre::{Result, WrapErr, bail, eyre};

use crate::converter;
use crate::source_lookup::SourceLookup;

/// Configuration for replaying a transaction.
pub struct ReplayConfig {
    /// Sui RPC endpoint URL.
    pub rpc_url: String,
    /// Transaction digest to replay.
    pub digest: String,
    /// Directory containing Move source files (optional).
    pub source_dir: Option<PathBuf>,
    /// Output directory for CodeTracer trace files.
    pub out_dir: PathBuf,
    /// Output format (binary or json).
    pub format: TraceEventsFileFormat,
}

impl ReplayConfig {
    /// Create a config with sensible defaults.
    pub fn new(digest: String) -> Self {
        Self {
            rpc_url: "http://localhost:9000".to_string(),
            digest,
            source_dir: None,
            out_dir: PathBuf::from("./ct-traces/"),
            format: TraceEventsFileFormat::Binary,
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
    // Step 1: Run sui replay
    let replay_dir = run_sui_replay(&config.rpc_url, &config.digest)?;

    // Step 2: Find the trace file
    let trace_file = find_trace_file(&replay_dir)?;

    // Step 3-4: Process the trace file
    let search_dirs = match &config.source_dir {
        Some(dir) => vec![dir.clone()],
        None => vec![std::env::current_dir().unwrap_or_default()],
    };

    replay_from_existing_trace(&trace_file, &search_dirs, &config.out_dir, config.format)
}

/// Process an existing trace file through the replay pipeline.
///
/// This skips the `sui replay` step and directly processes a trace file.
/// Useful for testing and for cases where the trace file already exists.
pub fn replay_from_existing_trace(
    trace_file: &Path,
    search_dirs: &[PathBuf],
    out_dir: &Path,
    format: TraceEventsFileFormat,
) -> Result<()> {
    // Read and optionally decompress the trace file.
    let raw_bytes = fs::read(trace_file)
        .wrap_err_with(|| format!("Failed to read trace file: {}", trace_file.display()))?;

    let trace_data = if trace_file
        .extension()
        .is_some_and(|ext| ext == "zst")
    {
        let mut decoder = zstd::Decoder::new(raw_bytes.as_slice())
            .wrap_err("Failed to create zstd decoder")?;
        let mut decompressed = Vec::new();
        decoder
            .read_to_end(&mut decompressed)
            .wrap_err("Failed to decompress zstd trace file")?;
        decompressed
    } else {
        raw_bytes
    };

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
                        .find(|e| {
                            e.path()
                                .extension()
                                .is_some_and(|ext| ext == "move")
                        })
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
    converter::convert_trace(&trace_data, &source_map, &source_path, out_dir, format)?;

    eprintln!("Trace files written to {}", out_dir.display());

    Ok(())
}

/// Run `sui replay --trace --digest <DIGEST>` and return the output directory.
fn run_sui_replay(rpc_url: &str, digest: &str) -> Result<PathBuf> {
    // Check that sui CLI is available.
    let sui_check = Command::new("sui").arg("--version").output();
    if sui_check.is_err() {
        bail!(
            "sui CLI not found. Please install the Sui CLI and ensure it is in your PATH.\n\
             See: https://docs.sui.io/build/install"
        );
    }

    // Create a temporary directory for replay output.
    let replay_dir = std::env::temp_dir().join(format!("sui-replay-{digest}"));
    fs::create_dir_all(&replay_dir)
        .wrap_err("Failed to create replay output directory")?;

    let output = Command::new("sui")
        .args([
            "replay",
            "--rpc-url",
            rpc_url,
            "--trace",
            "--digest",
            digest,
        ])
        .current_dir(&replay_dir)
        .output()
        .wrap_err("Failed to execute sui replay")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("pruned") || stderr.contains("not found") {
            bail!(
                "Transaction {digest} could not be replayed. It may have been pruned \
                 from the node or does not exist.\nRPC: {rpc_url}\nError: {stderr}"
            );
        }
        bail!(
            "sui replay failed with exit code {:?}\nstderr: {}",
            output.status.code(),
            stderr
        );
    }

    Ok(replay_dir)
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
