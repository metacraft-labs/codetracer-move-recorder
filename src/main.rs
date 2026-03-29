use std::fs;
use std::io::Read;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use codetracer_trace_writer::TraceEventsFileFormat;
use eyre::{bail, WrapErr};

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

        /// Output format (binary or json)
        #[arg(short, long, default_value = "binary")]
        format: String,

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

        /// Output format (binary or json)
        #[arg(short, long, default_value = "binary")]
        format: String,
    },

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

            let fmt = match format.as_str() {
                "json" => TraceEventsFileFormat::Json,
                _ => TraceEventsFileFormat::Binary,
            };

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
            let fmt = match format.as_str() {
                "json" => TraceEventsFileFormat::Json,
                _ => TraceEventsFileFormat::Binary,
            };

            let config = ReplayConfig {
                rpc_url,
                digest,
                source_dir,
                out_dir,
                format: fmt,
            };

            replay::replay_transaction(&config)?;
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
