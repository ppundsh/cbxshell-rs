#Requires -Version 5.1
<#
.SYNOPSIS
    沿用 YubiKey（OpenPGP）上既有的 GnuPG 金鑰：commit / tag 簽章、pre-push 檢查與發行核准。

.DESCRIPTION
    適用於已用 gpg --edit-key / keytocard 把金鑰放進 YubiKey 的情況（不會建立或修改任何金鑰）。
      1. 找到 gpg.exe，確認 YubiKey 已插入（gpg --card-status）
      2. 找出存放在 YubiKey 上、可簽章的金鑰（或使用 -KeyId 指定）
      3. 設定 git：gpg.format=openpgp、gpg.program、user.signingkey、commit / tag 一律簽章
      4. 啟用 .githooks/pre-push（未用這把金鑰簽章的 commit / tag 拒絕推送）
      5. 把金鑰的公開金鑰值加入 assets\release-approvers.pub（gpg --export-ssh-key，不含 email），
         之後建置的 CBX Shell 只接受附有這把金鑰核准簽章（scripts\approve-release.ps1）的更新
      6. 實際簽一次測試資料（需要 YubiKey PIN；若設了觸碰原則還要觸碰）

    預設只設定這個 repo；加 -Global 則簽章設定寫入全域（所有 repo）。

.PARAMETER KeyId
    要使用的金鑰（長 Key ID 或指紋）。省略時自動找 YubiKey 上可簽章的金鑰。

.PARAMETER Global
    簽章設定寫入全域設定。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\setup-gpg-signing.ps1
.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\setup-gpg-signing.ps1 -KeyId 411F3603EC032043
#>
param(
    [string]$KeyId = '',
    [switch]$Global
)

$ErrorActionPreference = 'Continue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)

function Fail([string]$msg) {
    Write-Host "`n[錯誤] $msg" -ForegroundColor Red
    exit 1
}
function Step([string]$msg) { Write-Host "`n==> $msg" -ForegroundColor Cyan }
function Git-Config([string]$scope, [string]$key, [string]$value) {
    & git -C $Root config $scope $key $value
    if ($LASTEXITCODE -ne 0) { Fail "git config $scope $key 失敗" }
    Write-Host "    git config $scope $key = $value" -ForegroundColor DarkGray
}

# ---------------------------------------------------------------------------------------
Step '尋找 GnuPG'
$candidates = @(
    "${env:ProgramFiles(x86)}\GnuPG\bin\gpg.exe",
    "$env:ProgramFiles\GnuPG\bin\gpg.exe",
    "${env:ProgramFiles(x86)}\Gpg4win\..\GnuPG\bin\gpg.exe"
)
$Gpg = $candidates | Where-Object { $_ -and (Test-Path -LiteralPath $_) } | Select-Object -First 1
if (-not $Gpg) {
    $cmd = Get-Command gpg.exe -ErrorAction SilentlyContinue
    if ($cmd -and $cmd.Source -notlike '*\Git\usr\bin\*') { $Gpg = $cmd.Source }
}
if (-not $Gpg) { Fail '找不到 GnuPG（winget install GnuPG.Gpg4win）。不要用 Git 內附的 gpg，它讀不到 Gpg4win 的金鑰。' }
$Gpg = (Resolve-Path -LiteralPath $Gpg).Path
Write-Host "    $Gpg"
& git -C $Root rev-parse --is-inside-work-tree *> $null
if ($LASTEXITCODE -ne 0) { Fail "$Root 不是 git repo" }

# ---------------------------------------------------------------------------------------
Step '確認 YubiKey'
$Agent = Join-Path (Split-Path -Parent $Gpg) 'gpg-connect-agent.exe'
if (Test-Path -LiteralPath $Agent) {
    $cardList = @(& $Agent 'scd getinfo card_list' /bye 2>$null | Where-Object { $_ -match '^S SERIALNO ' })
    if ($cardList.Count -gt 1) {
        Fail "目前插了 $($cardList.Count) 支 YubiKey。GnuPG 一次只會用其中一支，請只留一支再執行（另一支之後換上去一樣能用）。"
    }
}
$cardColons = & $Gpg --card-status --with-colons 2>&1
if ($LASTEXITCODE -ne 0) { Fail "讀不到 YubiKey 的 OpenPGP 卡片，請插入 YubiKey 後重試。`n$($cardColons | Out-String)" }
$serial = ''
$cardSigFpr = ''
foreach ($line in $cardColons) {
    $f = "$line" -split ':'
    if ($f[0] -eq 'serial') { $serial = $f[1] }
    if ($f[0] -eq 'fpr' -and $f.Count -gt 1) { $cardSigFpr = $f[1].ToUpperInvariant() }
}
Write-Host "    YubiKey 序號 $serial"
if (-not $cardSigFpr) { Fail '這支 YubiKey 的簽章槽（Signature key）是空的。' }
Write-Host "    簽章槽金鑰指紋 $cardSigFpr"

# ---------------------------------------------------------------------------------------
Step '找出 YubiKey 上可簽章的金鑰'
function Load-Keys {
    $colons = & $Gpg --list-secret-keys --with-colons --fixed-list-mode 2>$null
    $list = @()
    $primaryFpr = ''
    $pendingKey = $null
    foreach ($line in $colons) {
        $f = $line -split ':'
        switch ($f[0]) {
            'sec' { $pendingKey = @{ kind = 'sec'; keyid = $f[4]; caps = $f[11]; card = $f[14]; algo = $f[3]; primary = '' } }
            'ssb' { $pendingKey = @{ kind = 'ssb'; keyid = $f[4]; caps = $f[11]; card = $f[14]; algo = $f[3]; primary = $primaryFpr } }
            'fpr' {
                if ($pendingKey) {
                    $pendingKey.fpr = $f[9]
                    if ($pendingKey.kind -eq 'sec') { $primaryFpr = $f[9]; $pendingKey.primary = $f[9] }
                    $list += , $pendingKey
                    $pendingKey = $null
                }
            }
        }
    }
    return , $list
}
function Find-Key($list, [string]$id) {
    $want = $id.Trim().TrimEnd('!').ToUpperInvariant() -replace '^0X', ''
    return $list | Where-Object { $_.fpr.EndsWith($want) -or $_.keyid -eq $want } | Select-Object -First 1
}

$keys = Load-Keys
if (-not (Find-Key $keys $cardSigFpr)) {
    # This PC's GnuPG does not know the card yet: it needs the public key and a "learn"
    # so the secret-key stubs point at the YubiKey.
    & $Gpg --list-keys $cardSigFpr *> $null
    if ($LASTEXITCODE -ne 0) {
        Write-Host '    這台電腦沒有這把金鑰的公鑰，從 GitHub 匯入（https://github.com/ppundsh.gpg）…' -ForegroundColor Yellow
        $pubFile = Join-Path $env:TEMP 'cbxshell-owner.gpg'
        try {
            Invoke-WebRequest -UseBasicParsing 'https://github.com/ppundsh.gpg' -OutFile $pubFile
            & $Gpg --batch --import $pubFile
        } catch {
            Write-Host "    下載失敗：$($_.Exception.Message)" -ForegroundColor Yellow
        }
        Remove-Item -LiteralPath $pubFile -ErrorAction SilentlyContinue
    }
    Write-Host '    重新關聯 YubiKey（learn --force）…' -ForegroundColor Yellow
    & $Agent 'scd serialno' 'learn --force' /bye | Out-Null
    $keys = Load-Keys
}
$cardKey = Find-Key $keys $cardSigFpr
if (-not $cardKey) {
    Fail ("GnuPG 仍找不到 YubiKey 上的金鑰 $cardSigFpr。請手動執行：`n" +
        "        gpg --card-edit     （輸入 fetch 後 quit，從卡片記錄的網址匯入公鑰）`n" +
        "        gpg-connect-agent `"scd serialno`" `"learn --force`" /bye`n" +
        "        gpg --list-secret-keys --keyid-format=long   （應出現 sec> 或 ssb>）")
}

if ($KeyId) {
    $byId = Find-Key $keys $KeyId
    if (-not $byId) { Fail "找不到金鑰 $KeyId（gpg --list-secret-keys --keyid-format=long）" }
    if ($byId.primary -eq $cardKey.primary) {
        # Same identity: sign with the (sub)key that actually sits in the card's signature slot.
        $sel = $cardKey
    } else {
        Fail "插著的 YubiKey 簽章槽是 $cardSigFpr，不屬於 $KeyId。請換插另一支 YubiKey，或省略 -KeyId。"
    }
} else {
    $sel = $cardKey
}
if (-not ($sel.caps -cmatch 's')) { Fail "$($sel.keyid) 不能簽章（能力：$($sel.caps)）" }
if ($sel.algo -ne '22') {
    Fail "金鑰演算法代碼 $($sel.algo) 不是 Ed25519（22）；發行核准只支援 Ed25519（Curve 25519）。"
}
$SigningKey = $sel.keyid

# Your own key must be "ultimate" on this PC, otherwise git reports signatures as untrusted
# (%G? = U) and the pre-push hook refuses to push.
$pubLine = (& $Gpg --list-keys --with-colons $sel.primary 2>$null) | Where-Object { $_ -like 'pub:*' } | Select-Object -First 1
if ($pubLine -and (($pubLine -split ':')[8] -ne 'u')) {
    $ot = Join-Path $env:TEMP ('cbx-ownertrust-' + [guid]::NewGuid().ToString('N') + '.txt')
    Set-Content -LiteralPath $ot -Value "$($sel.primary):6:" -Encoding ascii
    & $Gpg --import-ownertrust $ot 2>$null
    Remove-Item -LiteralPath $ot -ErrorAction SilentlyContinue
    Write-Host '    已將這把金鑰設為完全信任（ultimate）' -ForegroundColor Green
}
Write-Host "    使用 $($sel.kind) $SigningKey（指紋 $($sel.fpr)，主金鑰 $($sel.primary)）" -ForegroundColor Green

# ---------------------------------------------------------------------------------------
$scope = if ($Global) { '--global' } else { '--local' }
Step "設定 git 簽章（$scope）"
$GpgFwd = $Gpg -replace '\\', '/'
Git-Config $scope 'gpg.format' 'openpgp'
Git-Config $scope 'gpg.program' $GpgFwd
# "!" = sign with exactly this (sub)key instead of letting GnuPG pick one.
Git-Config $scope 'user.signingkey' "$SigningKey!"
Git-Config $scope 'commit.gpgsign' 'true'
Git-Config $scope 'tag.gpgSign' 'true'
Git-Config '--local' 'core.hooksPath' '.githooks'

$email = ((& git -C $Root config user.email) -join '').Trim()
$uids = (& $Gpg --list-keys --with-colons $sel.primary 2>$null) | Where-Object { $_ -like 'uid:*' } |
    ForEach-Object { ($_ -split ':')[9] }
if ($email -and -not ($uids | Where-Object { $_ -like "*<$email>*" })) {
    Write-Host '    注意：git 的 user.email 不在這把金鑰的使用者 ID 裡，GitHub 會把簽章標成 Unverified。' -ForegroundColor Yellow
    Write-Host '          把 user.email 改成金鑰裡、且已在 GitHub 驗證過的 email（gpg --edit-key → adduid 可新增）。' -ForegroundColor Yellow
}

# ---------------------------------------------------------------------------------------
Step '發行核准金鑰（assets\release-approvers.pub，編譯進更新器）'
$ssh = ((& $Gpg --export-ssh-key "$($sel.fpr)!" 2>$null) -join '').Trim()
if ($ssh -notmatch '^ssh-ed25519 \S+') { Fail "gpg --export-ssh-key 失敗（需要 GnuPG 2.2 以上）：$ssh" }
$parts = $ssh -split '\s+'
# Key material + "openpgp:0x…" only — no user ID / e-mail goes into the repository.
$approverLine = "openpgp $($parts[0]) $($parts[1]) openpgp:0x$SigningKey (YubiKey OpenPGP)"
$approvers = Join-Path $Root 'assets\release-approvers.pub'
$current = if (Test-Path -LiteralPath $approvers) { Get-Content -LiteralPath $approvers } else { @() }
if (@($current | Where-Object { $_ -like "openpgp $($parts[0]) $($parts[1])*" }).Count -eq 0) {
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    [IO.File]::AppendAllText($approvers, "$approverLine`n", $utf8)
    Write-Host "    已加入 $approvers（請提交這個檔案）" -ForegroundColor Green
} else {
    Write-Host '    已存在'
}

# ---------------------------------------------------------------------------------------
Step '測試簽章（請輸入 YubiKey 的 OpenPGP PIN；若有設觸碰原則請觸碰）'
$tmp = Join-Path $env:TEMP ('cbx-gpg-test-' + [guid]::NewGuid().ToString('N') + '.txt')
[IO.File]::WriteAllText($tmp, "cbxshell signing test`n")
& $Gpg --batch --yes --armor --detach-sign --local-user "$($sel.fpr)!" -o "$tmp.asc" $tmp
$signed = ($LASTEXITCODE -eq 0)
if ($signed) { & $Gpg --verify "$tmp.asc" $tmp 2>&1 | Out-Null; $signed = ($LASTEXITCODE -eq 0) }
Remove-Item -LiteralPath $tmp, "$tmp.asc" -ErrorAction SilentlyContinue
if (-not $signed) { Fail '測試簽章失敗。確認 YubiKey 已插入、PIN 正確，或執行 gpg --card-status 檢查。' }
Write-Host '    簽章與驗證成功' -ForegroundColor Green

$ykman = Get-Command ykman -ErrorAction SilentlyContinue
Write-Host ''
Write-Host '建議：讓每次簽章都必須觸碰 YubiKey（防止電腦被入侵時在背景偷簽）：' -ForegroundColor Yellow
if ($ykman) {
    Write-Host '    ykman openpgp keys set-touch sig on' -ForegroundColor Yellow
} else {
    Write-Host '    安裝 YubiKey Manager 後執行：ykman openpgp keys set-touch sig on' -ForegroundColor Yellow
}
Write-Host "`n完成。之後 commit / tag 會自動以 $SigningKey 簽章；發行時執行 scripts\approve-release.ps1。" -ForegroundColor Green
