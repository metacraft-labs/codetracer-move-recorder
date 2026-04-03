use std::process::Command;

fn cargo_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_codetracer-move-recorder"))
}

#[test]
fn help_succeeds_and_mentions_name() {
    let output = cargo_bin().arg("--help").output().expect("failed to run");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("codetracer-move-recorder"),
        "Help output should mention codetracer-move-recorder, got: {stdout}"
    );
}

#[test]
fn version_succeeds_and_contains_version() {
    let output = cargo_bin().arg("--version").output().expect("failed to run");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("0.1.0"),
        "Version output should contain 0.1.0, got: {stdout}"
    );
}

#[test]
fn record_nonexistent_file_fails() {
    let output = cargo_bin()
        .args(["record", "/nonexistent/path/trace.json.zst"])
        .output()
        .expect("failed to run");
    assert!(
        !output.status.success(),
        "record with nonexistent file should fail"
    );
}

#[test]
fn record_creates_output_files() {
    let tmp = tempfile::TempDir::new().expect("failed to create temp dir");

    // Write a minimal valid NDJSON trace file (not compressed, .json extension).
    let trace_file = tmp.path().join("dummy_trace.json");
    let trace_data = "{\"version\":3}\n{\"OpenFrame\":{\"frame\":{\"frame_id\":1,\"function_name\":\"main\",\"module\":{\"address\":\"0x0\",\"name\":\"test\"},\"type_instantiation\":[],\"parameters\":[],\"return_types\":[],\"locals_types\":[],\"is_native\":false},\"gas_left\":1000}}\n{\"CloseFrame\":{\"frame_id\":1,\"return_\":[],\"gas_left\":900}}\n";
    std::fs::write(&trace_file, trace_data).expect("failed to write dummy trace");

    let out_dir = tmp.path().join("ct-traces");

    let output = cargo_bin()
        .args([
            "record",
            "-o",
            out_dir.to_str().unwrap(),
            trace_file.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run");

    assert!(
        output.status.success(),
        "record should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        out_dir.join("trace_metadata.json").exists(),
        "trace_metadata.json should be created"
    );
    assert!(
        out_dir.join("trace_paths.json").exists(),
        "trace_paths.json should be created"
    );
}
