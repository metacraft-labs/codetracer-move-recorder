//! Aptos on-chain transaction replay pipeline.
//!
//! Orchestrates:
//! 1. `aptos move replay --txn-version <VERSION>` with optional `--profile-gas`
//! 2. Parse MOVE_VM_TRACE CSV output (requires `MOVE_VM_TRACE` env var pointing to output file)
//! 3. Parse gas profiler JSON output (when `--profile-gas` is used)
//! 4. Fetch historical account state via REST API
//! 5. Merge trace + gas data and produce CodeTracer trace
//!
//! ## Differences from Sui replay
//!
//! Sui's `sui replay --trace` produces rich structured traces in a single step.
//! Aptos replay requires combining multiple data sources:
//! - MOVE_VM_TRACE env var for execution trace (minimal CSV)
//! - --profile-gas for gas breakdown (JSON flamegraph)
//! - REST API for historical state (account resources at ledger versions)
//!
//! None of these individually provides the depth of Sui's trace format.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use codetracer_trace_writer_nim::TraceEventsFileFormat;
use eyre::{Result, bail, eyre};

use crate::aptos_adapter::{
    self, AptosRestConfig, merge_trace_and_gas, parse_gas_profile_json, parse_move_vm_trace_csv,
};

/// Configuration for replaying an Aptos transaction.
#[derive(Debug, Clone)]
pub struct AptosReplayConfig {
    /// Aptos REST API base URL.
    pub node_url: String,
    /// Transaction version (ledger version) to replay.
    pub txn_version: u64,
    /// Directory containing Move source files (optional).
    pub source_dir: Option<PathBuf>,
    /// Output directory for CodeTracer trace files.
    pub out_dir: PathBuf,
    /// Output format (binary or json).
    pub format: TraceEventsFileFormat,
    /// Whether to also run --profile-gas for additional data.
    pub profile_gas: bool,
}

impl AptosReplayConfig {
    /// Create a config with sensible defaults.
    pub fn new(txn_version: u64) -> Self {
        Self {
            node_url: "https://fullnode.mainnet.aptoslabs.com/v1".to_string(),
            txn_version,
            source_dir: None,
            out_dir: PathBuf::from("./ct-traces/"),
            format: TraceEventsFileFormat::Binary,
            profile_gas: true,
        }
    }
}

/// Replay an Aptos transaction and produce a CodeTracer trace.
///
/// Steps:
/// 1. Run `aptos move replay --txn-version <VERSION>` with MOVE_VM_TRACE env
/// 2. Optionally run with `--profile-gas` for gas data
/// 3. Parse the MOVE_VM_TRACE CSV and gas profile JSON
/// 4. Merge and convert to CodeTracer format
///
/// Note: This requires the `aptos` CLI to be installed and in PATH.
pub fn aptos_replay_transaction(config: &AptosReplayConfig) -> Result<()> {
    // Check that aptos CLI is available.
    let aptos_check = Command::new("aptos").arg("--version").output();
    if aptos_check.is_err() {
        bail!(
            "aptos CLI not found. Please install the Aptos CLI and ensure it is in your PATH.\n\
             See: https://aptos.dev/tools/aptos-cli/install-cli"
        );
    }

    // Create working directory.
    let work_dir = std::env::temp_dir().join(format!("aptos-replay-{}", config.txn_version));
    fs::create_dir_all(&work_dir)
        .map_err(|e| eyre!("failed to create working directory: {e}"))?;

    // Step 1: Run aptos move replay with MOVE_VM_TRACE
    let trace_csv_path = work_dir.join("move_vm_trace.csv");
    let trace_csv = run_aptos_replay_with_trace(
        &config.node_url,
        config.txn_version,
        &trace_csv_path,
        &work_dir,
    )?;

    // Step 2: Optionally get gas profile data.
    let gas_profile = if config.profile_gas {
        match run_aptos_replay_with_gas_profile(
            &config.node_url,
            config.txn_version,
            &work_dir,
        ) {
            Ok(profile) => Some(profile),
            Err(e) => {
                eprintln!("warning: gas profiling failed (continuing without gas data): {e}");
                None
            }
        }
    } else {
        None
    };

    // Step 3: Parse trace entries.
    let trace_entries = parse_move_vm_trace_csv(&trace_csv);
    if trace_entries.is_empty() {
        bail!(
            "MOVE_VM_TRACE produced no trace entries for transaction version {}.\n\
             This may indicate the transaction did not execute any Move bytecode.",
            config.txn_version
        );
    }

    // Step 4: Merge trace + gas data.
    let enriched = merge_trace_and_gas(&trace_entries, gas_profile.as_ref());

    // Step 5: Determine source path.
    let source_path = config
        .source_dir
        .as_ref()
        .and_then(|dir| find_first_move_file(dir))
        .unwrap_or_else(|| PathBuf::from("aptos_transaction.move"));

    // Step 6: Convert to CodeTracer format.
    fs::create_dir_all(&config.out_dir)
        .map_err(|e| eyre!("failed to create output directory: {e}"))?;

    aptos_adapter::convert_aptos_trace(&enriched, &source_path, &config.out_dir, config.format)?;

    eprintln!(
        "Aptos trace files written to {}\n\n{}",
        config.out_dir.display(),
        aptos_adapter::aptos_limitations_summary()
    );

    Ok(())
}

/// Process existing MOVE_VM_TRACE CSV data (bypassing the aptos CLI).
///
/// Useful for testing and for cases where trace data was collected separately.
pub fn aptos_replay_from_existing_data(
    trace_csv: &str,
    gas_profile_json: Option<&str>,
    source_path: &Path,
    out_dir: &Path,
    format: TraceEventsFileFormat,
) -> Result<()> {
    let trace_entries = parse_move_vm_trace_csv(trace_csv);
    if trace_entries.is_empty() {
        bail!("no trace entries in MOVE_VM_TRACE CSV data");
    }

    let gas_profile = match gas_profile_json {
        Some(json) => Some(parse_gas_profile_json(json)?),
        None => None,
    };

    let enriched = merge_trace_and_gas(&trace_entries, gas_profile.as_ref());

    fs::create_dir_all(out_dir)
        .map_err(|e| eyre!("failed to create output directory: {e}"))?;

    aptos_adapter::convert_aptos_trace(&enriched, source_path, out_dir, format)?;

    Ok(())
}

/// Run `aptos move replay` with `MOVE_VM_TRACE` env var set.
fn run_aptos_replay_with_trace(
    node_url: &str,
    txn_version: u64,
    trace_output_path: &Path,
    work_dir: &Path,
) -> Result<String> {
    let output = Command::new("aptos")
        .args([
            "move",
            "replay",
            "--txn-version",
            &txn_version.to_string(),
            "--node-url",
            node_url,
        ])
        .env("MOVE_VM_TRACE", trace_output_path.to_str().unwrap_or(""))
        .current_dir(work_dir)
        .output()
        .map_err(|e| eyre!("failed to execute aptos move replay: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not found") || stderr.contains("does not exist") {
            bail!(
                "Transaction version {txn_version} could not be replayed. \
                 It may not exist or the node may not have the required data.\n\
                 Node URL: {node_url}\nError: {stderr}"
            );
        }
        bail!(
            "aptos move replay failed with exit code {:?}\nstderr: {}",
            output.status.code(),
            stderr
        );
    }

    // Read the trace CSV file produced by MOVE_VM_TRACE.
    if trace_output_path.exists() {
        fs::read_to_string(trace_output_path)
            .map_err(|e| eyre!("failed to read MOVE_VM_TRACE output: {e}"))
    } else {
        // Some versions of the Aptos VM may not produce the trace file
        // if MOVE_VM_TRACE is not supported or the feature is disabled.
        bail!(
            "MOVE_VM_TRACE output file was not created at {}.\n\
             The Aptos CLI may not support MOVE_VM_TRACE, or the transaction \
             did not execute any Move bytecode.",
            trace_output_path.display()
        );
    }
}

/// Run `aptos move replay --profile-gas` and parse the JSON output.
fn run_aptos_replay_with_gas_profile(
    node_url: &str,
    txn_version: u64,
    work_dir: &Path,
) -> Result<aptos_adapter::AptosGasProfile> {
    let output = Command::new("aptos")
        .args([
            "move",
            "replay",
            "--txn-version",
            &txn_version.to_string(),
            "--node-url",
            node_url,
            "--profile-gas",
        ])
        .current_dir(work_dir)
        .output()
        .map_err(|e| eyre!("failed to execute aptos move replay --profile-gas: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("aptos move replay --profile-gas failed: {stderr}");
    }

    // The gas profiler writes output files to the working directory.
    // Look for a JSON file with gas profile data.
    let gas_json_path = find_gas_profile_json(work_dir)?;
    let json_data = fs::read_to_string(&gas_json_path)
        .map_err(|e| eyre!("failed to read gas profile JSON: {e}"))?;

    parse_gas_profile_json(&json_data)
}

/// Find the gas profile JSON file in the work directory.
fn find_gas_profile_json(dir: &Path) -> Result<PathBuf> {
    let entries = fs::read_dir(dir)
        .map_err(|e| eyre!("failed to read directory {}: {e}", dir.display()))?;

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if name.ends_with(".json") && name.contains("gas") {
            return Ok(path);
        }
    }

    Err(eyre!(
        "no gas profile JSON file found in {}",
        dir.display()
    ))
}

/// Find the first .move file in a directory (recursively).
fn find_first_move_file(dir: &Path) -> Option<PathBuf> {
    if !dir.exists() {
        return None;
    }
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext == "move")
        })
        .map(|e| e.path().to_path_buf())
}

/// Fetch historical account resources from the Aptos REST API.
///
/// Uses `GET /v1/accounts/{address}/resources?ledger_version={version}`
/// to retrieve account state at a specific ledger version.
///
/// This requires making an HTTP request to the Aptos REST API.
/// In the current implementation, this shells out to `curl` to avoid
/// adding a heavy HTTP client dependency.
pub fn fetch_historical_resources(
    rest_config: &AptosRestConfig,
    address: &str,
    ledger_version: Option<u64>,
) -> Result<Vec<aptos_adapter::AptosResourceData>> {
    let url = rest_config.resources_url(address, ledger_version);

    let output = Command::new("curl")
        .args(["-s", "-f", &url])
        .output()
        .map_err(|e| eyre!("failed to execute curl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to fetch resources from {url}: HTTP error\n{stderr}"
        );
    }

    let body = String::from_utf8_lossy(&output.stdout);
    aptos_adapter::parse_resources_response(&body)
}
