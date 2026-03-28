use std::fs;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use eyre::{bail, WrapErr};
use serde_json::json;

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

        /// Path to the trace file (e.g. trace.json.zst)
        trace_file: PathBuf,
    },

    /// Print version information
    Version,
}

fn main() -> eyre::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Record {
            out_dir,
            format: _,
            trace_file,
        } => {
            if !trace_file.exists() {
                bail!(
                    "Trace file does not exist: {}",
                    trace_file.display()
                );
            }

            eprintln!("Trace conversion not yet implemented");

            fs::create_dir_all(&out_dir)
                .wrap_err_with(|| format!("Failed to create output directory: {}", out_dir.display()))?;

            let metadata = json!({
                "recorder": "codetracer-move-recorder",
                "version": env!("CARGO_PKG_VERSION"),
                "status": "placeholder"
            });
            fs::write(
                out_dir.join("trace_metadata.json"),
                serde_json::to_string_pretty(&metadata)?,
            )?;

            let paths = json!({
                "trace_metadata": "trace_metadata.json",
                "status": "placeholder"
            });
            fs::write(
                out_dir.join("trace_paths.json"),
                serde_json::to_string_pretty(&paths)?,
            )?;

            eprintln!("Placeholder trace files written to {}", out_dir.display());
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
