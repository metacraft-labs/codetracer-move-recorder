//! The `replay` and `aptos-replay` commands leave nothing behind in the
//! system temp directory, whether the replay succeeds or fails.
//!
//! Both commands run a chain CLI (`sui replay --trace` / `aptos move replay`)
//! in a scratch directory and read the trace it writes there. Each test runs
//! the real recorder binary with `TMPDIR` pointed at an empty directory of its
//! own and asserts that directory is empty again when the command exits.
//!
//! MOCK JUSTIFICATION: `sui` and `aptos` are replaced by small shell scripts
//! placed first on `PATH`. The real CLIs replay a transaction fetched from a
//! live RPC node, which a test cannot depend on. The scripts reproduce only
//! the part of the CLI contract the recorder relies on — `--version` succeeds,
//! the replay writes a trace into its working directory (sui) or to the path
//! given to `--trace` (aptos), `aptos move disassemble` writes
//! `<module>.mv.masm` to `--output-dir`, and a failed replay exits non-zero —
//! and what they write is real: committed `sui` and Aptos trace fixtures and
//! the committed disassemblies of the modules the Aptos trace enters. For
//! Aptos, `curl` (which fetches each module's bytecode from a fullnode) is
//! replaced as well, by a script that answers with a bytecode header; the
//! fake disassembler ignores the bytecode and copies the fixture's real
//! disassembly. Everything the recorder does with that output runs unmocked.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const DIGEST: &str = "4Xk9TestDigestForTempDirLifecycle";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Writes an executable `/bin/sh` script. `set -e` makes a command the
/// script cannot run (a `cp` missing from `PATH`, say) fail the script, so a
/// fake replay can never report success without having written its trace.
fn write_script(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\nset -e\n{body}")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

struct Sandbox {
    _root: tempfile::TempDir,
    bin: PathBuf,
    tmp: PathBuf,
    out: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let root = tempfile::TempDir::new().unwrap();
        let bin = root.path().join("bin");
        let tmp = root.path().join("tmp");
        let out = root.path().join("out");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&tmp).unwrap();
        Sandbox {
            _root: root,
            bin,
            tmp,
            out,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(env!("CARGO_BIN_EXE_codetracer-move-recorder"))
            .args(args)
            .env("PATH", path)
            .env("TMPDIR", &self.tmp)
            .env_remove("CODETRACER_MOVE_RECORDER_DISABLED")
            .output()
            .expect("failed to run codetracer-move-recorder")
    }

    fn leftovers(&self) -> Vec<String> {
        fs::read_dir(&self.tmp)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }
}

fn sui_fixture() -> PathBuf {
    manifest_dir()
        .join("test-programs/move/flow_test/traces/flow_test__flow_test__test_arithmetic.json.zst")
}

/// A `sui` that answers `--version`, and for `replay` writes the fixture
/// trace where the real `sui replay -o <OUT> --digest <D>` does:
/// `<OUT>/<D>/trace.json.zst`.
fn fake_sui(sandbox: &Sandbox, replay_exit: i32) {
    write_script(
        &sandbox.bin,
        "sui",
        &format!(
            "case \"$1\" in\n\
             --version) echo 'sui 0.0.0-test'; exit 0 ;;\n\
             replay)\n\
               out=.; digest=\n\
               while [ $# -gt 0 ]; do\n\
                 case \"$1\" in -o) out=\"$2\"; shift ;; --digest) digest=\"$2\"; shift ;; esac\n\
                 shift\n\
               done\n\
               mkdir -p \"$out/$digest\"\n\
               cp '{}' \"$out/$digest/trace.json.zst\"\n\
               exit {replay_exit} ;;\n\
             esac\nexit 2\n",
            sui_fixture().display()
        ),
    );
}

fn aptos_fixture() -> PathBuf {
    manifest_dir().join("tests/fixtures/aptos-7881330395")
}

/// An `aptos` whose `move replay ... --trace <PATH>` writes the fixture
/// trace to `<PATH>` and whose `move disassemble --bytecode-path <D>/<m>.mv
/// --output-dir <O>` writes the fixture's `<m>.mv.masm` to `<O>`; and a
/// `curl` that answers every module request with a bytecode header.
fn fake_aptos(sandbox: &Sandbox, replay_exit: i32) {
    let trace = sandbox.bin.join("aptos-trace.json");
    let compressed = fs::File::open(aptos_fixture().join("trace.json.zst")).unwrap();
    fs::write(&trace, zstd::decode_all(compressed).unwrap()).unwrap();
    let masm_dir = aptos_fixture()
        .join("0x0000000000000000000000000000000000000000000000000000000000000001/bytecode");
    write_script(
        &sandbox.bin,
        "aptos",
        &format!(
            "if [ \"$1\" = --version ]; then echo 'aptos 0.0.0-test'; exit 0; fi\n\
             cmd=\"$1 $2\"; trace=; mv=; outdir=\n\
             while [ $# -gt 0 ]; do\n\
               case \"$1\" in --trace) trace=\"$2\"; shift ;; --bytecode-path) mv=\"$2\"; shift ;; --output-dir) outdir=\"$2\"; shift ;; esac\n\
               shift\n\
             done\n\
             case \"$cmd\" in\n\
             'move replay') cp '{}' \"$trace\"; exit {replay_exit} ;;\n\
             'move disassemble') m=$(basename \"$mv\" .mv); cp '{}'/\"$m.mv.masm\" \"$outdir/\"; exit 0 ;;\n\
             esac\nexit 2\n",
            trace.display(),
            masm_dir.display()
        ),
    );
    write_script(
        &sandbox.bin,
        "curl",
        "printf '{\"bytecode\":\"0xa11ceb0b\"}'\n",
    );
}

fn assert_clean(sandbox: &Sandbox, output: &Output, what: &str) {
    let left = sandbox.leftovers();
    assert!(
        left.is_empty(),
        "{what} left {left:?} in TMPDIR\nstatus: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn sui_replay_success_leaves_no_temp_dir() {
    let sb = Sandbox::new();
    fake_sui(&sb, 0);
    let source_dir = manifest_dir().join("test-programs/move/flow_test");
    let out = sb.run(&[
        "replay",
        "--digest",
        DIGEST,
        "--source-dir",
        source_dir.to_str().unwrap(),
        "--out-dir",
        sb.out.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "replay should succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fs::read_dir(&sb.out).unwrap().next().is_some(),
        "replay should write a trace to --out-dir"
    );
    assert_clean(&sb, &out, "a successful sui replay");
}

#[test]
fn sui_replay_failure_leaves_no_temp_dir() {
    let sb = Sandbox::new();
    fake_sui(&sb, 1);
    let out = sb.run(&[
        "replay",
        "--digest",
        DIGEST,
        "--out-dir",
        sb.out.to_str().unwrap(),
    ]);
    assert!(!out.status.success(), "a failed sui replay must fail");
    assert_clean(&sb, &out, "a failed sui replay");
}

#[test]
fn aptos_replay_success_leaves_no_temp_dir() {
    let sb = Sandbox::new();
    fake_aptos(&sb, 0);
    let out = sb.run(&[
        "aptos-replay",
        "--txn-version",
        "42",
        "--out-dir",
        sb.out.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "aptos-replay should succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fs::read_dir(&sb.out).unwrap().next().is_some(),
        "aptos-replay should write a trace to --out-dir"
    );
    assert_clean(&sb, &out, "a successful aptos replay");
}

#[test]
fn aptos_replay_failure_leaves_no_temp_dir() {
    let sb = Sandbox::new();
    fake_aptos(&sb, 1);
    let out = sb.run(&[
        "aptos-replay",
        "--txn-version",
        "42",
        "--out-dir",
        sb.out.to_str().unwrap(),
    ]);
    assert!(!out.status.success(), "a failed aptos replay must fail");
    assert_clean(&sb, &out, "a failed aptos replay");
}
