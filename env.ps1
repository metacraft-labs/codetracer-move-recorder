# codetracer-move-recorder Windows dev environment (PowerShell)
# Usage: . .\env.ps1
#
# The recorder builds and tests with a plain `cargo build` / `cargo test`.
# Its Windows requirements are:
#
#   1. The shared CodeTracer toolchain (Rust, Nim + nimble, just, Cap'n Proto,
#      MSVC).  These are provisioned by the main `codetracer` repo's env.ps1,
#      which this script dot-sources.  Nim is needed because the
#      `codetracer_trace_writer_nim` crate's build script compiles a Nim
#      static library.
#
#   2. An explicit MSVC linker for the `x86_64-pc-windows-msvc` target -- see
#      the comment block below `WINDOWS_DIY_CL_EXE`.
#
#   3. The Sui CLI.  `tests/test_sui_integration.rs` drives a real
#      `sui move test --trace full` against the dedicated
#      `test-programs/move/sui_flow_test` package and converts the emitted
#      Move trace v3 NDJSON through the recorder.  The recorder's
#      `move_types::TraceEvent` parser targets the externally-tagged v3
#      trace events emitted by Sui >= 1.68, so a recent release is pinned;
#      it is the official prebuilt `sui-*-windows-x86_64.tgz` from
#      `MystenLabs/sui`.
#      (The Aptos data-adapter tests parse committed fixtures and do not
#      shell out to an `aptos` binary, so none is provisioned here.)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition

# --- 1. Shared CodeTracer toolchain -----------------------------------------
$env:WINDOWS_DIY_SKIP_FPC = "1"
$env:WINDOWS_DIY_SKIP_LLVM = "1"
$env:WINDOWS_DIY_SKIP_NARGO = "1"
$env:WINDOWS_DIY_SKIP_DOTNET = "1"

$codetracerEnv = Join-Path (Split-Path -Parent $scriptDir) "codetracer\env.ps1"
if (-not (Test-Path $codetracerEnv)) {
    throw "Could not find the shared CodeTracer env.ps1 at $codetracerEnv -- the ``codetracer`` repo must be checked out as a sibling of this repo."
}
. $codetracerEnv

# --- 2. Explicit MSVC linker (immune to Git Bash PATH reordering) -----------
# The `just test` recipe runs `verify-cli-convention-no-silent-skip.sh` via
# bash, which invokes `cargo build`.  A bash login shell re-orders PATH so
# Git Bash's coreutils `link.exe` precedes the MSVC toolchain; pinning the
# linker by absolute path bypasses PATH resolution.
if ($env:WINDOWS_DIY_CL_EXE -and (Test-Path $env:WINDOWS_DIY_CL_EXE)) {
    $msvcBin = Split-Path -Parent $env:WINDOWS_DIY_CL_EXE
    $msvcLink = Join-Path $msvcBin "link.exe"
    if (Test-Path $msvcLink) {
        $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = $msvcLink
    }
    if ($env:Path -notlike "$msvcBin;*") {
        $env:Path = "$msvcBin;$($env:Path)"
    }
}

# --- 3. Sui CLI -------------------------------------------------------------
$devDepsRoot = if ($env:WINDOWS_DIY_INSTALL_ROOT) { $env:WINDOWS_DIY_INSTALL_ROOT }
               elseif (Test-Path "D:\") { "D:\metacraft-dev-deps" }
               else { Join-Path $env:LOCALAPPDATA "codetracer\windows-diy" }

$suiVersion = "1.72.2"
$suiDir = Join-Path $devDepsRoot "sui\$suiVersion"
$suiExe = Join-Path $suiDir "sui.exe"
if (Test-Path $suiExe) {
    Write-Host "Sui CLI $suiVersion already installed"
} else {
    Write-Host "Installing Sui CLI $suiVersion..."
    New-Item -ItemType Directory -Force -Path $suiDir | Out-Null
    $suiTgz = Join-Path $env:TEMP "sui-$suiVersion.tgz"
    $suiUrl = "https://github.com/MystenLabs/sui/releases/download/mainnet-v$suiVersion/sui-mainnet-v$suiVersion-windows-x86_64.tgz"
    Invoke-WebRequest -Uri $suiUrl -OutFile $suiTgz
    # `tar` (bsdtar) ships with Windows 10+.
    & tar -xzf $suiTgz -C $suiDir
    if ($LASTEXITCODE -ne 0) { throw "Failed to extract the Sui CLI archive" }
    Remove-Item $suiTgz -Force -ErrorAction SilentlyContinue
    if (-not (Test-Path $suiExe)) { throw "sui.exe missing after extraction" }
    Write-Host "Installed Sui CLI to $suiDir"
}
if ($env:Path -notlike "*$suiDir*") {
    $env:Path = "$suiDir;$($env:Path)"
}

Write-Host "sui: $((& sui --version) 2>&1)"
Write-Host "codetracer-move-recorder dev environment ready."
