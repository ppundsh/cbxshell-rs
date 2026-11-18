#Requires -Version 5.1
<#
.SYNOPSIS
    簽署安裝檔並產生自動更新用的 latest.json（Sign the installer and write latest.json）。

.DESCRIPTION
    1. 以 Tauri signer（minisign / Ed25519）簽署 dist\CBXShell-Setup.exe，產生 .sig
    2. 產生 dist\latest.json（與 Tauri 更新器相同格式），供管理工具的「檢查更新」讀取

    私鑰來源（與 WinNUT-Client-RS 相同）：
      TAURI_SIGNING_PRIVATE_KEY           私鑰內容，或私鑰檔案路徑
      TAURI_SIGNING_PRIVATE_KEY_PASSWORD  私鑰密碼（沒有密碼時設為空字串）
    對應的公鑰須放在 assets\updater.pub 並隨版本一起建置，詳見 doc\updates.zh-TW.md。

.PARAMETER Setup
    要簽署的安裝檔，預設 dist\CBXShell-Setup.exe。

.PARAMETER Tag
    發行標籤（例如 v2.0.2），預設取 GITHUB_REF_NAME，否則為 v<Cargo.toml 版本>。

.PARAMETER Notes
    寫入 latest.json 的版本說明。

.EXAMPLE
    $env:TAURI_SIGNING_PRIVATE_KEY = "$HOME\.tauri\cbxshell.key"
    $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = ''
    powershell -ExecutionPolicy Bypass -File release-manifest.ps1
#>
param(
    [string]$Setup = 'dist\CBXShell-Setup.exe',
    [string]$Tag = $env:GITHUB_REF_NAME,
    [string]$Notes = '',
    [string]$Repo = 'ppundsh/cbxshell-rs'
)

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location -LiteralPath $Root

if (-not (Test-Path -LiteralPath $Setup)) { throw "找不到 $Setup，請先執行 build.ps1" }

$cargo = Get-Content -LiteralPath 'Cargo.toml' -Raw
if ($cargo -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"') {
    throw '無法從 Cargo.toml 的 [workspace.package] 讀取版本'
}
$version = $Matches[1]
if (-not $Tag) { $Tag = "v$version" }
if ($Tag.TrimStart('v') -ne $version) { throw "標籤 $Tag 與 Cargo.toml 版本 $version 不一致" }

$key = $env:TAURI_SIGNING_PRIVATE_KEY
if (-not $key) { throw '缺少 TAURI_SIGNING_PRIVATE_KEY（私鑰內容或私鑰檔案路徑）' }
if ($null -eq $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD) { $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = '' }

$sig = "$Setup.sig"
Remove-Item -LiteralPath $sig -ErrorAction SilentlyContinue

# The CLI reads the key / password from the environment, so neither appears on a command line.
$savedKey = $env:TAURI_SIGNING_PRIVATE_KEY
try {
    if (Test-Path -LiteralPath $key -PathType Leaf) {
        $env:TAURI_SIGNING_PRIVATE_KEY_PATH = (Resolve-Path -LiteralPath $key).Path
        Remove-Item Env:\TAURI_SIGNING_PRIVATE_KEY
    }
    & npx --yes '@tauri-apps/cli@2' signer sign $Setup
    if ($LASTEXITCODE -ne 0) { throw "tauri signer sign 失敗（exit $LASTEXITCODE）" }
} finally {
    $env:TAURI_SIGNING_PRIVATE_KEY = $savedKey
    Remove-Item Env:\TAURI_SIGNING_PRIVATE_KEY_PATH -ErrorAction SilentlyContinue
}
if (-not (Test-Path -LiteralPath $sig)) { throw "沒有產生 $sig" }

$signature = (Get-Content -LiteralPath $sig -Raw).Trim()
$asset = Split-Path -Leaf $Setup
$manifest = [ordered]@{
    version   = $version
    notes     = $Notes
    pub_date  = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
    platforms = [ordered]@{
        'windows-x86_64' = [ordered]@{
            signature = $signature
            url       = "https://github.com/$Repo/releases/download/$Tag/$asset"
        }
    }
}
$out = Join-Path (Split-Path -Parent (Resolve-Path -LiteralPath $Setup).Path) 'latest.json'
[IO.File]::WriteAllText($out, ($manifest | ConvertTo-Json -Depth 5), (New-Object System.Text.UTF8Encoding($false)))
Write-Host "已產生 $out 與 $sig" -ForegroundColor Green
