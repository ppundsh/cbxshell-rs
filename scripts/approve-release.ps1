#Requires -Version 5.1
<#
.SYNOPSIS
    用 YubiKey 核准並發布 GitHub Actions 建好的草稿版本（Approve a draft release with the YubiKey）。

.DESCRIPTION
    CI 只會建立「草稿」Release（安裝檔＋minisign .sig＋latest.json），使用者看不到。
    這個腳本：
      1. 以 git verify-tag 確認標籤是你用 YubiKey 簽的（GnuPG 或 FIDO2 SSH，依 git 的 gpg.format）
      2. 下載草稿裡的 CBXShell-Setup.exe 與 latest.json，核對版本與網址，顯示 SHA-256
      3. 你確認後，用 YubiKey 對「版本＋安裝檔 SHA-256」簽章並在本機先驗證一次：
         GnuPG（OpenPGP 卡片）→ gpg --detach-sign --armor
         FIDO2 SSH 金鑰     → ssh-keygen -Y sign -n cbxshell-release
      4. 把核准簽章寫進 latest.json 的 approval 欄位、上傳，然後發布草稿

    管理工具的更新器同時要求 CI 的 minisign 簽章與這份 YubiKey 核准，
    因此即使 GitHub 帳號、CI 或簽章私鑰外洩，也無法推送更新給使用者。

.PARAMETER Tag
    要核准的標籤，例如 v2.0.2。

.PARAMETER KeyName
    YubiKey 金鑰檔名（%USERPROFILE%\.ssh\），預設 id_ed25519_sk_github。

.PARAMETER NoPublish
    只上傳核准後的 latest.json，不把草稿改為正式發布。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\approve-release.ps1 -Tag v2.0.2
#>
param(
    [Parameter(Mandatory = $true)][string]$Tag,
    [string]$KeyName = 'id_ed25519_sk_github',
    [string]$Repo = 'ppundsh/cbxshell-rs',
    [switch]$NoPublish,
    # auto: follow git's gpg.format (openpgp → GnuPG key on the YubiKey, ssh → FIDO2 SSH key)
    [ValidateSet('auto', 'gpg', 'ssh')][string]$Method = 'auto'
)

$ErrorActionPreference = 'Continue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$Namespace = 'cbxshell-release'
$Asset = 'CBXShell-Setup.exe'

function Fail([string]$msg) {
    Write-Host "`n[錯誤] $msg" -ForegroundColor Red
    exit 1
}
function Step([string]$msg) { Write-Host "`n==> $msg" -ForegroundColor Cyan }

if ($Tag -notmatch '^v(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$') { Fail "標籤格式應為 vX.Y.Z：$Tag" }
$Version = $Matches[1]

# ---------------------------------------------------------------------------------------
Step '檢查工具'
if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { Fail '找不到 GitHub CLI（winget install GitHub.cli，然後 gh auth login）' }
if (-not (Get-Command git -ErrorAction SilentlyContinue)) { Fail '找不到 git' }
if ($Method -eq 'auto') {
    $fmt = ((& git -C $Root config gpg.format) -join '').Trim()
    $Method = if ($fmt -eq 'ssh') { 'ssh' } else { 'gpg' }
}
$approvers = Join-Path $Root 'assets\release-approvers.pub'

if ($Method -eq 'gpg') {
    $Gpg = ((& git -C $Root config gpg.program) -join '').Trim()
    if (-not $Gpg -or -not (Test-Path -LiteralPath $Gpg)) {
        $Gpg = @("${env:ProgramFiles(x86)}\GnuPG\bin\gpg.exe", "$env:ProgramFiles\GnuPG\bin\gpg.exe") |
            Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    }
    if (-not $Gpg) { Fail '找不到 GnuPG，請先執行 scripts\setup-gpg-signing.ps1' }
    $GpgKey = ((& git -C $Root config user.signingkey) -join '').Trim().TrimEnd('!')
    if (-not $GpgKey) { Fail '沒有設定 user.signingkey，請先執行 scripts\setup-gpg-signing.ps1' }
    $ssh = ((& $Gpg --export-ssh-key "$GpgKey!" 2>$null) -join '').Trim()
    if ($ssh -notmatch '^ssh-ed25519 (\S+)') { Fail "無法匯出 $GpgKey 的公開金鑰值" }
    $keyBody = "openpgp ssh-ed25519 $($Matches[1])"
    $signHint = '以 YubiKey 簽署核准（請輸入 OpenPGP PIN；若有設觸碰原則請觸碰）'
} else {
    $SshKeygen = Join-Path $env:WINDIR 'System32\OpenSSH\ssh-keygen.exe'
    if (-not (Test-Path $SshKeygen)) { Fail '找不到 Windows OpenSSH（ssh-keygen.exe）' }
    $KeyPath = Join-Path (Join-Path $env:USERPROFILE '.ssh') $KeyName
    if (-not (Test-Path "$KeyPath.pub")) {
        # Fall back to the key git signs with (set by scripts\setup-yubikey.ps1).
        $cfgKey = ((& git -C $Root config user.signingkey) -join '').Trim() -replace '\.pub$', ''
        if ($cfgKey -and (Test-Path -LiteralPath "$cfgKey.pub")) { $KeyPath = $cfgKey }
    }
    if (-not (Test-Path "$KeyPath.pub")) { Fail "找不到 $KeyPath.pub，請先執行 scripts\setup-yubikey.ps1" }
    $pub = (Get-Content -LiteralPath "$KeyPath.pub" -Raw).Trim()
    if ($pub -notmatch '^sk-ssh-ed25519@openssh\.com ') { Fail '核准金鑰必須是 YubiKey 上的 sk-ssh-ed25519 金鑰' }
    $keyBody = ($pub -split ' ')[0..1] -join ' '
    $signHint = '以 YubiKey 簽署核准（請輸入 FIDO2 PIN 並觸碰金鑰）'
}
Write-Host "    核准方式：$Method"

if (-not (Select-String -LiteralPath $approvers -SimpleMatch $keyBody -Quiet)) {
    Fail "這把金鑰不在 assets\release-approvers.pub；已發行的版本不會接受它的核准。"
}

# ---------------------------------------------------------------------------------------
Step "確認標籤 $Tag 有 YubiKey 簽章"
& git -C $Root fetch --quiet origin "refs/tags/${Tag}:refs/tags/$Tag" 2>$null
& git -C $Root verify-tag $Tag
if ($LASTEXITCODE -ne 0) { Fail "git verify-tag $Tag 失敗：標籤未簽章或不是允許的金鑰" }

# ---------------------------------------------------------------------------------------
Step "下載草稿 Release $Tag"
$work = Join-Path $env:TEMP "cbxshell-approve-$Version"
if (Test-Path $work) { Remove-Item -LiteralPath $work -Recurse -Force }
New-Item -ItemType Directory -Path $work | Out-Null
& gh release download $Tag -R $Repo -p $Asset -p 'latest.json' -D $work
if ($LASTEXITCODE -ne 0) { Fail 'gh release download 失敗' }
$setup = Join-Path $work $Asset
$manifestPath = Join-Path $work 'latest.json'
if (-not (Test-Path $setup) -or -not (Test-Path $manifestPath)) { Fail '草稿缺少安裝檔或 latest.json' }

$manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
$platform = $manifest.platforms.'windows-x86_64'
if ($manifest.version -ne $Version) { Fail "latest.json 版本 $($manifest.version) 與標籤 $Tag 不符" }
$expectedUrl = "https://github.com/$Repo/releases/download/$Tag/$Asset"
if ($platform.url -ne $expectedUrl) { Fail "latest.json 網址不符：$($platform.url)" }
if (-not $platform.signature) { Fail 'latest.json 沒有 minisign 簽章' }

$sha = (Get-FileHash -Algorithm SHA256 -LiteralPath $setup).Hash.ToLowerInvariant()
$size = (Get-Item -LiteralPath $setup).Length
$commit = (& git -C $Root rev-list -n 1 $Tag) -join ''
Write-Host ''
Write-Host "    版本      $Version"
Write-Host "    標籤提交  $commit"
Write-Host "    安裝檔    $Asset（$size 位元組）"
Write-Host "    SHA-256   $sha" -ForegroundColor Yellow
Write-Host ''
$answer = Read-Host '確認要核准並發布這個版本？輸入 yes 繼續'
if ($answer -ne 'yes') { Fail '已取消' }

# ---------------------------------------------------------------------------------------
Step $signHint
$statement = "$Namespace`nversion=$Version`nsha256=$sha`n"
$stmtPath = Join-Path $work 'approval.txt'
$utf8 = New-Object System.Text.UTF8Encoding($false)
[IO.File]::WriteAllText($stmtPath, $statement, $utf8)
if ($Method -eq 'gpg') {
    # Binary-document signature (no --textmode): the updater hashes the exact bytes.
    & $Gpg --batch --yes --armor --detach-sign --digest-algo SHA512 --local-user "$GpgKey!" -o "$stmtPath.asc" $stmtPath
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path "$stmtPath.asc")) { Fail 'gpg --detach-sign 失敗' }
    & $Gpg --verify "$stmtPath.asc" $stmtPath
    if ($LASTEXITCODE -ne 0) { Fail '本機驗證核准簽章失敗' }
    $armored = (Get-Content -LiteralPath "$stmtPath.asc" -Raw).Trim()
} else {
    & $SshKeygen -Y sign -f $KeyPath -n $Namespace $stmtPath
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path "$stmtPath.sig")) { Fail 'ssh-keygen -Y sign 失敗' }

    # Verify exactly as the updater will: this key, this namespace, PIN-verified signature.
    $allowed = Join-Path $work 'allowed_signers'
    [IO.File]::WriteAllText($allowed, "approver namespaces=`"$Namespace`",verify-required $keyBody`n", $utf8)
    & cmd /d /c "`"$SshKeygen`" -Y verify -f `"$allowed`" -I approver -n $Namespace -s `"$stmtPath.sig`" < `"$stmtPath`""
    if ($LASTEXITCODE -ne 0) { Fail '本機驗證核准簽章失敗' }
    $armored = (Get-Content -LiteralPath "$stmtPath.sig" -Raw).Trim()
}

$platform | Add-Member -NotePropertyName approval -NotePropertyValue $armored -Force
[IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 6), $utf8)

# ---------------------------------------------------------------------------------------
Step '上傳核准後的 latest.json'
& gh release upload $Tag $manifestPath -R $Repo --clobber
if ($LASTEXITCODE -ne 0) { Fail 'gh release upload 失敗' }

if ($NoPublish) {
    Write-Host "`n已上傳核准；草稿尚未發布（-NoPublish）。" -ForegroundColor Green
} else {
    Step '發布 Release'
    & gh release edit $Tag -R $Repo --draft=false --latest
    if ($LASTEXITCODE -ne 0) { Fail 'gh release edit 失敗' }
    Write-Host "`n$Tag 已核准並發布。" -ForegroundColor Green
}
