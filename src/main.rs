//! CLI entry point for the CodeTracer Move recorder.
//!
//! Supports three subcommands:
//!
//! * `record`        — convert a Sui move-trace-format NDJSON file (Move
//!                     trace v3) into a CodeTracer CTFS trace bundle.
//! * `replay`        — replay an on-chain Sui transaction via the local
//!                     `sui replay` CLI and convert the resulting trace
//!                     into a CodeTracer CTFS trace bundle.
//! * `aptos-replay`  — replay an on-chain Aptos transaction via the local
//!                     `aptos move replay` CLI (with optional gas
//!                     profiling) and convert the resulting trace into a
//!                     CodeTracer CTFS trace bundle.
//!
//! # Usage
//!
//! ```text
//! codetracer-move-recorder record <trace-file> --out-dir <output-dir>
//! ```
//!
//! The recorder always writes traces in the canonical CodeTracer multi-stream
//! CTFS format (see `Recorder-CLI-Conventions.md` §4 in `codetracer-specs`).
//! No `--format` flag is exposed: human-readable conversion is handled
//! out-of-band by `ct print` (shipped with `codetracer-trace-format-nim`).
//!
//! # Environment variables
//!
//! * `CODETRACER_MOVE_RECORDER_OUT_DIR` — fallback for `--out-dir` when the
//!   flag is not given. The CLI flag always wins.
//! * `CODETRACER_MOVE_RECORDER_DISABLED` — set to `1` or `true` to skip
//!   recording entirely. The recorder still validates its inputs (where
//!   applicable) and propagates a clean exit code.
//! * `CODETRACER_MOVE_RECORDER_LOG_LEVEL` — recorder log verbosity (advisory;
//!   the Move recorder currently logs to stderr unconditionally).

use std::fs;
use std::io::Read;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use eyre::{WrapErr, bail};

use codetracer_move_recorder::aptos_adapter;
use codetracer_move_recorder::aptos_replay::{self, AptosReplayConfig};
use codetracer_move_recorder::converter;
use codetracer_move_recorder::replay::{self, ReplayConfig};
use codetracer_move_recorder::source_map::SourceMapResolver;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Environment variable used as a fallback for `--out-dir` when the CLI
/// flag is omitted.  Convention: see `Recorder-CLI-Conventions.md` §5.
const ENV_OUT_DIR: &str = "CODETRACER_MOVE_RECORDER_OUT_DIR";

/// Environment variable that, when set to `1`/`true`, disables tracing
/// entirely — the recorder runs as a transparent pass-through.
const ENV_DISABLED: &str = "CODETRACER_MOVE_RECORDER_DISABLED";

/// Default output directory used when neither `--out-dir` nor
/// `CODETRACER_MOVE_RECORDER_OUT_DIR` is set.
const DEFAULT_OUT_DIR: &str = "./ct-traces/";

// ---------------------------------------------------------------------------
// CLI definition
// ---------------------------------------------------------------------------

/// CodeTracer Move recorder -- record Move smart-contract execution traces.
///
/// Traces are always written in the canonical CTFS multi-stream format.
/// To convert a recorded `.ct` bundle to JSON / text for inspection, use
/// `ct print` from `codetracer-trace-format-nim`.
#[derive(Parser)]
#[command(name = "codetracer-move-recorder")]
#[command(version)]
#[command(
    about = "CodeTracer recorder for Move smart contracts (Sui/Aptos) (CTFS-only). \
             Use `ct print` from codetracer-trace-format-nim for human-readable conversion.",
    long_about = "Record Move smart-contract execution traces for CodeTracer.\n\
                  \n\
                  Output is always written in the canonical CodeTracer CTFS\n\
                  multi-stream format. Use `ct print` (shipped with the\n\
                  codetracer-trace-format-nim sibling) to convert a recorded\n\
                  `.ct` bundle to JSON or other human-readable forms.\n\
                  \n\
                  Environment variables:\n\
                    CODETRACER_MOVE_RECORDER_OUT_DIR    fallback for --out-dir\n\
                    CODETRACER_MOVE_RECORDER_DISABLED   set to 1/true to skip recording\n\
                    CODETRACER_MOVE_RECORDER_LOG_LEVEL  log verbosity (advisory)"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Convert a Move trace file into CodeTracer CTFS trace format.
    Record {
        /// Output directory for trace files.
        ///
        /// Falls back to the `CODETRACER_MOVE_RECORDER_OUT_DIR` environment
        /// variable when the flag is omitted.
        #[arg(short, long)]
        out_dir: Option<PathBuf>,

        /// Path to the Move source file (used for source mapping).
        #[arg(short, long)]
        source: Option<PathBuf>,

        /// Path to the trace file (e.g. trace.json or trace.json.zst).
        trace_file: PathBuf,
    },

    /// Replay an on-chain Sui transaction and produce a CodeTracer CTFS trace.
    Replay {
        /// Transaction digest to replay.
        #[arg(long)]
        digest: String,

        /// Sui RPC endpoint URL.
        #[arg(long, default_value = "http://localhost:9000")]
        rpc_url: String,

        /// Directory containing Move source files.
        #[arg(long)]
        source_dir: Option<PathBuf>,

        /// Output directory for trace files.
        ///
        /// Falls back to the `CODETRACER_MOVE_RECORDER_OUT_DIR` environment
        /// variable when the flag is omitted.
        #[arg(short, long)]
        out_dir: Option<PathBuf>,
    },

    /// Replay an on-chain Aptos transaction and produce a CodeTracer CTFS trace.
    AptosReplay {
        /// Transaction version (ledger version) to replay.
        #[arg(long)]
        txn_version: u64,

        /// Aptos REST API node URL.
        #[arg(long, default_value = "https://fullnode.mainnet.aptoslabs.com/v1")]
        node_url: String,

        /// Directory containing Move source files.
        #[arg(long)]
        source_dir: Option<PathBuf>,

        /// Output directory for trace files.
        ///
        /// Falls back to the `CODETRACER_MOVE_RECORDER_OUT_DIR` environment
        /// variable when the flag is omitted.
        #[arg(short, long)]
        out_dir: Option<PathBuf>,

        /// Also run --profile-gas for additional gas data.
        #[arg(long, default_value = "true")]
        profile_gas: bool,
    },

    /// Show Aptos-specific limitations compared to Sui support.
    AptosLimitations,

    /// Print version information.
    Version,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the effective output directory:
///   1. `--out-dir` if given on the CLI.
///   2. `CODETRACER_MOVE_RECORDER_OUT_DIR` env var.
///   3. `DEFAULT_OUT_DIR` ("./ct-traces/").
fn resolve_out_dir(cli_out_dir: Option<PathBuf>) -> PathBuf {
    if let Some(path) = cli_out_dir {
        return path;
    }
    if let Some(value) = std::env::var_os(ENV_OUT_DIR)
        && !value.is_empty()
    {
        return PathBuf::from(value);
    }
    PathBuf::from(DEFAULT_OUT_DIR)
}

/// Whether the recorder is disabled via env var.  When true, the CLI
/// must execute its target operation in pass-through mode without
/// emitting any trace artefacts.
fn recording_disabled() -> bool {
    match std::env::var(ENV_DISABLED) {
        Ok(value) => {
            let v = value.trim();
            v == "1" || v.eq_ignore_ascii_case("true")
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() -> eyre::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Record {
            out_dir,
            source,
            trace_file,
        } => {
            if !trace_file.exists() {
                bail!("Trace file does not exist: {}", trace_file.display());
            }

            if recording_disabled() {
                eprintln!("{ENV_DISABLED} is set; skipping trace recording (no output written).");
                return Ok(());
            }

            // Read and optionally decompress the trace file.
            let raw_bytes = fs::read(&trace_file)
                .wrap_err_with(|| format!("Failed to read trace file: {}", trace_file.display()))?;

            let trace_data = if trace_file.extension().is_some_and(|ext| ext == "zst") {
                // Decompress zstd
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

            // Determine source path (use provided --source or derive from trace file).
            let source_path =
                source.unwrap_or_else(|| trace_file.with_extension("").with_extension("move"));

            // For now, use an empty source map. Real source maps will come in a
            // later milestone when we parse .mvsm files.
            let source_map = SourceMapResolver::empty();

            let out_dir = resolve_out_dir(out_dir);
            fs::create_dir_all(&out_dir).wrap_err_with(|| {
                format!("Failed to create output directory: {}", out_dir.display())
            })?;

            converter::convert_trace(&trace_data, &source_map, &source_path, &out_dir)?;

            eprintln!("Trace files written to {}", out_dir.display());
        }
        Commands::Replay {
            digest,
            rpc_url,
            source_dir,
            out_dir,
        } => {
            if recording_disabled() {
                eprintln!("{ENV_DISABLED} is set; skipping replay recording (no output written).");
                return Ok(());
            }

            let out_dir = resolve_out_dir(out_dir);

            let config = ReplayConfig {
                rpc_url,
                digest,
                source_dir,
                out_dir,
            };

            replay::replay_transaction(&config)?;
        }
        Commands::AptosReplay {
            txn_version,
            node_url,
            source_dir,
            out_dir,
            profile_gas,
        } => {
            if recording_disabled() {
                eprintln!(
                    "{ENV_DISABLED} is set; skipping Aptos replay recording (no output written)."
                );
                return Ok(());
            }

            let out_dir = resolve_out_dir(out_dir);

            let config = AptosReplayConfig {
                node_url,
                txn_version,
                source_dir,
                out_dir,
                profile_gas,
            };

            aptos_replay::aptos_replay_transaction(&config)?;
        }
        Commands::AptosLimitations => {
            println!("{}", aptos_adapter::aptos_limitations_summary());
        }
        Commands::Version => {
            println!("codetracer-move-recorder {}", env!("CARGO_PKG_VERSION"));
        }
    }

    Ok(())
}
