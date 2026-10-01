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
//! in `MOVE_VM_TRACE` (aptos), and a failed replay exits non-zero — and the
//! trace they write is a real one: a committed `sui` trace fixture, and the
//! MOVE_VM_TRACE CSV shape exercised in `test_aptos.rs`. Everything the
//! recorder does with that output runs unmocked.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const DIGEST: &str = "4Xk9TestDigestForTempDirLifecycle";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn write_script(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
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

fn fake_sui(sandbox: &Sandbox, replay_exit: i32) {
    write_script(
        &sandbox.bin,
        "sui",
        &format!(
            "case \"$1\" in\n\
             --version) echo 'sui 0.0.0-test'; exit 0 ;;\n\
             replay) cp '{}' ./trace.json.zst; exit {replay_exit} ;;\n\
             esac\nexit 2\n",
            sui_fixture().display()
        ),
    );
}

fn fake_aptos(sandbox: &Sandbox, replay_exit: i32) {
    write_script(
        &sandbox.bin,
        "aptos",
        &format!(
            "if [ \"$1\" = --version ]; then echo 'aptos 0.0.0-test'; exit 0; fi\n\
             if [ -n \"$MOVE_VM_TRACE\" ]; then\n\
             printf '0x1::module::init,0\\n0x1::module::init,1\\n0x1::module::process,0\\n' > \"$MOVE_VM_TRACE\"\n\
             exit {replay_exit}\n\
             fi\n\
             echo 'gas profiling unavailable' >&2; exit 1\n"
        ),
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
