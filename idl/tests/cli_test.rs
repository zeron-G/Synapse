/// CLI integration tests for `synapse compile`.
///
/// These tests build the `synapse` binary and invoke it as a subprocess,
/// verifying correct output files, error messages, and exit codes.
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Build the synapse binary once and return its path.
fn synapse_bin() -> PathBuf {
    // Build the binary via cargo
    let status = Command::new("cargo")
        .args(["build", "--bin", "synapse"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("failed to run cargo build");
    assert!(status.success(), "cargo build failed");

    // Find the binary in target/debug (idl/ is a standalone crate, not a workspace member)
    let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("debug")
        .join("synapse");
    assert!(bin.exists(), "binary not found at {}", bin.display());
    bin
}

fn game_bridge_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("game.bridge")
}

/// Helper: run the synapse binary with args, return (exit_code, stdout, stderr).
fn run(args: &[&str]) -> (i32, String, String) {
    let bin = synapse_bin();
    let output = Command::new(&bin)
        .args(args)
        .output()
        .expect("failed to execute synapse binary");

    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (code, stdout, stderr)
}

// ============================================================
// Success cases
// ============================================================

#[test]
fn test_compile_rust() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run(&[
        "compile",
        game_bridge_path().to_str().unwrap(),
        "--lang",
        "rust",
        "--out-dir",
        tmp.path().to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("wrote"), "stdout: {stdout}");

    let out = tmp.path().join("game.rs");
    assert!(out.exists(), "expected game.rs in output dir");

    let content = fs::read_to_string(&out).unwrap();
    assert!(content.contains("#[repr(C)]"));
    assert!(content.contains("pub struct Vec3f"));
}

#[test]
fn test_compile_python() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run(&[
        "compile",
        game_bridge_path().to_str().unwrap(),
        "--lang",
        "python",
        "--out-dir",
        tmp.path().to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("wrote"), "stdout: {stdout}");

    let out = tmp.path().join("game.py");
    assert!(out.exists(), "expected game.py in output dir");

    let content = fs::read_to_string(&out).unwrap();
    assert!(content.contains("import ctypes"));
    assert!(content.contains("class Vec3f"));
}

#[test]
fn test_compile_cpp() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run(&[
        "compile",
        game_bridge_path().to_str().unwrap(),
        "--lang",
        "cpp",
        "--out-dir",
        tmp.path().to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("wrote"), "stdout: {stdout}");

    let out = tmp.path().join("game.hpp");
    assert!(out.exists(), "expected game.hpp in output dir");

    let content = fs::read_to_string(&out).unwrap();
    assert!(content.contains("#pragma once"));
    assert!(content.contains("struct Vec3f"));
}

#[test]
fn test_compile_multiple_langs() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run(&[
        "compile",
        game_bridge_path().to_str().unwrap(),
        "--lang",
        "rust",
        "--lang",
        "python",
        "--lang",
        "cpp",
        "--out-dir",
        tmp.path().to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");

    assert!(tmp.path().join("game.rs").exists());
    assert!(tmp.path().join("game.py").exists());
    assert!(tmp.path().join("game.hpp").exists());
    // Should mention all three files
    assert_eq!(stdout.matches("wrote").count(), 3);
}

#[test]
fn test_compile_default_out_dir() {
    // When --out-dir is omitted, defaults to "."
    let tmp = tempfile::tempdir().unwrap();
    let bin = synapse_bin();
    let output = Command::new(&bin)
        .args([
            "compile",
            game_bridge_path().to_str().unwrap(),
            "--lang",
            "rust",
        ])
        .current_dir(tmp.path())
        .output()
        .expect("failed to execute synapse");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(tmp.path().join("game.rs").exists());
}

// ============================================================
// Error cases
// ============================================================

#[test]
fn test_error_missing_file() {
    let (code, _stdout, stderr) = run(&["compile", "nonexistent.bridge", "--lang", "rust"]);
    assert_ne!(code, 0);
    assert!(stderr.contains("cannot read"), "stderr: {stderr}");
}

#[test]
fn test_error_unknown_language() {
    let (code, _stdout, stderr) = run(&[
        "compile",
        game_bridge_path().to_str().unwrap(),
        "--lang",
        "java",
    ]);
    assert_ne!(code, 0);
    assert!(stderr.contains("unknown language"), "stderr: {stderr}");
}

#[test]
fn test_error_parse_error() {
    let tmp = tempfile::tempdir().unwrap();
    let bad_file = tmp.path().join("bad.bridge");
    fs::write(&bad_file, "struct { invalid }").unwrap();

    let (code, _stdout, stderr) = run(&["compile", bad_file.to_str().unwrap(), "--lang", "rust"]);
    assert_ne!(code, 0);
    assert!(!stderr.is_empty(), "expected error message on stderr");
}

#[test]
fn test_error_no_args() {
    let bin = synapse_bin();
    let output = Command::new(&bin)
        .output()
        .expect("failed to execute synapse");
    // clap should exit with non-zero when no subcommand given
    assert!(!output.status.success());
}

#[test]
fn test_error_missing_lang_flag() {
    let (code, _stdout, stderr) = run(&["compile", game_bridge_path().to_str().unwrap()]);
    assert_ne!(code, 0);
    // clap should complain about missing --lang
    assert!(
        stderr.contains("--lang") || stderr.contains("required"),
        "stderr: {stderr}"
    );
}
