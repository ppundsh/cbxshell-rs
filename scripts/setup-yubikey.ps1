#Requires -Version 5.1
<#
.SYNOPSIS
    用 YubiKey（FIDO2）保護這個 repo 的 push 與 commit / tag 簽章。

.DESCRIPTION
    1. 用 Windows 內建 OpenSSH 在 YubiKey 上產生 ed25519-sk 金鑰（私鑰不離開 YubiKey，
       每次使用都要輸入 FIDO2 PIN 並觸碰）。已存在則沿用。
    2. 設定 git：commit / tag 一律用這把金鑰簽章（gpg.format=ssh）。
    3. push 走 SSH 並指定這把金鑰（core.sshCommand），把 origin 改成 SSH 網址。
    4. 啟用 .githooks/pre-push：未簽章或簽章不在允許清單的 commit / tag 一律拒絕推送。
    5. 公鑰複製到剪貼簿，接著到 GitHub 加成「Authentication」與「Signing」兩種金鑰。
    6. 把公鑰加入 assets\release-approvers.pub：之後建置的 CBX Shell 只接受附有這把
       YubiKey 核准簽章（scripts\approve-release.ps1）的更新。

    預設只設定這個 repo（--local）。加 -Global 則簽章設定套用到所有 repo
    （pre-push hook 與 origin 網址仍只改這個 repo）。

.PARAMETER Global
    commit / tag 簽章與 sshCommand 寫入全域設定（所有 repo 共用）。

.PARAMETER KeyName
    金鑰檔名（存在 %USERPROFILE%\.ssh\），預設 id_ed25519_sk_github。

.PARAMETER KeepRemote
    不要把 origin 從 https 改成 SSH。

.PARAMETER Application
    YubiKey 上常駐金鑰的槽位名稱，預設 ssh:github。若這個槽位已有金鑰、但這台電腦找不到
    它的金鑰檔（Windows 無法用 ssh-keygen -K 取回），可改用新槽位建立另一把，例如：
    -Application ssh:cbxshell -KeyName id_ed25519_sk_cbxshell

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\setup-yubikey.ps1
#>
param(
    [switch]$Global,
    [string]$KeyName = 'id_ed25519_sk_github',
    [switch]$KeepRemote,
    [string]$Application = 'ssh:github'
)

# Native tools report failures through exit codes; checked explicitly below.
$ErrorActionPreference = 'Continue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)

function Fail([string]$msg) {
    Write-Host "`n[錯誤] $msg" -ForegroundColor Red
    exit 1
}
function Step([string]$msg) { Write-Host "`n==> $msg" -ForegroundColor Cyan }

# Reads the FIDO "application" (e.g. ssh:github) out of an OpenSSH sk public-key line.
function Get-SkApplication([string]$pubLine) {
    $parts = $pubLine.Trim() -split '\s+'
    if ($parts.Count -lt 2 -or $parts[0] -notlike 'sk-*') { return $null }
    try { $b = [Convert]::FromBase64String($parts[1]) } catch { return $null }
    $pos = 0
    $strings = @()
    while ($pos + 4 -le $b.Length -and $strings.Count -lt 4) {
        $n = ([int]$b[$pos] -shl 24) -bor ([int]$b[$pos + 1] -shl 16) -bor ([int]$b[$pos + 2] -shl 8) -bor [int]$b[$pos + 3]
        $pos += 4
        if ($n -lt 0 -or $pos + $n -gt $b.Length) { return $null }
        $strings += , ($b[$pos..($pos + $n - 1)])
        $pos += $n
    }
    # ed25519-sk: type, key, application; ecdsa-sk: type, curve, point, application
    $type = [Text.Encoding]::ASCII.GetString($strings[0])
    $idx = if ($type -like 'sk-ecdsa-*') { 3 } else { 2 }
    if ($strings.Count -le $idx) { return $null }
    return [Text.Encoding]::UTF8.GetString($strings[$idx])
}
function Git-Config([string]$scope, [string]$key, [string]$value) {
    & git -C $Root config $scope $key $value
    if ($LASTEXITCODE -ne 0) { Fail "git config $scope $key 失敗" }
    Write-Host "    git config $scope $key = $value" -ForegroundColor DarkGray
}

# ---------------------------------------------------------------------------------------
Step '檢查 git 與 Windows OpenSSH'
if (-not (Get-Command git -ErrorAction SilentlyContinue)) { Fail '找不到 git（winget install Git.Git）' }
& git -C $Root rev-parse --is-inside-work-tree *> $null
if ($LASTEXITCODE -ne 0) { Fail "$Root 不是 git repo" }

# Windows' own OpenSSH talks to the YubiKey through Windows Hello (webauthn.dll).
$OpenSsh = Join-Path $env:WINDIR 'System32\OpenSSH'
$Ssh = Join-Path $OpenSsh 'ssh.exe'
$SshKeygen = Join-Path $OpenSsh 'ssh-keygen.exe'
if (-not (Test-Path $SshKeygen)) {
    Fail '找不到 Windows OpenSSH 用戶端。請到「設定 > 系統 > 選用功能」加入「OpenSSH 用戶端」。'
}
$verText = (& cmd /d /c "`"$Ssh`" -V 2>&1") -join ' '
Write-Host "    $verText"
if ($verText -match 'OpenSSH_for_Windows_(\d+)\.(\d+)') {
    $ver = [version]"$($Matches[1]).$($Matches[2])"
    if ($ver -lt [version]'8.9') { Fail "OpenSSH $ver 太舊；FIDO2 金鑰需要 8.9 以上（Windows 11 內建版本即可）。" }
} else {
    Write-Host '    無法判斷 OpenSSH 版本，繼續嘗試。' -ForegroundColor Yellow
}

# Forward slashes: git and OpenSSH accept them and they need no escaping.
$SshFwd = $Ssh -replace '\\', '/'
$SshKeygenFwd = $SshKeygen -replace '\\', '/'
$SshDir = Join-Path $env:USERPROFILE '.ssh'
$KeyPath = Join-Path $SshDir $KeyName
$PubPath = "$KeyPath.pub"
$KeyFwd = $KeyPath -replace '\\', '/'

# ---------------------------------------------------------------------------------------
Step '確認 git 使用者（GitHub 依 email 判斷簽章是否 Verified）'
$email = (& git -C $Root config user.email) -join ''
$name = (& git -C $Root config user.name) -join ''
if (-not $name) {
    $name = Read-Host '    git user.name（例如 GitHub 帳號名稱）'
    if (-not $name) { Fail '需要 user.name' }
    Git-Config '--global' 'user.name' $name
}
if (-not $email) {
    Write-Host '    建議使用 GitHub 已驗證的 email，或 GitHub 提供的 noreply 地址' -ForegroundColor DarkGray
    Write-Host '    （GitHub > Settings > Emails，形如 12345678+帳號@users.noreply.github.com）。' -ForegroundColor DarkGray
    $email = Read-Host '    git user.email'
    if (-not $email) { Fail '需要 user.email' }
    Git-Config '--global' 'user.email' $email
}
Write-Host "    $name <$email>"

# ---------------------------------------------------------------------------------------
Step "YubiKey 金鑰：$KeyPath"
# 1) Reuse whatever git is already configured to sign with, if it is a YubiKey (-sk) key.
if (-not (Test-Path $PubPath)) {
    $cfgKey = ((& git config --global user.signingkey) -join '').Trim()
    if ($cfgKey -and -not $cfgKey.EndsWith('.pub')) { $cfgKey = "$cfgKey.pub" }
    if ($cfgKey -and (Test-Path -LiteralPath $cfgKey) -and ((Get-Content -LiteralPath $cfgKey -Raw) -match '^sk-')) {
        $PubPath = (Resolve-Path -LiteralPath $cfgKey).Path
        $KeyPath = $PubPath.Substring(0, $PubPath.Length - 4)
        $KeyFwd = $KeyPath -replace '\\', '/'
        Write-Host "    沿用 git 已設定的簽章金鑰：$PubPath" -ForegroundColor Green
    }
}
# 2) A key file for the same YubiKey slot already in ~/.ssh under another name.
if (-not (Test-Path $PubPath) -and (Test-Path $SshDir)) {
    foreach ($f in Get-ChildItem -LiteralPath $SshDir -Filter '*.pub' -ErrorAction SilentlyContinue) {
        $line = (Get-Content -LiteralPath $f.FullName -Raw)
        if ((Get-SkApplication $line) -eq $Application -and (Test-Path -LiteralPath ($f.FullName.Substring(0, $f.FullName.Length - 4)))) {
            $PubPath = $f.FullName
            $KeyPath = $PubPath.Substring(0, $PubPath.Length - 4)
            $KeyFwd = $KeyPath -replace '\\', '/'
            Write-Host "    找到同一個 YubiKey 槽位（$Application）的金鑰檔：$PubPath" -ForegroundColor Green
            break
        }
    }
}
# 3) A resident key already on the YubiKey (e.g. created on another PC): download its handle
#    instead of trying to create a new one, which would ask to overwrite it.
if (-not (Test-Path $PubPath)) {
    Write-Host '    嘗試從 YubiKey 取回既有的常駐金鑰（ssh-keygen -K，需要 FIDO2 PIN）…' -ForegroundColor Yellow
    Write-Host '    詢問 passphrase 時可直接按 Enter。' -ForegroundColor DarkGray
    $rkDir = Join-Path $env:TEMP ('yubikey-rk-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $rkDir | Out-Null
    Push-Location -LiteralPath $rkDir
    try { & $SshKeygen -K } finally { Pop-Location }
    # (Windows' WebAuthn-based OpenSSH often cannot enumerate resident keys: "invalid format".)
    $rk = Get-ChildItem -LiteralPath $rkDir -Filter '*.pub' -ErrorAction SilentlyContinue |
        Where-Object { (Get-SkApplication (Get-Content -LiteralPath $_.FullName -Raw)) -eq $Application } |
        Select-Object -First 1
    if ($rk) {
        if (-not (Test-Path $SshDir)) { New-Item -ItemType Directory -Path $SshDir | Out-Null }
        Copy-Item -LiteralPath ($rk.FullName.Substring(0, $rk.FullName.Length - 4)) -Destination $KeyPath
        Copy-Item -LiteralPath $rk.FullName -Destination $PubPath
        Write-Host "    已取回 $($rk.Name) → $KeyPath" -ForegroundColor Green
    } else {
        Write-Host "    取不回 $Application 的常駐金鑰，將建立新的。" -ForegroundColor DarkGray
        Write-Host "    若接著詢問「Overwrite key in token」，代表 YubiKey 上已有這個槽位的金鑰：" -ForegroundColor Yellow
        Write-Host "      回答 n，再改用新槽位重跑：-Application ssh:cbxshell -KeyName id_ed25519_sk_cbxshell" -ForegroundColor Yellow
        Write-Host "      （回答 y 會覆寫舊金鑰，已加到 GitHub 的那把會失效。）" -ForegroundColor Yellow
    }
    Remove-Item -LiteralPath $rkDir -Recurse -Force -ErrorAction SilentlyContinue
}
if (Test-Path $PubPath) {
    Write-Host '    已存在，沿用這把金鑰。' -ForegroundColor Green
} else {
    if (-not (Test-Path $SshDir)) { New-Item -ItemType Directory -Path $SshDir | Out-Null }
    Write-Host '    請插入 YubiKey。接下來會要求輸入 FIDO2 PIN 並觸碰金鑰。' -ForegroundColor Yellow
    Write-Host '    之後詢問 passphrase 時可直接按 Enter（私鑰在 YubiKey 內，檔案只是索引）。' -ForegroundColor Yellow
    # resident: the key can be restored on another PC with `ssh-keygen -K`.
    # verify-required: every signature / login needs the PIN, not just a touch.
    # application=ssh:github: own slot, so an existing resident "ssh:" key is never overwritten.
    $common = @('-O', 'resident', '-O', 'verify-required', '-O', "application=$Application", '-C', "$email (YubiKey)", '-f', $KeyPath)
    & $SshKeygen -t ed25519-sk @common
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $PubPath)) {
        Write-Host '    ed25519-sk 失敗（舊韌體不支援），改用 ecdsa-sk。' -ForegroundColor Yellow
        & $SshKeygen -t ecdsa-sk @common
        if ($LASTEXITCODE -ne 0 -or -not (Test-Path $PubPath)) {
            Fail "無法在 YubiKey 上建立金鑰。若是因為 $Application 槽位已有金鑰，請改用：`n        powershell -ExecutionPolicy Bypass -File scripts\setup-yubikey.ps1 -Application ssh:cbxshell -KeyName id_ed25519_sk_cbxshell`n    否則請確認已設定 FIDO2 PIN（Yubico Authenticator > Passkeys）。"
        }
    }
}
$pub = (Get-Content -LiteralPath $PubPath -Raw).Trim()
if ($pub -notmatch '^sk-(ssh-ed25519|ecdsa-sha2-nistp256)@openssh\.com ') {
    Fail "$PubPath 不是 YubiKey（-sk）金鑰，為避免誤用軟體金鑰而停止。"
}

# ---------------------------------------------------------------------------------------
Step '允許的簽章者清單（git 用它驗證簽章，pre-push 也靠它）'
$allowed = Join-Path $SshDir 'allowed_signers'
$keyBody = ($pub -split ' ')[0..1] -join ' '
$line = "$email namespaces=`"git`" $keyBody"
$existing = if (Test-Path $allowed) { Get-Content -LiteralPath $allowed } else { @() }
if ($existing -notcontains $line) {
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    [IO.File]::AppendAllText($allowed, "$line`n", $utf8)
    Write-Host "    已加入 $allowed"
} else {
    Write-Host '    已存在'
}

# ---------------------------------------------------------------------------------------
Step '發行核准金鑰（assets\release-approvers.pub，編譯進更新器）'
if ($pub -notmatch '^sk-ssh-ed25519@openssh\.com ') {
    Write-Host '    這是 ecdsa-sk 金鑰；CBX Shell 更新器只接受 sk-ssh-ed25519，略過。' -ForegroundColor Yellow
    Write-Host '    （YubiKey 韌體 5.2.3 以上可建立 ed25519-sk，請用 -KeyName 指定新檔名重跑。）' -ForegroundColor Yellow
} else {
    $approvers = Join-Path $Root 'assets\release-approvers.pub'
    # No e-mail in the committed file: the key alone identifies the approver.
    $approverLine = "$keyBody cbxshell-release-approver (YubiKey)"
    $current = if (Test-Path -LiteralPath $approvers) { Get-Content -LiteralPath $approvers } else { @() }
    if (@($current | Where-Object { $_ -like "$keyBody*" }).Count -eq 0) {
        $utf8 = New-Object System.Text.UTF8Encoding($false)
        [IO.File]::AppendAllText($approvers, "$approverLine`n", $utf8)
        Write-Host "    已加入 $approvers（請提交這個檔案）" -ForegroundColor Green
    } else {
        Write-Host '    已存在'
    }
}

# ---------------------------------------------------------------------------------------
$scope = if ($Global) { '--global' } else { '--local' }
Step "git 簽章與 SSH 設定（$scope）"
Git-Config $scope 'gpg.format' 'ssh'
Git-Config $scope 'gpg.ssh.program' $SshKeygenFwd
Git-Config $scope 'user.signingkey' $KeyFwd
Git-Config $scope 'gpg.ssh.allowedSignersFile' ($allowed -replace '\\', '/')
Git-Config $scope 'commit.gpgsign' 'true'
Git-Config $scope 'tag.gpgsign' 'true'
# ssh expands ~ itself, which keeps user names with spaces out of the shell command line.
Git-Config $scope 'core.sshCommand' "$SshFwd -o IdentitiesOnly=yes -o IdentityFile=$KeyFwd"

Step 'pre-push 檢查（只對這個 repo）'
Git-Config '--local' 'core.hooksPath' '.githooks'

if (-not $KeepRemote) {
    Step 'origin 改用 SSH'
    $url = (& git -C $Root remote get-url origin) -join ''
    if ($url -match '^https://github\.com/([^/]+)/([^/]+?)(\.git)?/?$') {
        $sshUrl = "git@github.com:$($Matches[1])/$($Matches[2]).git"
        & git -C $Root remote set-url origin $sshUrl
        Write-Host "    $url  →  $sshUrl"
    } else {
        Write-Host "    origin 是 $url，不更動。"
    }
}

# ---------------------------------------------------------------------------------------
Set-Clipboard -Value $pub
Write-Host "`n==================================================================" -ForegroundColor Green
Write-Host ' 公鑰已複製到剪貼簿。請到 GitHub 加入「兩次」：' -ForegroundColor Green
Write-Host '   https://github.com/settings/ssh/new'
Write-Host '   1) Key type = Authentication Key （push 用）'
Write-Host '   2) Key type = Signing Key        （commit / tag 顯示 Verified 用）'
Write-Host "`n 加好之後測試（各需 PIN + 觸碰一次）："
Write-Host "   & `"$Ssh`" -o IdentitiesOnly=yes -i `"$KeyPath`" -T git@github.com"
Write-Host '   git commit --allow-empty -m "test: signed commit"   # 之後用 git log --show-signature -1 檢查'
Write-Host "==================================================================`n" -ForegroundColor Green
