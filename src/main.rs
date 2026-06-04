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
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Parser, Subcommand};
use eyre::{WrapErr, bail, eyre};

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

/// Walk upwards from `start` looking for the nearest directory that
/// contains a `Move.toml` (the package manifest).  Returns the package
/// directory on success or `None` if no manifest is found before we run
/// out of parents.
///
/// We treat `Move.toml` as the canonical package root marker because it
/// is the only file the Sui/Aptos Move CLI accepts as a package handle
/// for `move test --trace` / `move build`.
fn find_move_package_root(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_file() {
        start.parent().map(Path::to_path_buf)
    } else {
        Some(start.to_path_buf())
    };
    while let Some(dir) = current {
        if dir.join("Move.toml").is_file() {
            return Some(dir);
        }
        current = dir.parent().map(Path::to_path_buf);
    }
    None
}

/// Locate a Move toolchain CLI that supports `move test --trace`.
///
/// The recorder accepts either Sui (`sui`) or Aptos (`aptos`) on PATH.
/// An explicit override is honoured first via the `CODETRACER_MOVE_CLI`
/// env var (e.g. set to `/nix/store/.../bin/sui`) so deterministic
/// environments can pin the toolchain version.
fn detect_move_cli() -> eyre::Result<String> {
    if let Ok(explicit) = std::env::var("CODETRACER_MOVE_CLI")
        && !explicit.trim().is_empty()
    {
        return Ok(explicit);
    }
    for candidate in ["sui", "aptos"] {
        if which_on_path(candidate).is_some() {
            return Ok(candidate.to_string());
        }
    }
    bail!(
        "no Move toolchain on PATH (need `sui` or `aptos` to run `move test --trace`). \
         Override with the CODETRACER_MOVE_CLI environment variable."
    )
}

/// Minimal which-style PATH probe.  Returns the first PATH entry that
/// contains an executable file named `name`.  We avoid pulling in the
/// `which` crate for this single use.
fn which_on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Run `<cli> move test --trace --path <package_root>` and return the
/// generated trace directory (`<package_root>/traces`).
///
/// Sui and Aptos share the same `move test --trace` UX, so the same
/// invocation works for both.  We stream child stdio to our own
/// stderr/stdout so the recorder remains usable interactively when
/// invoked by hand.
fn run_move_test_with_trace(cli: &str, package_root: &Path) -> eyre::Result<PathBuf> {
    // Sui places trace artefacts under `<package_root>/traces/`.  The
    // directory is wiped on each `move test --trace` run, so any
    // pre-existing traces from a previous invocation get replaced —
    // this matches the GUI test expectation that `ct record` is
    // idempotent.
    let traces_dir = package_root.join("traces");

    let status = Command::new(cli)
        .arg("move")
        .arg("test")
        .arg("--trace")
        .arg("--path")
        .arg(package_root)
        // Silence the toolchain's progress chatter into stderr so the
        // important compile errors remain visible if anything fails.
        .status()
        .wrap_err_with(|| {
            format!(
                "failed to spawn `{cli} move test --trace` for package {}",
                package_root.display()
            )
        })?;

    if !status.success() {
        bail!(
            "`{cli} move test --trace` failed for package {} (exit status: {status})",
            package_root.display()
        );
    }

    if !traces_dir.is_dir() {
        bail!(
            "`{cli} move test --trace` did not produce a traces/ directory under {}",
            package_root.display()
        );
    }

    Ok(traces_dir)
}

/// Pick a trace file inside `traces_dir` to convert into a CodeTracer
/// recording.  The Sui/Aptos toolchains emit one file per `#[test]`
/// function (`<package>__<module>__<test>.json.zst`).  When the source
/// path identifies a module, we prefer traces whose filename references
/// that module so multi-module packages don't degenerate to whichever
/// test happens to sort first lexicographically.
///
/// Returns the selected trace file's path.
fn pick_trace_file(traces_dir: &Path, source_path: &Path) -> eyre::Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = fs::read_dir(traces_dir)
        .wrap_err_with(|| format!("failed to read trace dir {}", traces_dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with(".json.zst") || n.ends_with(".json"))
                .unwrap_or(false)
        })
        .collect();

    if candidates.is_empty() {
        bail!(
            "no Move trace files found in {} — did `move test --trace` succeed?",
            traces_dir.display()
        );
    }

    // Stable ordering so we always pick the same file on repeated runs
    // — important for tests that compare results across invocations.
    candidates.sort();

    // Prefer a trace whose filename embeds the source module stem.  The
    // toolchain encodes the test target as `<pkg>__<module>__<test>`, so
    // matching on `source_path` 's file stem (e.g. `flow_test`) is the
    // simplest reliable selector across Sui/Aptos.
    if let Some(stem) = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        && let Some(matched) = candidates.iter().find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.contains(stem))
                .unwrap_or(false)
        })
    {
        return Ok(matched.clone());
    }

    Ok(candidates[0].clone())
}

/// Read a (possibly zstd-compressed) Move NDJSON trace file into a byte
/// buffer ready for conversion.
fn load_trace_bytes(trace_file: &Path) -> eyre::Result<Vec<u8>> {
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

/// Drive the full `.move`-source → CTFS pipeline.
///
/// The flow:
///   1. Discover the Move package root by walking up to `Move.toml`.
///   2. Detect a Move toolchain CLI (`sui` or `aptos`) on PATH.
///   3. Run `<cli> move test --trace --path <package_root>` to produce
///      NDJSON traces under `<package_root>/traces/`.
///   4. Pick a trace file whose filename matches the requested source
///      file (so a multi-module package surfaces the intended target).
///   5. Convert the trace into a CTFS bundle under `out_dir`.
///
/// This is the path `ct record path/to/foo.move` exercises — the older
/// `record <trace.json.zst>` invocation continues to work for callers
/// that already have a pre-computed trace file.
fn record_from_move_source(
    source: &Path,
    explicit_source_path: Option<PathBuf>,
    out_dir: &Path,
) -> eyre::Result<()> {
    let package_root = find_move_package_root(source).ok_or_else(|| {
        eyre!(
            "could not locate a Move.toml package manifest at or above {}",
            source.display()
        )
    })?;
    eprintln!(
        "codetracer-move-recorder: tracing Move package at {}",
        package_root.display()
    );

    let cli = detect_move_cli()?;
    let traces_dir = run_move_test_with_trace(&cli, &package_root)?;
    let trace_file = pick_trace_file(&traces_dir, source)?;
    eprintln!(
        "codetracer-move-recorder: converting trace {}",
        trace_file.display()
    );

    let trace_data = load_trace_bytes(&trace_file)?;

    // The conversion needs a representative source path so the resulting
    // CTFS bundle can resolve breakpoints to the user-facing file.  The
    // CLI's explicit `--source` flag wins; otherwise we use the .move
    // file the user originally passed.
    let source_path = explicit_source_path.unwrap_or_else(|| source.to_path_buf());

    // Source-map resolution is not yet wired up for the source-driven
    // pipeline — the converter accepts an empty map and falls back to
    // best-effort line numbers via the package's `build/.../debug_info`
    // JSON, which the converter loads automatically when it sees a
    // `.move` source path inside a Sui/Aptos package layout.  Real
    // `.mvsm` parsing arrives in a follow-up milestone (see Sui
    // source-map TODO in `src/source_map.rs`).
    let source_map = SourceMapResolver::empty();

    // Opt in to every enrichment the GUI flow relies on
    // (per-source-line steps from debug info, MoveTestEntry baseline
    // event for the event log).  See `ConverterOptions::for_ct_record_flow`
    // for the full list and rationale.
    let options = converter::ConverterOptions::default().for_ct_record_flow();

    converter::convert_trace_with_options(
        &trace_data,
        &source_map,
        &source_path,
        out_dir,
        options,
    )?;
    Ok(())
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

            let out_dir = resolve_out_dir(out_dir);
            fs::create_dir_all(&out_dir).wrap_err_with(|| {
                format!("Failed to create output directory: {}", out_dir.display())
            })?;

            // ----------------------------------------------------------------
            // Two input modes are supported by the `record` subcommand:
            //
            //   (a) A pre-computed Move NDJSON trace file (legacy mode used
            //       by `aptos-replay` integration tests and the GUI test
            //       fixtures that ship `.json.zst` recordings).  Detected
            //       by a `.json` or `.json.zst` extension.
            //
            //   (b) A `.move` source file.  This is what `ct record
            //       path/to/foo.move` passes through from the user-facing
            //       CodeTracer CLI: the recorder walks up to `Move.toml`,
            //       invokes the Sui/Aptos `move test --trace` toolchain to
            //       generate NDJSON traces, and then converts the trace
            //       matching the requested module into a CTFS bundle.
            //
            // We split the two modes here rather than in `ct` because the
            // recorder is the only component that knows how to drive the
            // Move toolchain — `ct` deliberately treats every external
            // recorder as a black box that accepts a single program
            // argument.  This keeps the contract identical to circom,
            // cairo, leo, …, all of which take their language's source
            // file directly.
            // ----------------------------------------------------------------
            let extension = trace_file
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("");
            if extension.eq_ignore_ascii_case("move") {
                record_from_move_source(&trace_file, source, &out_dir)?;
            } else {
                // Legacy pre-computed-trace mode.
                let trace_data = load_trace_bytes(&trace_file)?;

                // Determine source path (use provided --source or derive
                // from trace file).
                let source_path =
                    source.unwrap_or_else(|| trace_file.with_extension("").with_extension("move"));

                // For now, use an empty source map. Real source maps will
                // come in a later milestone when we parse .mvsm files.
                let source_map = SourceMapResolver::empty();

                converter::convert_trace(&trace_data, &source_map, &source_path, &out_dir)?;
            }

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
