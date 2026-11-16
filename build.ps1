#Requires -Version 5.1
<#
.SYNOPSIS
    Builds CBX Shell (Release), runs the 14-point self-test suite (including 7-vector adversarial security tests), and packages CBXShell-Setup.exe.
.PARAMETER UseGnu
    Builds using the self-contained GNU toolchain (+stable-x86_64-pc-windows-gnu).
.PARAMETER Register
    Registers the built cbxshell.dll with regsvr32.
#>
param(
    [switch]$UseGnu,
    [switch]$Register
)

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path

Push-Location $Root
$savedRustFlags = $env:RUSTFLAGS
try {
    $toolchainArgs = @()
    $relDir = Join-Path $Root "target\release"
    $distDir = Join-Path $Root "dist"

    if ($UseGnu) {
        $installed = rustup toolchain list
        if ($installed -notmatch "x86_64-pc-windows-gnu") {
            Write-Host "==> Installing GNU toolchain..." -ForegroundColor Yellow
            rustup toolchain install stable-x86_64-pc-windows-gnu --profile minimal
            if ($LASTEXITCODE -ne 0) { throw "Failed to install stable-x86_64-pc-windows-gnu" }
        }
        $toolchainArgs = @("+stable-x86_64-pc-windows-gnu")
    }

    Stop-Process -Name "cbx-cli", "cbx-manager", "cbx-installer", "CBXShell-Setup" -Force -ErrorAction SilentlyContinue

    # ---- Privacy: strip build-machine paths from the binaries -------------------------------
    # rustc embeds absolute source paths (panic locations, debug info) for every crate, e.g.
    # "<USERPROFILE>\.cargo\registry\src\...". That leaks the builder's user name and folder
    # layout into every released binary. Remap them to neutral prefixes. When several remaps
    # match, rustc applies the LAST one, so the most specific prefixes come last.
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" }
    $rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE ".rustup" }
    $remap = @()
    if ($env:USERPROFILE) { $remap += "--remap-path-prefix=$($env:USERPROFILE)=~" }
    $remap += "--remap-path-prefix=$rustupHome=/rustup"
    $remap += "--remap-path-prefix=$cargoHome=/cargo"
    $remap += "--remap-path-prefix=$Root=/src"
    $env:RUSTFLAGS = (@($env:RUSTFLAGS) + $remap | Where-Object { $_ }) -join ' '

    Write-Host "==> Building core binaries (cbxshell.dll, cbx-manager.exe, cbx-cli.exe)..." -ForegroundColor Cyan
    cargo @toolchainArgs build --release -p cbxshell -p cbx-manager -p cbx-cli
    if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed ($LASTEXITCODE)" }

    Write-Host "==> Running 20-point self-test suite ($relDir\cbx-cli.exe --selftest)..." -ForegroundColor Cyan
    & (Join-Path $relDir "cbx-cli.exe") --selftest
    if ($LASTEXITCODE -ne 0) { throw "cbx-cli --selftest failed ($LASTEXITCODE)" }

    Write-Host "==> Packaging standalone installer (CBXShell-Setup.exe)..." -ForegroundColor Cyan
    $env:CBX_PAYLOAD_DIR = $relDir
    $env:CBX_PACK_STAMP = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds().ToString()
    cargo @toolchainArgs build --release -p cbx-installer
    if ($LASTEXITCODE -ne 0) { throw "cbx-installer build failed ($LASTEXITCODE)" }

    New-Item -ItemType Directory -Force -Path $distDir | Out-Null
    $setupSrc = Join-Path $relDir "cbx-installer.exe"
    $setupDst = Join-Path $distDir "CBXShell-Setup.exe"
    Copy-Item -Force $setupSrc $setupDst

    # ---- Privacy gate: refuse to ship binaries that still contain build-machine paths ---------
    # Scans ASCII and UTF-16LE strings for the builder's profile path, user name and repo root.
    Write-Host "==> Scanning release binaries for embedded build-machine paths..." -ForegroundColor Cyan
    $needles = @()
    if ($env:USERPROFILE) { $needles += $env:USERPROFILE }
    if ($env:USERNAME -and $env:USERNAME.Length -ge 3) { $needles += "\Users\$($env:USERNAME)\" }
    $needles += $Root
    $bins = @(
        (Join-Path $relDir "cbxshell.dll"),
        (Join-Path $relDir "cbx-manager.exe"),
        (Join-Path $relDir "cbx-cli.exe"),
        $setupDst
    ) | Where-Object { Test-Path $_ }
    $leaks = @()
    foreach ($bin in $bins) {
        $bytes = [System.IO.File]::ReadAllBytes($bin)
        $ascii = [System.Text.Encoding]::GetEncoding(28591).GetString($bytes)
        $utf16 = [System.Text.Encoding]::Unicode.GetString($bytes)
        foreach ($n in $needles) {
            if ($ascii.IndexOf($n, [StringComparison]::OrdinalIgnoreCase) -ge 0 -or
                $utf16.IndexOf($n, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
                $leaks += "$(Split-Path -Leaf $bin): contains a build-machine path"
            }
        }
    }
    if ($leaks.Count -gt 0) {
        $leaks | Sort-Object -Unique | ForEach-Object { Write-Host "    $_" -ForegroundColor Red }
        throw "Privacy gate failed: release binaries embed build-machine paths."
    }

    Write-Host "`n==> Build artifacts & installer:" -ForegroundColor Green
    Get-Item (Join-Path $relDir "cbxshell.dll"), (Join-Path $relDir "cbx-manager.exe"), (Join-Path $relDir "cbx-cli.exe"), $setupDst |
        Select-Object Name, DirectoryName, Length, LastWriteTime | Format-Table -AutoSize

    if ($Register) {
        $dll = Join-Path $relDir "cbxshell.dll"
        Write-Host "==> Registering $dll..." -ForegroundColor Cyan
        Start-Process regsvr32.exe -ArgumentList "/s `"$dll`"" -Verb RunAs -Wait
        Write-Host "==> Registered successfully." -ForegroundColor Green
    }
}
finally {
    $env:RUSTFLAGS = $savedRustFlags
    Pop-Location
}
