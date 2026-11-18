#Requires -Version 5.1
<#
.SYNOPSIS
    Bumps the CBX Shell version in ONE place and (optionally) commits, tags and pushes a release.

.DESCRIPTION
    Single source of truth: `[workspace.package] version` in Cargo.toml.
    Everything else derives from it at build time:
      * cbx-manager sidebar / About page   -> env!("CARGO_PKG_VERSION")
      * Windows file/product version (PE)  -> build_res.rs reads CARGO_PKG_VERSION_*
      * installer "DisplayVersion"         -> env!("CARGO_PKG_VERSION")
    This script rewrites Cargo.toml, refreshes the workspace entries in Cargo.lock, and can
    create the matching signed `vX.Y.Z` git tag (YubiKey). Pushing that tag triggers
    .github/workflows/release.yml, which builds a draft release; publish it with
    scripts\approve-release.ps1 -Tag vX.Y.Z.

.EXAMPLE
    .\bump-version.ps1                 # 2.0.1 -> 2.0.2 (patch), files only
    .\bump-version.ps1 minor -Tag      # 2.0.2 -> 2.1.0, commit + annotated tag v2.1.0
    .\bump-version.ps1 2.1.0 -Tag -Push
    .\bump-version.ps1 -Tag -Push -NoBump   # tag/push the version already in Cargo.toml
#>
param(
    [string]$Part = 'patch',   # patch | minor | major | explicit X.Y.Z
    [switch]$Tag,              # commit the bump and create annotated tag vX.Y.Z
    [switch]$Push,             # push the commit and the tag to origin (implies -Tag)
    [switch]$NoBump            # keep the current version (e.g. first release after a manual edit)
)
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $Root
if ($Push) { $Tag = $true }

function Fail($msg) { Write-Host "[ABORTED] $msg" -ForegroundColor Red; exit 1 }
function Invoke-Git { & git.exe @args; if ($LASTEXITCODE -ne 0) { Fail "git $($args -join ' ') failed ($LASTEXITCODE)" } }

$cargoToml = Join-Path $Root 'Cargo.toml'
$toml = [System.IO.File]::ReadAllText($cargoToml)
$section = [regex]::Match($toml, '(?ms)^\[workspace\.package\]\s*$(.*?)(?=^\[|\z)')
if (-not $section.Success) { Fail '[workspace.package] section not found in Cargo.toml' }
$verMatch = [regex]::Match($section.Groups[1].Value, '(?m)^version\s*=\s*"(\d+)\.(\d+)\.(\d+)"')
if (-not $verMatch.Success) { Fail 'version = "X.Y.Z" not found in [workspace.package]' }
$old = '{0}.{1}.{2}' -f $verMatch.Groups[1].Value, $verMatch.Groups[2].Value, $verMatch.Groups[3].Value
$maj = [int]$verMatch.Groups[1].Value; $min = [int]$verMatch.Groups[2].Value; $pat = [int]$verMatch.Groups[3].Value

if ($NoBump) {
    $new = $old
} else {
    switch -Regex ($Part) {
        '^patch$' { $pat++ }
        '^minor$' { $min++; $pat = 0 }
        '^major$' { $maj++; $min = 0; $pat = 0 }
        '^\d+\.\d+\.\d+$' { $p = $Part.Split('.'); $maj = [int]$p[0]; $min = [int]$p[1]; $pat = [int]$p[2] }
        default { Fail "Unknown part '$Part' (use patch, minor, major or X.Y.Z)" }
    }
    $new = "$maj.$min.$pat"
    if ([version]$new -le [version]$old) { Fail "New version $new must be greater than $old" }
}

if ($Tag) {
    $dirty = & git.exe status --porcelain --untracked-files=no
    if ($dirty) { Fail "Commit or stash your changes first - the release commit must contain only the version bump.`n$dirty" }
    if (& git.exe tag -l "v$new") { Fail "Tag v$new already exists." }
}

if ($new -ne $old) {
    # 1. Cargo.toml: replace only inside [workspace.package]
    $start = $section.Groups[1].Index
    $body = $section.Groups[1].Value
    $newBody = [regex]::Replace($body, '(?m)^(version\s*=\s*")\d+\.\d+\.\d+(")', "`${1}$new`${2}", 1)
    $toml = $toml.Substring(0, $start) + $newBody + $toml.Substring($start + $body.Length)
    [System.IO.File]::WriteAllText($cargoToml, $toml, (New-Object System.Text.UTF8Encoding($false)))

    # 2. Cargo.lock: workspace members only (no dependency upgrades, works offline)
    $lockPath = Join-Path $Root 'Cargo.lock'
    if (Test-Path $lockPath) {
        $meta = & cargo metadata --no-deps --format-version 1 --offline | ConvertFrom-Json
        $lock = [System.IO.File]::ReadAllText($lockPath)
        foreach ($pkg in $meta.packages) {
            $name = [regex]::Escape($pkg.name)
            $lock = [regex]::Replace($lock, "(?m)(^name = `"$name`"\r?\nversion = `")[^`"]+(`")", "`${1}$new`${2}")
        }
        [System.IO.File]::WriteAllText($lockPath, $lock, (New-Object System.Text.UTF8Encoding($false)))
    }
    Write-Host "==> Version $old -> $new" -ForegroundColor Green
} else {
    Write-Host "==> Keeping version $new" -ForegroundColor Green
}

if ($Tag) {
    if ($new -ne $old) {
        Invoke-Git add Cargo.toml Cargo.lock
        # -S: signed with the YubiKey (user.signingkey); needs the PIN and a touch.
        Invoke-Git commit -S -q -m "chore(release): v$new"
    }
    # -s: signed annotated tag. CI refuses to build tags that are not signed by a release approver.
    Invoke-Git tag -s "v$new" -m "CBX Shell v$new"
    Invoke-Git verify-tag "v$new"
    Write-Host "==> Tagged v$new (signed)" -ForegroundColor Green
}
if ($Push) {
    $branch = (& git.exe rev-parse --abbrev-ref HEAD).Trim()
    Invoke-Git push origin $branch
    Invoke-Git push origin "v$new"
    Write-Host "==> Pushed $branch and v$new (GitHub Actions will build the release)" -ForegroundColor Green
}
