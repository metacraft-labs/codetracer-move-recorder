//! Aptos on-chain transaction replay pipeline.
//!
//! Orchestrates:
//! 1. `aptos move replay --network <NET> --txn-id <VERSION> --trace <FILE>`,
//!    which re-executes the transaction against the state it ran on and
//!    writes a value trace in the move-trace-format NDJSON (version 3) the
//!    Sui converter reads: every frame, instruction and stack/local effect,
//!    with typed values.  `--trace` is not in the stock Aptos CLI; it comes
//!    from the CodeTracer fork (metacraft-labs/aptos-core, branch
//!    `codetracer/value-tracing-aptos-node-v1.49.8`).
//! 2. For every module the trace entered, the module's bytecode as it was
//!    on chain when the transaction ran, from the REST API
//!    (`/v1/accounts/{addr}/module/{name}?ledger_version=<VERSION - 1>`),
//!    and its disassembly from `aptos move disassemble`.
//! 3. The conversion shared with the Sui replay path
//!    ([`crate::replay::convert_replay_trace`]): each instruction becomes a
//!    step on its disassembly line, and values attach to those steps.
//!
//! The artifacts are laid out like a `sui replay` directory:
//!
//! ```text
//! <replay-dir>/trace.json
//! <replay-dir>/0x<address, 64 hex digits>/bytecode/<module>.mv
//! <replay-dir>/0x<address, 64 hex digits>/bytecode/<module>.mv.masm
//! <replay-dir>/0x<address, 64 hex digits>/source/<module>.move   (optional)
//! <replay-dir>/0x<address, 64 hex digits>/source/<module>.json   (optional)
//! ```
//!
//! and the bundle records each path relative to it.  The optional
//! `source/` files are a module's published source and the compiler's
//! source map for exactly its bytes, written by a source provider that
//! proved the source against the chain (BlockTracer's
//! `tools/chain/lib/aptos-source.mjs`); a module with them is positioned on
//! its source and its locals are named — see
//! [`DebugInfo::from_aptos_replay_dir`], which refuses a map that is not
//! bound to the bytes and texts beside it.  With
//! `--replay-dir` the directory is kept and reused: an existing trace is not
//! replayed again and existing modules are not fetched again.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use eyre::{Result, WrapErr, bail, eyre};
use serde::Deserialize;

use crate::aptos_adapter::{self, AptosRestConfig};
use crate::converter::{ConverterOptions, MoveChain};
use crate::move_debug_info::{DebugInfo, normalize_address};
use crate::replay::{self, DEFAULT_MAX_TRACE_BYTES, ReplayConversion};

/// Configuration for replaying an Aptos transaction.
///
/// The output format is fixed to CTFS — see
/// `Recorder-CLI-Conventions.md` §4 in `codetracer-specs`.  Use
/// `ct print` (from `codetracer-trace-format-nim`) for human-readable
/// conversion of the produced bundle.
#[derive(Debug, Clone)]
pub struct AptosReplayConfig {
    /// Network to replay on: `mainnet`, `testnet`, `devnet`, or a REST
    /// endpoint URL (passed to `aptos move replay --network`).
    pub network: String,
    /// Transaction version (ledger version) to replay.
    pub txn_version: u64,
    /// The Aptos CLI to run; it must support `aptos move replay --trace`.
    pub aptos_cli: PathBuf,
    /// API key for the node, sent as `Authorization: Bearer <key>` to lift
    /// the anonymous rate limit of the public fullnodes.
    pub node_api_key: Option<String>,
    /// Where to keep the replay's artifacts.  `None` uses a scratch
    /// directory that is removed afterwards.
    pub replay_dir: Option<PathBuf>,
    /// Refuse a trace that is larger than this many bytes.
    pub max_trace_bytes: u64,
    /// The container's recording id; `None` lets the writer mint one.
    pub recording_id: Option<String>,
    /// Output directory for CodeTracer trace files.
    pub out_dir: PathBuf,
}

impl AptosReplayConfig {
    /// Create a config with sensible defaults.
    pub fn new(txn_version: u64) -> Self {
        Self {
            network: "mainnet".to_string(),
            txn_version,
            aptos_cli: PathBuf::from("aptos"),
            node_api_key: None,
            replay_dir: None,
            max_trace_bytes: DEFAULT_MAX_TRACE_BYTES,
            recording_id: None,
            out_dir: PathBuf::from("./ct-traces/"),
        }
    }
}

/// The REST API base (`…/v1`) of a network as `aptos move replay
/// --network` names it.
pub fn rest_base_url(network: &str) -> String {
    match network {
        "mainnet" | "testnet" | "devnet" => {
            format!("https://fullnode.{network}.aptoslabs.com/v1")
        }
        url => {
            let url = url.trim_end_matches('/');
            if url.ends_with("/v1") {
                url.to_string()
            } else {
                format!("{url}/v1")
            }
        }
    }
}

/// Replay an Aptos transaction and produce a CodeTracer trace.
pub fn aptos_replay_transaction(config: &AptosReplayConfig) -> Result<ReplayConversion> {
    let (dir, _scratch) = match &config.replay_dir {
        Some(dir) => {
            fs::create_dir_all(dir)
                .wrap_err_with(|| format!("Failed to create {}", dir.display()))?;
            // Absolute, because the CLI runs with this directory as its
            // working directory and is handed paths inside it.
            let dir = fs::canonicalize(dir)
                .wrap_err_with(|| format!("Failed to resolve {}", dir.display()))?;
            (dir, None)
        }
        None => {
            let scratch = tempfile::Builder::new()
                .prefix(&format!("aptos-replay-{}-", config.txn_version))
                .tempdir()
                .wrap_err("Failed to create replay working directory")?;
            (scratch.path().to_path_buf(), Some(scratch))
        }
    };

    let trace_file = dir.join("trace.json");
    if !trace_file.is_file() {
        run_aptos_replay(config, &trace_file, &dir)?;
    }

    let modules = executed_modules(&replay::read_trace_file(&trace_file)?)?;
    let rest = rest_base_url(&config.network);
    // The state the transaction ran on is the ledger state after the
    // version before it; a module upgraded by the transaction itself must
    // not be read back in its upgraded form.
    let state_version = config.txn_version.saturating_sub(1);
    for (address, module) in &modules {
        let bytecode_dir = dir.join(format!("0x{address}")).join("bytecode");
        let mv = bytecode_dir.join(format!("{module}.mv"));
        if !mv.is_file() {
            fs::create_dir_all(&bytecode_dir)?;
            let bytes = fetch_module_bytecode(
                &rest,
                address,
                module,
                state_version,
                config.node_api_key.as_deref(),
            )?;
            fs::write(&mv, bytes).wrap_err_with(|| format!("Failed to write {}", mv.display()))?;
        }
        if !bytecode_dir.join(format!("{module}.mv.masm")).is_file() {
            disassemble(&config.aptos_cli, &mv, &bytecode_dir)?;
        }
    }

    convert_aptos_replay_dir_bounded(
        &dir,
        &config.out_dir,
        config.max_trace_bytes,
        config.recording_id.as_deref(),
    )
}

/// Convert an Aptos replay directory (see the module docs for its layout;
/// the trace may be `trace.json` or `trace.json.zst`) into a CTFS bundle in
/// `out_dir`.  Modules whose disassembly is missing or refused keep their
/// values and calls but have no line mapping; refusals are reported on
/// stderr and returned.
pub fn convert_aptos_replay_dir(dir: &Path, out_dir: &Path) -> Result<ReplayConversion> {
    convert_aptos_replay_dir_bounded(dir, out_dir, DEFAULT_MAX_TRACE_BYTES, None)
}

/// [`convert_aptos_replay_dir`] with an explicit bound on the trace size
/// and an optional fixed recording id.
pub fn convert_aptos_replay_dir_bounded(
    dir: &Path,
    out_dir: &Path,
    max_trace_bytes: u64,
    recording_id: Option<&str>,
) -> Result<ReplayConversion> {
    let trace_file = ["trace.json", "trace.json.zst"]
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| eyre!("no trace.json or trace.json.zst in {}", dir.display()))?;
    replay::convert_replay_trace(
        &trace_file,
        out_dir,
        max_trace_bytes,
        recording_id,
        || {
            DebugInfo::from_aptos_replay_dir(dir)
                .wrap_err_with(|| format!("Failed to read replay directory: {}", dir.display()))
        },
        ConverterOptions::default().with_chain(MoveChain::Aptos),
        "transaction.masm",
    )
}

/// The `(address, module)` pairs — address as 64 hex digits — of every
/// module a trace opened a frame in, in sorted order.
pub fn executed_modules(trace_data: &[u8]) -> Result<BTreeSet<(String, String)>> {
    #[derive(Deserialize)]
    struct Line {
        #[serde(rename = "OpenFrame")]
        open_frame: Option<OpenFrame>,
    }
    #[derive(Deserialize)]
    struct OpenFrame {
        frame: FrameModule,
    }
    #[derive(Deserialize)]
    struct FrameModule {
        module: crate::move_types::ModuleId,
    }

    let text = std::str::from_utf8(trace_data).wrap_err("trace is not UTF-8")?;
    let mut modules = BTreeSet::new();
    for (i, line) in text.lines().enumerate().skip(1) {
        if !line.starts_with("{\"OpenFrame\"") {
            continue;
        }
        let parsed: Line = serde_json::from_str(line)
            .wrap_err_with(|| format!("trace line {}: malformed OpenFrame", i + 1))?;
        if let Some(open) = parsed.open_frame {
            modules.insert((
                normalize_address(&open.frame.module.address),
                open.frame.module.name,
            ));
        }
    }
    Ok(modules)
}

/// Run `aptos move replay --trace`, writing the trace to `trace_file`.
fn run_aptos_replay(config: &AptosReplayConfig, trace_file: &Path, work_dir: &Path) -> Result<()> {
    let mut command = Command::new(&config.aptos_cli);
    command
        .args(["move", "replay", "--network", &config.network, "--txn-id"])
        .arg(config.txn_version.to_string())
        .arg("--trace")
        .arg(trace_file)
        .current_dir(work_dir);
    if let Some(key) = &config.node_api_key {
        command.args(["--node-api-key", key]);
    }
    let output = command.output().map_err(|e| {
        eyre!(
            "failed to run {} (an Aptos CLI with `move replay --trace` is required; \
             see metacraft-labs/aptos-core, branch codetracer/value-tracing-aptos-node-v1.49.8): {e}",
            config.aptos_cli.display()
        )
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stderr.contains("--trace") && stderr.contains("unexpected argument") {
            bail!(
                "{} does not support `aptos move replay --trace`; use the CodeTracer \
                 build of the Aptos CLI (metacraft-labs/aptos-core, branch \
                 codetracer/value-tracing-aptos-node-v1.49.8).\n{stderr}",
                config.aptos_cli.display()
            );
        }
        bail!(
            "aptos move replay of version {} on {} failed with exit code {:?}\n\
             stdout: {stdout}\nstderr: {stderr}",
            config.txn_version,
            config.network,
            output.status.code(),
        );
    }
    if !trace_file.is_file() {
        bail!(
            "aptos move replay exited successfully but wrote no trace at {}",
            trace_file.display()
        );
    }
    Ok(())
}

/// Fetch a module's bytecode at `ledger_version` from the REST API.
fn fetch_module_bytecode(
    rest_base: &str,
    address: &str,
    module: &str,
    ledger_version: u64,
    api_key: Option<&str>,
) -> Result<Vec<u8>> {
    let url =
        format!("{rest_base}/accounts/0x{address}/module/{module}?ledger_version={ledger_version}");
    let mut command = Command::new("curl");
    command
        // The public fullnodes allow an anonymous client 40,000 compute
        // units per 300 seconds and answer 429 beyond that; waiting out one
        // window is enough, so retries span a little more than one.
        .args([
            "-sS",
            "--fail-with-body",
            "--retry",
            "12",
            "--retry-delay",
            "30",
        ])
        .arg(&url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The key is passed on stdin rather than in the argument list, which
    // other users of the machine can read.
    if api_key.is_some() {
        command.args(["-H", "@-"]).stdin(Stdio::piped());
    }
    let mut child = command
        .spawn()
        .map_err(|e| eyre!("failed to run curl for {url}: {e}"))?;
    if let Some(key) = api_key
        && let Some(mut stdin) = child.stdin.take()
    {
        writeln!(stdin, "Authorization: Bearer {key}")?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let hint = if stderr.contains("429") {
            "\nThe node is rate-limiting this client: pass --node-api-key, or rerun later \
             with --replay-dir to reuse what was already fetched."
        } else {
            ""
        };
        bail!(
            "fetching {url} failed: {stderr}{}{hint}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    #[derive(Deserialize)]
    struct MoveModuleBytecode {
        bytecode: String,
    }
    let body: MoveModuleBytecode = serde_json::from_slice(&output.stdout)
        .wrap_err_with(|| format!("{url} did not return a module"))?;
    decode_hex(&body.bytecode).wrap_err_with(|| format!("{url}: bytecode is not hex"))
}

/// Decode `0x`-prefixed (or bare) hex.
fn decode_hex(text: &str) -> Result<Vec<u8>> {
    let hex = text.strip_prefix("0x").unwrap_or(text);
    if !hex.len().is_multiple_of(2) {
        bail!("odd number of hex digits");
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| eyre!("at offset {i}: {e}")))
        .collect()
}

/// Run `aptos move disassemble` on `mv`, writing `<module>.mv.masm` to
/// `out_dir`.
fn disassemble(aptos_cli: &Path, mv: &Path, out_dir: &Path) -> Result<()> {
    let output = Command::new(aptos_cli)
        .args(["move", "disassemble", "--assume-yes", "--bytecode-path"])
        .arg(mv)
        .arg("--output-dir")
        .arg(out_dir)
        .current_dir(out_dir)
        .output()
        .map_err(|e| {
            eyre!(
                "failed to run {} move disassemble: {e}",
                aptos_cli.display()
            )
        })?;
    let masm = out_dir.join(format!(
        "{}.masm",
        mv.file_name().unwrap_or_default().to_string_lossy()
    ));
    if !output.status.success() || !masm.is_file() {
        bail!(
            "aptos move disassemble of {} did not write {} (exit code {:?})\nstdout: {}\nstderr: {}",
            mv.display(),
            masm.display(),
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
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
        bail!("failed to fetch resources from {url}: HTTP error\n{stderr}");
    }

    let body = String::from_utf8_lossy(&output.stdout);
    aptos_adapter::parse_resources_response(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rest_base_follows_the_network_name() {
        assert_eq!(
            rest_base_url("mainnet"),
            "https://fullnode.mainnet.aptoslabs.com/v1"
        );
        assert_eq!(
            rest_base_url("http://localhost:8080"),
            "http://localhost:8080/v1"
        );
        assert_eq!(
            rest_base_url("http://localhost:8080/v1/"),
            "http://localhost:8080/v1"
        );
    }

    #[test]
    fn hex_decodes_with_and_without_prefix() {
        assert_eq!(
            decode_hex("0xa11ceb0b").unwrap(),
            vec![0xa1, 0x1c, 0xeb, 0x0b]
        );
        assert_eq!(decode_hex("00ff").unwrap(), vec![0, 255]);
        assert!(decode_hex("0xabc").is_err());
        assert!(decode_hex("zz").is_err());
    }
}
