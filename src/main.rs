use std::fs;
use std::io::Read;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use codetracer_trace_writer_nim::TraceEventsFileFormat;
use eyre::{bail, WrapErr};

/// Output trace container format selection for the CLI.
///
/// `Ctfs` is the canonical multi-stream container that the upstream Nim
/// reader (`NimTraceReaderHandle`) and the db-backend `CTFSTraceReader`
/// consume directly.  It is the default for new traces.
///
/// `Binary` is the legacy CBOR + Zstd format kept for backward compatibility
/// with older traces; `Json` is the human-readable variant useful for
/// debugging.
///
/// This mirrors the format selection added to other recorders during the
/// 2026-05 CTFS audits (Solana 1.44, EVM 1.39).
#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    /// Canonical CodeTracer multi-stream container (recommended).
    Ctfs,
    /// Legacy CBOR + Zstd binary format.
    Binary,
    /// Human-readable JSON (slower; useful for debugging).
    Json,
}

impl From<OutputFormat> for TraceEventsFileFormat {
    fn from(f: OutputFormat) -> Self {
        match f {
            OutputFormat::Ctfs => TraceEventsFileFormat::Ctfs,
            OutputFormat::Binary => TraceEventsFileFormat::Binary,
            OutputFormat::Json => TraceEventsFileFormat::Json,
        }
    }
}

use codetracer_move_recorder::aptos_adapter;
use codetracer_move_recorder::aptos_replay::{self, AptosReplayConfig};
use codetracer_move_recorder::converter;
use codetracer_move_recorder::replay::{self, ReplayConfig};
use codetracer_move_recorder::source_map::SourceMapResolver;

#[derive(Parser)]
#[command(name = "codetracer-move-recorder")]
#[command(version)]
#[command(about = "CodeTracer recorder for Move smart contracts (Sui/Aptos)")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Convert a Move trace file into CodeTracer trace format
    Record {
        /// Output directory for trace files
        #[arg(short, long, default_value = "./ct-traces/")]
        out_dir: PathBuf,

        /// Output format (ctfs, binary or json).  Defaults to `ctfs`,
        /// the canonical CodeTracer multi-stream container.
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Ctfs)]
        format: OutputFormat,

        /// Path to the Move source file (used for source mapping)
        #[arg(short, long)]
        source: Option<PathBuf>,

        /// Path to the trace file (e.g. trace.json or trace.json.zst)
        trace_file: PathBuf,
    },

    /// Replay an on-chain Sui transaction and produce a CodeTracer trace
    Replay {
        /// Transaction digest to replay
        #[arg(long)]
        digest: String,

        /// Sui RPC endpoint URL
        #[arg(long, default_value = "http://localhost:9000")]
        rpc_url: String,

        /// Directory containing Move source files
        #[arg(long)]
        source_dir: Option<PathBuf>,

        /// Output directory for trace files
        #[arg(short, long, default_value = "./ct-traces/")]
        out_dir: PathBuf,

        /// Output format (ctfs, binary or json).  Defaults to `ctfs`,
        /// the canonical CodeTracer multi-stream container.
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Ctfs)]
        format: OutputFormat,
    },

    /// Replay an on-chain Aptos transaction and produce a CodeTracer trace
    AptosReplay {
        /// Transaction version (ledger version) to replay
        #[arg(long)]
        txn_version: u64,

        /// Aptos REST API node URL
        #[arg(long, default_value = "https://fullnode.mainnet.aptoslabs.com/v1")]
        node_url: String,

        /// Directory containing Move source files
        #[arg(long)]
        source_dir: Option<PathBuf>,

        /// Output directory for trace files
        #[arg(short, long, default_value = "./ct-traces/")]
        out_dir: PathBuf,

        /// Output format (ctfs, binary or json).  Defaults to `ctfs`,
        /// the canonical CodeTracer multi-stream container.
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Ctfs)]
        format: OutputFormat,

        /// Also run --profile-gas for additional gas data
        #[arg(long, default_value = "true")]
        profile_gas: bool,
    },

    /// Show Aptos-specific limitations compared to Sui support
    AptosLimitations,

    /// Print version information
    Version,
}

fn main() -> eyre::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Record {
            out_dir,
            format,
            source,
            trace_file,
        } => {
            if !trace_file.exists() {
                bail!(
                    "Trace file does not exist: {}",
                    trace_file.display()
                );
            }

            // Read and optionally decompress the trace file.
            let raw_bytes = fs::read(&trace_file)
                .wrap_err_with(|| {
                    format!("Failed to read trace file: {}", trace_file.display())
                })?;

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
            let source_path = source.unwrap_or_else(|| {
                trace_file
                    .with_extension("")
                    .with_extension("move")
            });

            // For now, use an empty source map. Real source maps will come in a
            // later milestone when we parse .mvsm files.
            let source_map = SourceMapResolver::empty();

            let fmt: TraceEventsFileFormat = format.into();

            fs::create_dir_all(&out_dir)
                .wrap_err_with(|| {
                    format!(
                        "Failed to create output directory: {}",
                        out_dir.display()
                    )
                })?;

            converter::convert_trace(
                &trace_data,
                &source_map,
                &source_path,
                &out_dir,
                fmt,
            )?;

            eprintln!("Trace files written to {}", out_dir.display());
        }
        Commands::Replay {
            digest,
            rpc_url,
            source_dir,
            out_dir,
            format,
        } => {
            let fmt: TraceEventsFileFormat = format.into();

            let config = ReplayConfig {
                rpc_url,
                digest,
                source_dir,
                out_dir,
                format: fmt,
            };

            replay::replay_transaction(&config)?;
        }
        Commands::AptosReplay {
            txn_version,
            node_url,
            source_dir,
            out_dir,
            format,
            profile_gas,
        } => {
            let fmt: TraceEventsFileFormat = format.into();

            let config = AptosReplayConfig {
                node_url,
                txn_version,
                source_dir,
                out_dir,
                format: fmt,
                profile_gas,
            };

            aptos_replay::aptos_replay_transaction(&config)?;
        }
        Commands::AptosLimitations => {
            println!("{}", aptos_adapter::aptos_limitations_summary());
        }
        Commands::Version => {
            println!(
                "codetracer-move-recorder {}",
                env!("CARGO_PKG_VERSION")
            );
        }
    }

    Ok(())
}
